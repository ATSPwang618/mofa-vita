use tjs_core::{Diagnostic, FunctionId, Instruction, Phase, Register, Span};

use super::{Compiler, places::RawAccess};
use crate::ast::{BinaryOp, ExprId, ExprKind, MemberName, UnaryOp};

impl Compiler<'_> {
    pub(super) fn condition_value(&mut self, id: ExprId, dst: Register) -> Result<(), Diagnostic> {
        let expression = *self.program.expression(id);
        match expression.kind {
            ExprKind::Sequence { lhs, rhs } => {
                self.discard(lhs)?;
                self.condition_value(rhs, dst)
            }
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => self.conditional_value(
                condition,
                then_value,
                else_value,
                dst,
                true,
                expression.span,
            ),
            ExprKind::Not(inner) => {
                self.condition_value(inner, dst)?;
                self.emit(Instruction::Not { dst, src: dst }, expression.span)
            }
            ExprKind::Binary { op, lhs, rhs }
                if matches!(
                    op,
                    BinaryOp::Equal
                        | BinaryOp::StrictEqual
                        | BinaryOp::StrictNotEqual
                        | BinaryOp::NotEqual
                        | BinaryOp::Less
                        | BinaryOp::LessEqual
                        | BinaryOp::Greater
                        | BinaryOp::GreaterEqual
                ) =>
            {
                // TJS's condition-flag path reads local operands after RHS effects.
                // Ordinary value-producing comparisons still snapshot the LHS.
                let left = self.operand(lhs)?;
                let right = self.operand(rhs)?;
                self.emit(binary_instruction(op, dst, left, right), expression.span)
            }
            _ => self.expression(id, dst),
        }
    }

    pub(super) fn operand(&mut self, id: ExprId) -> Result<Register, Diagnostic> {
        let expression = *self.program.expression(id);
        match expression.kind {
            ExprKind::WithObject => {
                if let Some(&register) = self.with_objects.last() {
                    return Ok(register);
                }
            }
            ExprKind::Sequence { lhs, rhs } => {
                self.discard(lhs)?;
                return self.operand(rhs);
            }
            ExprKind::Name(name) => {
                if let Some(local) = self.lookup(name)? {
                    return Ok(local);
                }
                // Global/this property reads snapshot the value when evaluated.
            }
            ExprKind::Assign { target, value } => {
                let source = self.operand(value)?;
                self.store_target(target, source, expression.span)?;
                return Ok(source);
            }
            ExprKind::CompoundAssign { target, op, value } => {
                let right = self.operand(value)?;
                return self.modify(
                    target,
                    super::places::update_operation(op),
                    right,
                    false,
                    expression.span,
                );
            }
            ExprKind::Update {
                target,
                increment,
                postfix,
            } => {
                let right = self.temporary(expression.span)?;
                // Numeric update must not inherit string concatenation from Add.
                self.emit(
                    Instruction::LoadInt {
                        dst: right,
                        value: if increment { 1 } else { -1 },
                    },
                    expression.span,
                )?;
                return self.modify(
                    target,
                    if increment {
                        tjs_core::value::UpdateOp::Increment
                    } else {
                        tjs_core::value::UpdateOp::Decrement
                    },
                    right,
                    postfix,
                    expression.span,
                );
            }
            _ => {}
        }
        let temporary = self.temporary(expression.span)?;
        self.expression(id, temporary)?;
        Ok(temporary)
    }

    pub(super) fn expression(&mut self, id: ExprId, dst: Register) -> Result<(), Diagnostic> {
        let expression = *self.program.expression(id);
        let mark = self.next_temp;
        match expression.kind {
            ExprKind::Swap { .. } | ExprKind::PostfixIf { .. } => {
                return Err(Diagnostic::new(
                    Phase::Compile,
                    expression.span,
                    "swap and postfix if do not produce a value; use them as statements",
                ));
            }
            ExprKind::Sequence { lhs, rhs } => {
                self.discard(lhs)?;
                self.expression(rhs, dst)?;
            }
            ExprKind::Logical { and, lhs, rhs } => {
                self.logical_value(and, lhs, rhs, dst, expression.span)?;
            }
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                self.conditional_value(
                    condition,
                    then_value,
                    else_value,
                    dst,
                    false,
                    expression.span,
                )?;
            }
            ExprKind::Super => {
                let base = self.super_expression.ok_or_else(|| {
                    Diagnostic::new(
                        Phase::Compile,
                        expression.span,
                        "super requires a method or accessor of a class with one base",
                    )
                })?;
                let previous = self.global_context;
                self.global_context = true;
                self.expression(base, dst)?;
                self.global_context = previous;
            }
            ExprKind::Eval(inner) => {
                let src = self.operand(inner)?;
                self.emit(
                    Instruction::Eval {
                        dst: Some(dst),
                        src,
                    },
                    expression.span,
                )?;
            }
            ExprKind::This => self.emit(Instruction::LoadThis { dst }, expression.span)?,
            ExprKind::Function(function) => self.emit(
                Instruction::LoadFunction {
                    dst,
                    function: FunctionId(function.0 as u32 + 1),
                },
                expression.span,
            )?,
            ExprKind::Global => self.emit(Instruction::LoadGlobal { dst }, expression.span)?,
            ExprKind::WithObject => {
                let instruction = if let Some(&src) = self.with_objects.last() {
                    Instruction::Move { dst, src }
                } else {
                    Instruction::LoadGlobal { dst }
                };
                self.emit(instruction, expression.span)?;
            }
            ExprKind::InContextOf { object, context } => {
                let object = self.operand(object)?;
                let context = self.operand(context)?;
                self.emit(
                    Instruction::BindContext {
                        dst,
                        object,
                        context,
                    },
                    expression.span,
                )?;
            }
            ExprKind::Array { start, end } => {
                let array = self.temporary(expression.span)?;
                self.emit(Instruction::NewArray { dst: array }, expression.span)?;
                let element_mark = self.next_temp;
                for index in start..end {
                    let value = self.operand(self.program.array_elements[index])?;
                    self.emit(Instruction::ArrayPush { array, value }, expression.span)?;
                    self.next_temp = element_mark;
                }
                self.emit(Instruction::Move { dst, src: array }, expression.span)?;
            }
            ExprKind::Dictionary { start, end } => {
                // Construct in a temporary: an initializer must not expose the
                // destination binding before all entries have been evaluated.
                let object = self.temporary(expression.span)?;
                self.emit(Instruction::NewDictionary { dst: object }, expression.span)?;
                let entry_mark = self.next_temp;
                for index in start..end {
                    let (key, value) = self.program.dictionary_entries[index];
                    // Like the reference's T_DICELM, local operands remain live
                    // addresses until both expressions have finished.
                    let key = self.operand(key)?;
                    let value = self.operand(value)?;
                    self.emit(
                        Instruction::SetMember { object, key, value },
                        expression.span,
                    )?;
                    self.next_temp = entry_mark;
                }
                self.emit(Instruction::Move { dst, src: object }, expression.span)?;
            }
            ExprKind::Member { object, name } => {
                let (object, key) = self.member_operands(object, name)?;
                self.emit(Instruction::GetMember { dst, object, key }, expression.span)?;
            }
            ExprKind::RawProperty(inner) => {
                self.raw_property(inner, RawAccess::Read(dst), expression.span)?
            }
            ExprKind::Dereference(inner) => {
                let src = self.operand(inner)?;
                self.emit(Instruction::GetProperty { dst, src }, expression.span)?;
            }
            ExprKind::TypeOf(inner) => {
                if let ExprKind::Member { object, name } = self.program.expression(inner).kind {
                    let computed = matches!(name, MemberName::Computed(_));
                    let (object, key) = self.member_operands(object, name)?;
                    self.emit(
                        Instruction::TypeOfMember {
                            dst,
                            object,
                            key,
                            computed,
                        },
                        expression.span,
                    )?;
                } else {
                    self.expression(inner, dst)?;
                    self.emit(Instruction::TypeOf { dst, src: dst }, expression.span)?;
                }
            }
            ExprKind::Delete(target) => {
                self.delete_target(target, dst)?;
            }
            ExprKind::Call { .. } | ExprKind::Construct { .. } => {
                self.call_expression(id, Some(dst))?
            }
            ExprKind::Integer(value) => {
                self.emit(Instruction::LoadInt { dst, value }, expression.span)?
            }
            ExprKind::Real(bits) => {
                self.emit(Instruction::LoadReal { dst, bits }, expression.span)?
            }
            ExprKind::Unary { op, inner } => {
                self.expression(inner, dst)?;
                let instruction = match op {
                    UnaryOp::CharacterCode => Instruction::CharacterCode { dst, src: dst },
                    UnaryOp::CharacterFrom => Instruction::CharacterFrom { dst, src: dst },
                    UnaryOp::IsValid => Instruction::IsValid { dst, src: dst },
                    UnaryOp::Invalidate => Instruction::Invalidate { dst, src: dst },
                    UnaryOp::Number => Instruction::ToNumber { dst, src: dst },
                    UnaryOp::Integer => Instruction::ToInteger { dst, src: dst },
                    UnaryOp::Real => Instruction::ToReal { dst, src: dst },
                    UnaryOp::String => Instruction::ToString { dst, src: dst },
                    UnaryOp::BitNot => Instruction::BitNot { dst, src: dst },
                };
                self.emit(instruction, expression.span)?;
            }
            ExprKind::ConstantArray { .. } | ExprKind::ConstantDictionary { .. } => {
                let constant = self.constants.containers[&id.0];
                self.emit(Instruction::LoadConstant { dst, constant }, expression.span)?;
            }
            ExprKind::Void => self.emit(Instruction::LoadVoid { dst }, expression.span)?,
            ExprKind::Null => self.emit(Instruction::LoadNull { dst }, expression.span)?,
            ExprKind::Octet(index) => {
                let constant = self.program.strings.len() as u32 + index;
                self.emit(Instruction::LoadConstant { dst, constant }, expression.span)?;
            }
            ExprKind::RegExp(index) => self.regexp(index, dst, expression.span)?,
            ExprKind::String(constant) => {
                self.emit(Instruction::LoadConstant { dst, constant }, expression.span)?
            }
            ExprKind::NameString(name) => self.load_name(name, dst)?,
            ExprKind::Name(name) => {
                if let Some(src) = self.lookup(name)? {
                    if src != dst {
                        self.emit(Instruction::Move { dst, src }, expression.span)?;
                    }
                } else {
                    let key = self.temporary(name)?;
                    self.load_name(name, key)?;
                    if self.global_context {
                        let object = self.temporary(name)?;
                        self.emit(Instruction::LoadGlobal { dst: object }, name)?;
                        self.emit(Instruction::GetMember { dst, object, key }, expression.span)?;
                    } else {
                        self.emit(Instruction::GetName { dst, key }, expression.span)?;
                    }
                }
            }
            ExprKind::Negate(inner) => {
                self.expression(inner, dst)?;
                self.emit(Instruction::Negate { dst, src: dst }, expression.span)?;
            }
            ExprKind::Not(inner) => {
                self.expression(inner, dst)?;
                self.emit(Instruction::Not { dst, src: dst }, expression.span)?;
            }
            ExprKind::Binary { op, lhs, rhs } => {
                // Snapshot the left operand before evaluating a possibly mutating RHS.
                let left = self.temporary(expression.span)?;
                self.expression(lhs, left)?;
                let right = self.temporary(expression.span)?;
                self.expression(rhs, right)?;
                let instruction = binary_instruction(op, dst, left, right);
                self.emit(instruction, expression.span)?;
            }
            ExprKind::Assign { target, value } => {
                // TJS evaluates the RHS before the receiver and key. A local RHS
                // may be changed by those evaluations before SetMember reads it.
                let source = self.operand(value)?;
                self.store_target(target, source, expression.span)?;
                if source != dst {
                    self.emit(Instruction::Move { dst, src: source }, expression.span)?;
                }
            }
            ExprKind::CompoundAssign { .. } | ExprKind::Update { .. } => {
                let source = self.operand(id)?;
                if source != dst {
                    self.emit(Instruction::Move { dst, src: source }, expression.span)?;
                }
            }
        }
        self.next_temp = mark;
        Ok(())
    }

    pub(super) fn load_name(&mut self, name: Span, dst: Register) -> Result<(), Diagnostic> {
        let units = self.sources.slice(name).ok_or_else(|| {
            Diagnostic::new(Phase::Compile, name, "source handle is no longer valid")
        })?;
        let constant = self.constants.name(units);
        self.emit(Instruction::LoadConstant { dst, constant }, name)
    }

    fn logical_value(
        &mut self,
        and: bool,
        lhs: ExprId,
        rhs: ExprId,
        dst: Register,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let mark = self.next_temp;
        let branch = self.condition_jump(lhs)?;
        if and {
            self.condition_value(rhs, dst)?;
            self.emit(Instruction::Not { dst, src: dst }, span)?;
            self.emit(Instruction::Not { dst, src: dst }, span)?;
        } else {
            self.emit(Instruction::LoadInt { dst, value: 1 }, span)?;
        }
        let end = self.jump(span)?;
        self.patch_here(branch);
        self.next_temp = mark;
        if and {
            self.emit(Instruction::LoadInt { dst, value: 0 }, span)?;
        } else {
            self.condition_value(rhs, dst)?;
            self.emit(Instruction::Not { dst, src: dst }, span)?;
            self.emit(Instruction::Not { dst, src: dst }, span)?;
        }
        self.patch_here(end);
        self.next_temp = mark;
        Ok(())
    }

    fn conditional_value(
        &mut self,
        condition: ExprId,
        then_value: ExprId,
        else_value: ExprId,
        dst: Register,
        flag: bool,
        span: Span,
    ) -> Result<(), Diagnostic> {
        let mark = self.next_temp;
        let branch = self.condition_jump(condition)?;
        if flag {
            self.condition_value(then_value, dst)?;
        } else {
            self.expression(then_value, dst)?;
        }
        let end = self.jump(span)?;
        self.patch_here(branch);
        self.next_temp = mark;
        if flag {
            self.condition_value(else_value, dst)?;
        } else {
            self.expression(else_value, dst)?;
        }
        self.patch_here(end);
        self.next_temp = mark;
        Ok(())
    }
}

pub(super) fn binary_instruction(
    op: BinaryOp,
    dst: Register,
    left: Register,
    right: Register,
) -> Instruction {
    match op {
        BinaryOp::LogicalAnd => Instruction::LogicalAnd {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::LogicalOr => Instruction::LogicalOr {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::In => Instruction::ContainsMember {
            dst,
            object: right,
            key: left,
        },
        BinaryOp::InstanceOf => Instruction::InstanceOf {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Divide => Instruction::Divide {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::IntDivide => Instruction::IntDivide {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Remainder => Instruction::Remainder {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::BitAnd => Instruction::BitAnd {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::BitOr => Instruction::BitOr {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::BitXor => Instruction::BitXor {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::ShiftLeft => Instruction::ShiftLeft {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::ShiftRight => Instruction::ShiftRight {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::ShiftRightUnsigned => Instruction::ShiftRightUnsigned {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::StrictEqual => Instruction::StrictEqual {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::StrictNotEqual => Instruction::StrictNotEqual {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Add => Instruction::Add {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Subtract => Instruction::Subtract {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Multiply => Instruction::Multiply {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Equal => Instruction::Equal {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::NotEqual => Instruction::NotEqual {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Less => Instruction::Less {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::LessEqual => Instruction::LessEqual {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::Greater => Instruction::Greater {
            dst,
            lhs: left,
            rhs: right,
        },
        BinaryOp::GreaterEqual => Instruction::GreaterEqual {
            dst,
            lhs: left,
            rhs: right,
        },
    }
}

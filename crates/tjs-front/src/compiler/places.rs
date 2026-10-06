//! Assignment targets and property operators, shared by reads and updates.
use super::{Compiler, expressions::binary_instruction};
use crate::ast::{BinaryOp, ExprId, ExprKind, MemberName};
use tjs_core::{Diagnostic, Instruction, Phase, Register, Span, value::UpdateOp};

#[derive(Clone, Copy)]
enum Place {
    Local(Register),
    Name(Register),
    Member { object: Register, key: Register },
    Property(Register),
}

#[derive(Clone, Copy)]
pub(super) enum RawAccess {
    Read(Register),
    Write(Register),
}

impl Compiler<'_> {
    pub(super) fn delete_target(&mut self, id: ExprId, dst: Register) -> Result<(), Diagnostic> {
        let expression = *self.program.expression(id);
        match expression.kind {
            ExprKind::Name(name) => {
                if !self.global_context {
                    let units = self.name(name)?.to_vec();
                    if let Some(binding) = self.scopes.iter_mut().rev().find_map(|scope| {
                        scope
                            .bindings
                            .get_mut(&units)
                            .filter(|binding| binding.active)
                    }) {
                        // TJS removes local names at compile time, including in
                        // branches that never execute. The old value is not cleared.
                        binding.active = false;
                        return self.emit(Instruction::LoadInt { dst, value: 1 }, expression.span);
                    }
                }
                let key = self.temporary(name)?;
                self.load_name(name, key)?;
                if self.global_context {
                    let object = self.temporary(name)?;
                    self.emit(Instruction::LoadGlobal { dst: object }, name)?;
                    self.emit(
                        Instruction::DeleteMember { dst, object, key },
                        expression.span,
                    )
                } else {
                    self.emit(Instruction::DeleteName { dst, key }, expression.span)
                }
            }
            ExprKind::Member { object, name } => {
                let (object, key) = self.member_operands(object, name)?;
                self.emit(
                    Instruction::DeleteMember { dst, object, key },
                    expression.span,
                )
            }
            ExprKind::Sequence { lhs, rhs } => {
                self.discard(lhs)?;
                self.delete_target(rhs, dst)
            }
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                let mark = self.next_temp;
                let skip = self.condition_jump(condition)?;
                self.delete_target(then_value, dst)?;
                let end = self.jump(expression.span)?;
                self.patch_here(skip);
                self.next_temp = mark;
                self.delete_target(else_value, dst)?;
                self.patch_here(end);
                Ok(())
            }
            _ => Err(Diagnostic::new(
                Phase::Compile,
                expression.span,
                "delete requires a name or member",
            )),
        }
    }

    pub(super) fn modify(
        &mut self,
        target: ExprId,
        op: UpdateOp,
        right: Register,
        postfix: bool,
        span: Span,
    ) -> Result<Register, Diagnostic> {
        // RHS is evaluated first; receiver and computed key are evaluated once.
        if let ExprKind::Sequence { lhs, rhs } = self.program.expression(target).kind {
            self.discard(lhs)?;
            return self.modify(rhs, op, right, postfix, span);
        }
        if let ExprKind::Conditional {
            condition,
            then_value,
            else_value,
        } = self.program.expression(target).kind
        {
            let result = self.temporary(span)?;
            let mark = self.next_temp;
            let branch = self.condition_jump(condition)?;
            let src = self.modify(then_value, op, right, postfix, span)?;
            self.emit(Instruction::Move { dst: result, src }, span)?;
            let end = self.jump(span)?;
            self.patch_here(branch);
            self.next_temp = mark;
            let src = self.modify(else_value, op, right, postfix, span)?;
            self.emit(Instruction::Move { dst: result, src }, span)?;
            self.patch_here(end);
            self.next_temp = mark;
            return Ok(result);
        }
        let place = match self.program.expression(target).kind {
            ExprKind::Name(name) => match self.lookup(name)? {
                Some(local) => Place::Local(local),
                None => {
                    let key = self.temporary(name)?;
                    self.load_name(name, key)?;
                    if self.global_context {
                        let object = self.temporary(name)?;
                        self.emit(Instruction::LoadGlobal { dst: object }, name)?;
                        Place::Member { object, key }
                    } else {
                        Place::Name(key)
                    }
                }
            },
            ExprKind::Member { object, name } => {
                let (object, key) = self.member_operands(object, name)?;
                Place::Member { object, key }
            }
            ExprKind::Dereference(inner) => Place::Property(self.operand(inner)?),
            ExprKind::RawProperty(_) => {
                return Err(Diagnostic::new(
                    Phase::Compile,
                    span,
                    "raw properties support reading and assignment, not compound updates",
                ));
            }
            _ => unreachable!("parser checks update target"),
        };
        let current = match place {
            Place::Local(local) => local,
            _ => self.temporary(span)?,
        };
        let result = if postfix {
            let dst = self.temporary(span)?;
            self.read_place(place, dst, span)?;
            dst
        } else {
            current
        };
        match place {
            Place::Local(_) => {
                if op.unary() {
                    self.emit(
                        Instruction::ToNumber {
                            dst: current,
                            src: current,
                        },
                        span,
                    )?;
                    self.emit(
                        Instruction::Add {
                            dst: current,
                            lhs: current,
                            rhs: right,
                        },
                        span,
                    )?;
                } else {
                    let binary = match op {
                        UpdateOp::LogicalOr => BinaryOp::LogicalOr,
                        UpdateOp::LogicalAnd => BinaryOp::LogicalAnd,
                        UpdateOp::BitOr => BinaryOp::BitOr,
                        UpdateOp::BitXor => BinaryOp::BitXor,
                        UpdateOp::BitAnd => BinaryOp::BitAnd,
                        UpdateOp::ShiftRight => BinaryOp::ShiftRight,
                        UpdateOp::ShiftLeft => BinaryOp::ShiftLeft,
                        UpdateOp::ShiftRightUnsigned => BinaryOp::ShiftRightUnsigned,
                        UpdateOp::Add => BinaryOp::Add,
                        UpdateOp::Subtract => BinaryOp::Subtract,
                        UpdateOp::Remainder => BinaryOp::Remainder,
                        UpdateOp::Divide => BinaryOp::Divide,
                        UpdateOp::IntDivide => BinaryOp::IntDivide,
                        UpdateOp::Multiply => BinaryOp::Multiply,
                        _ => unreachable!("unary operation handled above"),
                    };
                    self.emit(binary_instruction(binary, current, current, right), span)?;
                }
            }
            _ => {
                if !op.unary() {
                    self.emit(
                        Instruction::Move {
                            dst: current,
                            src: right,
                        },
                        span,
                    )?;
                }
                // The VM retains the selected property across its getter/setter.
                // A consumed postfix result performs its independent read first.
                self.emit(
                    match place {
                        Place::Property(property) => Instruction::UpdateProperty {
                            property,
                            value: current,
                            op,
                        },
                        Place::Name(key) => Instruction::UpdateName {
                            key,
                            value: current,
                            op,
                        },
                        Place::Member { object, key } => Instruction::UpdateMember {
                            object,
                            key,
                            value: current,
                            op,
                        },
                        Place::Local(_) => unreachable!(),
                    },
                    span,
                )?;
            }
        }
        Ok(result)
    }

    pub(super) fn member_operands(
        &mut self,
        object: ExprId,
        name: MemberName,
    ) -> Result<(Register, Register), Diagnostic> {
        let object = self.operand(object)?;
        let key = match name {
            MemberName::Computed(key) => self.operand(key)?,
            MemberName::Named(name) => {
                let key = self.temporary(name)?;
                self.load_name(name, key)?;
                key
            }
        };
        Ok((object, key))
    }

    pub(super) fn store_target(
        &mut self,
        target: ExprId,
        value: Register,
        span: Span,
    ) -> Result<(), Diagnostic> {
        match self.program.expression(target).kind {
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                let mark = self.next_temp;
                let branch = self.condition_jump(condition)?;
                self.store_target(then_value, value, span)?;
                let end = self.jump(span)?;
                self.patch_here(branch);
                self.next_temp = mark;
                self.store_target(else_value, value, span)?;
                self.patch_here(end);
                self.next_temp = mark;
            }
            ExprKind::Sequence { lhs, rhs } => {
                self.discard(lhs)?;
                self.store_target(rhs, value, span)?;
            }
            ExprKind::Name(name) => {
                if let Some(dst) = self.lookup(name)? {
                    if dst != value {
                        self.emit(Instruction::Move { dst, src: value }, span)?;
                    }
                } else {
                    let key = self.temporary(name)?;
                    self.load_name(name, key)?;
                    if self.global_context {
                        let object = self.temporary(name)?;
                        self.emit(Instruction::LoadGlobal { dst: object }, name)?;
                        self.emit(Instruction::SetMember { object, key, value }, span)?;
                    } else {
                        self.emit(Instruction::SetName { key, value }, span)?;
                    }
                }
            }
            ExprKind::Member { object, name } => {
                let (object, key) = self.member_operands(object, name)?;
                self.emit(Instruction::SetMember { object, key, value }, span)?;
            }
            ExprKind::RawProperty(inner) => {
                self.raw_property(inner, RawAccess::Write(value), span)?
            }
            ExprKind::Dereference(inner) => {
                let property = self.operand(inner)?;
                self.emit(Instruction::SetProperty { property, value }, span)?;
            }
            _ => unreachable!("parser checks assignment target"),
        }
        Ok(())
    }
    fn read_place(&mut self, place: Place, dst: Register, span: Span) -> Result<(), Diagnostic> {
        self.emit(
            match place {
                Place::Local(src) => Instruction::Move { dst, src },
                Place::Name(key) => Instruction::GetName { dst, key },
                Place::Member { object, key } => Instruction::GetMember { dst, object, key },
                Place::Property(src) => Instruction::GetProperty { dst, src },
            },
            span,
        )
    }

    pub(super) fn raw_property(
        &mut self,
        target: ExprId,
        access: RawAccess,
        span: Span,
    ) -> Result<(), Diagnostic> {
        if let ExprKind::Sequence { lhs, rhs } = self.program.expression(target).kind {
            self.discard(lhs)?;
            return self.raw_property(rhs, access, span);
        }
        if let ExprKind::Conditional {
            condition,
            then_value,
            else_value,
        } = self.program.expression(target).kind
        {
            let mark = self.next_temp;
            let branch = self.condition_jump(condition)?;
            self.raw_property(then_value, access, span)?;
            let end = self.jump(span)?;
            self.patch_here(branch);
            self.next_temp = mark;
            self.raw_property(else_value, access, span)?;
            self.patch_here(end);
            self.next_temp = mark;
            return Ok(());
        }
        if let (ExprKind::InContextOf { object, context }, RawAccess::Read(dst)) =
            (self.program.expression(target).kind, access)
        {
            // T_INCONTEXTOF forwards ignore-property access to its left child;
            // the context expression still uses an ordinary read.
            self.raw_property(object, RawAccess::Read(dst), span)?;
            let context = self.operand(context)?;
            return self.emit(
                Instruction::BindContext {
                    dst,
                    object: dst,
                    context,
                },
                span,
            );
        }
        let (object, key) = match self.program.expression(target).kind {
            ExprKind::Member { object, name } => {
                let (object, key) = self.member_operands(object, name)?;
                (Some(object), key)
            }
            ExprKind::Name(name) if self.lookup(name)?.is_none() => {
                let key = self.temporary(name)?;
                self.load_name(name, key)?;
                let object = if self.global_context {
                    let object = self.temporary(name)?;
                    self.emit(Instruction::LoadGlobal { dst: object }, name)?;
                    Some(object)
                } else {
                    None
                };
                (object, key)
            }
            _ => {
                return Err(Diagnostic::new(
                    Phase::Compile,
                    span,
                    "& requires an object member or nonlocal name",
                ));
            }
        };
        let instruction = match (object, access) {
            (Some(object), RawAccess::Read(dst)) => Instruction::GetRawMember { dst, object, key },
            (None, RawAccess::Read(dst)) => Instruction::GetRawName { dst, key },
            (Some(object), RawAccess::Write(value)) => {
                Instruction::SetRawMember { object, key, value }
            }
            (None, RawAccess::Write(value)) => Instruction::SetRawName { key, value },
        };
        self.emit(instruction, span)
    }
}

pub(super) fn update_operation(op: BinaryOp) -> UpdateOp {
    match op {
        BinaryOp::LogicalOr => UpdateOp::LogicalOr,
        BinaryOp::LogicalAnd => UpdateOp::LogicalAnd,
        BinaryOp::BitOr => UpdateOp::BitOr,
        BinaryOp::BitXor => UpdateOp::BitXor,
        BinaryOp::BitAnd => UpdateOp::BitAnd,
        BinaryOp::ShiftRight => UpdateOp::ShiftRight,
        BinaryOp::ShiftLeft => UpdateOp::ShiftLeft,
        BinaryOp::ShiftRightUnsigned => UpdateOp::ShiftRightUnsigned,
        BinaryOp::Add => UpdateOp::Add,
        BinaryOp::Subtract => UpdateOp::Subtract,
        BinaryOp::Remainder => UpdateOp::Remainder,
        BinaryOp::Divide => UpdateOp::Divide,
        BinaryOp::IntDivide => UpdateOp::IntDivide,
        BinaryOp::Multiply => UpdateOp::Multiply,
        _ => unreachable!("parser restricts compound assignment operators"),
    }
}

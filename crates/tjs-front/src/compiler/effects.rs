//! Statement-only expressions and the CLI's optional completion values.
use super::Compiler;
use crate::ast::{ExprId, ExprKind};
use tjs_core::{Diagnostic, Instruction, Register};

impl Compiler<'_> {
    /// Top-level evaluation displays value-producing expressions. Statement-only
    /// branches produce void without making them usable in value contexts.
    pub(super) fn completion_value(&mut self, id: ExprId, dst: Register) -> Result<(), Diagnostic> {
        let expression = *self.program.expression(id);
        let mark = self.next_temp;
        match expression.kind {
            ExprKind::Swap { .. } | ExprKind::PostfixIf { .. } | ExprKind::Eval(_) => {
                self.discard(id)?;
                self.emit(Instruction::LoadVoid { dst }, expression.span)?;
            }
            ExprKind::Sequence { lhs, rhs } => {
                self.discard(lhs)?;
                self.completion_value(rhs, dst)?;
            }
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                let branch = self.condition_jump(condition)?;
                self.completion_value(then_value, dst)?;
                let end = self.jump(expression.span)?;
                self.patch_here(branch);
                self.next_temp = mark;
                self.completion_value(else_value, dst)?;
                self.patch_here(end);
            }
            _ => self.expression(id, dst)?,
        }
        self.next_temp = mark;
        Ok(())
    }

    pub(super) fn discard(&mut self, id: ExprId) -> Result<(), Diagnostic> {
        let mark = self.next_temp;
        let expression = *self.program.expression(id);
        match expression.kind {
            ExprKind::RegExp(_) => {}
            ExprKind::Call { .. } | ExprKind::Construct { .. } => self.call_expression(id, None)?,
            ExprKind::Eval(inner) => {
                let src = self.operand(inner)?;
                self.emit(Instruction::Eval { dst: None, src }, expression.span)?;
            }
            ExprKind::Swap { lhs, rhs } => {
                // The reference snapshots the left value, preserves a local RHS
                // address, then reevaluates both targets for their writes.
                let left = self.temporary(expression.span)?;
                self.expression(lhs, left)?;
                let right = self.operand(rhs)?;
                self.store_target(lhs, right, expression.span)?;
                self.store_target(rhs, left, expression.span)?;
            }
            ExprKind::PostfixIf { body, condition } => {
                let skip = self.condition_jump(condition)?;
                self.discard(body)?;
                self.patch_here(skip);
            }
            ExprKind::Sequence { lhs, rhs } => {
                self.discard(lhs)?;
                self.discard(rhs)?;
            }
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            } => {
                let branch = self.condition_jump(condition)?;
                self.discard(then_value)?;
                let end = self.jump(expression.span)?;
                self.patch_here(branch);
                self.next_temp = mark;
                self.discard(else_value)?;
                self.patch_here(end);
            }
            ExprKind::Update {
                target, increment, ..
            } => {
                let right = self.temporary(expression.span)?;
                self.emit(
                    Instruction::LoadInt {
                        dst: right,
                        value: if increment { 1 } else { -1 },
                    },
                    expression.span,
                )?;
                self.modify(
                    target,
                    if increment {
                        tjs_core::value::UpdateOp::Increment
                    } else {
                        tjs_core::value::UpdateOp::Decrement
                    },
                    right,
                    false,
                    expression.span,
                )?;
            }
            _ => {
                let dst = self.temporary(expression.span)?;
                self.expression(id, dst)?;
            }
        }
        self.next_temp = mark;
        Ok(())
    }
}

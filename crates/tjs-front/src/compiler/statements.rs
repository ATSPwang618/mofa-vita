use tjs_core::{CatchHandler, Diagnostic, FunctionId, Instruction, Phase, Register, Span};

use super::{Binding, Compiler, Context, Control, Scope};
use crate::ast::{ExprId, Statement, StmtId};

impl Compiler<'_> {
    /// Only the trailing statement contributes to a block/script completion.
    /// Earlier statements must pass null results to calls, as ordinary effects.
    pub(super) fn statement_list(
        &mut self,
        statements: &[crate::ast::StmtId],
    ) -> Result<(), Diagnostic> {
        let capture = self.capture_completion;
        for (index, &statement) in statements.iter().enumerate() {
            self.capture_completion = capture && index + 1 == statements.len();
            self.statement(statement)?;
        }
        self.capture_completion = capture;
        Ok(())
    }

    pub(super) fn statement(&mut self, id: StmtId) -> Result<(), Diagnostic> {
        match *self.program.statement(id) {
            Statement::Try {
                body,
                catch_name,
                catch_body,
                span,
            } => {
                let start = self.code.len() as u32;
                self.statement(body)?;
                let end = self.code.len() as u32;
                let skip_catch = self.jump(span)?;
                let target = self.code.len() as u32;
                let local_base = self.next_local;
                self.scopes.push(Scope::default());
                let exception = if let Some(name) = catch_name {
                    let units = self.name(name)?.to_vec();
                    let register = self.temporary(name)?;
                    self.next_local = self.next_temp;
                    self.scopes
                        .last_mut()
                        .expect("catch scope")
                        .bindings
                        .insert(units, Binding::active(register, name));
                    register
                } else {
                    Register(0)
                };
                self.reserve_scope([(catch_body, false)])?;
                self.statement(catch_body)?;
                self.scopes.pop();
                self.next_local = local_base;
                self.patch_here(skip_catch);
                self.handlers.push(CatchHandler {
                    start,
                    end,
                    target,
                    exception,
                });
            }
            Statement::Throw { value, span } => {
                self.expression(value, Register(0))?;
                self.emit(Instruction::Throw { src: Register(0) }, span)?;
            }
            Statement::Function { function, span } => {
                if self.context == Context::NamedFunction
                    && matches!(
                        self.program.function(function).kind,
                        crate::ast::FunctionKind::Function
                    )
                {
                    let name = self
                        .program
                        .function(function)
                        .name
                        .expect("named declaration");
                    let value = self.temporary(span)?;
                    self.emit(
                        Instruction::LoadFunction {
                            dst: value,
                            function: FunctionId(function.0 as u32 + 1),
                        },
                        span,
                    )?;
                    self.bind(name, value, span)?;
                }
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
            }
            Statement::Return { value, span } => {
                if let Some(value) = value {
                    self.expression(value, Register(0))?;
                } else {
                    self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
                }
                self.emit(Instruction::Return { src: Register(0) }, span)?;
            }
            Statement::Var {
                ref variables,
                span,
            } => {
                for &variable in variables {
                    self.declare(variable, span)?;
                }
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
            }
            Statement::Expression(expression) => {
                if self.context == Context::Script && self.capture_completion {
                    // Script evaluation exposes the expression result to the CLI.
                    self.completion_value(expression, Register(0))?;
                } else {
                    self.discard(expression)?;
                }
            }
            Statement::Empty(span) => {
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?
            }
            Statement::Block {
                ref statements,
                span,
            } => {
                let local_base = self.next_local;
                self.scopes.push(Scope::default());
                self.reserve_scope(statements.iter().map(|&id| (id, false)))?;
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
                self.statement_list(statements)?;
                self.scopes.pop();
                // TJS functions do not capture locals, so block slots can be reused.
                self.next_local = local_base;
            }
            Statement::If {
                condition,
                then_branch,
                else_branch,
                span,
            } => {
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
                let false_jump = self.condition_jump(condition)?;
                self.statement(then_branch)?;
                if let Some(else_branch) = else_branch {
                    let end_jump = self.jump(span)?;
                    self.patch_here(false_jump);
                    self.statement(else_branch)?;
                    self.patch_here(end_jump);
                } else {
                    self.patch_here(false_jump);
                }
            }
            Statement::While {
                condition,
                body,
                span,
            } => {
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
                let condition_pc = self.code.len() as u32;
                let exit_jump = self.condition_jump(condition)?;
                self.controls.push(Control::loop_scope());
                self.statement(body)?;
                self.emit(
                    Instruction::Jump {
                        target: condition_pc,
                    },
                    span,
                )?;
                self.patch_here(exit_jump);
                let loop_state = self.controls.pop().expect("entered loop");
                for jump in loop_state.continues.expect("loop scope") {
                    self.patch_to(jump, condition_pc);
                }
                for jump in loop_state.breaks {
                    self.patch_here(jump);
                }
            }
            Statement::For {
                initializer,
                condition,
                step,
                body,
                span,
            } => {
                let local_base = self.next_local;
                self.scopes.push(Scope::default());
                self.reserve_scope(
                    initializer
                        .into_iter()
                        .map(|id| (id, false))
                        .chain([(body, true)]),
                )?;
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
                if let Some(initializer) = initializer {
                    let capture = self.capture_completion;
                    self.capture_completion = false;
                    self.statement(initializer)?;
                    self.capture_completion = capture;
                }
                let condition_pc = self.code.len() as u32;
                let exit_jump = condition
                    .map(|expr| self.condition_jump(expr))
                    .transpose()?;
                self.controls.push(Control::loop_scope());
                self.statement(body)?;
                let step_pc = self.code.len() as u32;
                if let Some(step) = step {
                    self.discard(step)?;
                }
                self.emit(
                    Instruction::Jump {
                        target: condition_pc,
                    },
                    span,
                )?;
                if let Some(jump) = exit_jump {
                    self.patch_here(jump);
                }
                let loop_state = self.controls.pop().expect("entered loop");
                for jump in loop_state.continues.expect("loop scope") {
                    self.patch_to(jump, step_pc);
                }
                for jump in loop_state.breaks {
                    self.patch_here(jump);
                }
                self.scopes.pop();
                self.next_local = local_base;
            }
            Statement::DoWhile {
                body,
                condition,
                span,
            } => {
                self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;
                let body_pc = self.code.len() as u32;
                self.controls.push(Control::loop_scope());
                self.statement(body)?;
                let condition_pc = self.code.len() as u32;
                let exit_jump = self.condition_jump(condition)?;
                self.emit(Instruction::Jump { target: body_pc }, span)?;
                self.patch_here(exit_jump);
                let loop_state = self.controls.pop().expect("entered loop");
                for jump in loop_state.continues.expect("loop scope") {
                    self.patch_to(jump, condition_pc);
                }
                for jump in loop_state.breaks {
                    self.patch_here(jump);
                }
            }
            Statement::Break(span) => {
                if self.controls.is_empty() {
                    return Err(Diagnostic::new(
                        Phase::Compile,
                        span,
                        "break requires an enclosing loop or switch",
                    ));
                }
                let jump = self.jump(span)?;
                self.controls
                    .last_mut()
                    .expect("checked loop")
                    .breaks
                    .push(jump);
            }
            Statement::Continue(span) => {
                let Some(index) = self
                    .controls
                    .iter()
                    .rposition(|control| control.continues.is_some())
                else {
                    return Err(Diagnostic::new(
                        Phase::Compile,
                        span,
                        "continue requires an enclosing loop",
                    ));
                };
                let jump = self.jump(span)?;
                self.controls[index]
                    .continues
                    .as_mut()
                    .expect("loop scope")
                    .push(jump);
            }
            Statement::Switch { value, body, span } => self.switch(value, body, span)?,
            Statement::With { object, body, span } => self.with(object, body, span)?,
            Statement::Case { span, .. } => {
                return Err(Diagnostic::new(
                    Phase::Compile,
                    span,
                    "case/default must be directly inside a switch block",
                ));
            }
        }
        self.next_temp = self.next_local;
        Ok(())
    }

    fn switch(&mut self, value: ExprId, body: StmtId, span: Span) -> Result<(), Diagnostic> {
        let program = self.program;
        let Statement::Block { ref statements, .. } = *self.program.statement(body) else {
            unreachable!("parser requires a switch block")
        };
        let local_base = self.next_local;
        let selector = self.temporary(span)?;
        // A local operand must be copied: later case expressions may mutate it.
        self.expression(value, selector)?;
        self.next_local = self.next_temp;
        let case_base = self.next_local;
        self.scopes.push(Scope::default());
        self.reserve_scope(
            statements
                .iter()
                .take_while(|&&id| !matches!(program.statement(id), Statement::Case { .. }))
                .map(|&id| (id, false)),
        )?;
        self.controls.push(Control::default());
        self.emit(Instruction::LoadVoid { dst: Register(0) }, span)?;

        let mut next_test = None;
        let mut default_body = None;
        for (index, &statement) in statements.iter().enumerate() {
            if let Statement::Case { value, span } = *self.program.statement(statement) {
                // Fallthrough enters the body without evaluating its case expression.
                let fallthrough = next_test.map(|_| self.jump(span)).transpose()?;
                self.scopes.pop();
                self.next_local = case_base;
                self.next_temp = case_base;
                if let Some(jump) = next_test {
                    self.patch_here(jump);
                }
                next_test = Some(if let Some(value) = value {
                    let value = self.operand(value)?;
                    let condition = self.temporary(span)?;
                    self.emit(
                        Instruction::Equal {
                            dst: condition,
                            lhs: selector,
                            rhs: value,
                        },
                        span,
                    )?;
                    let jump = self.code.len();
                    self.emit(
                        Instruction::JumpIfFalse {
                            condition,
                            target: 0,
                        },
                        span,
                    )?;
                    jump
                } else {
                    let jump = self.jump(span)?;
                    // Like the reference, the last default is the no-match fallback.
                    default_body = Some(self.code.len() as u32);
                    jump
                });
                if let Some(jump) = fallthrough {
                    self.patch_here(jump);
                }
                // Case expressions cannot see the preceding case's local bindings.
                self.scopes.push(Scope::default());
                self.reserve_scope(
                    statements[index + 1..]
                        .iter()
                        .take_while(|&&id| !matches!(program.statement(id), Statement::Case { .. }))
                        .map(|&id| (id, false)),
                )?;
            } else {
                // TJS executes statements before the first label unconditionally.
                self.statement(statement)?;
            }
        }
        if let Some(next_test) = next_test {
            let end = self.jump(span)?;
            self.patch_here(next_test);
            if let Some(target) = default_body {
                self.emit(Instruction::Jump { target }, span)?;
            }
            self.patch_here(end);
        }
        for jump in self.controls.pop().expect("switch scope").breaks {
            self.patch_here(jump);
        }
        self.scopes.pop();
        self.next_local = local_base;
        Ok(())
    }

    fn with(&mut self, object: ExprId, body: StmtId, span: Span) -> Result<(), Diagnostic> {
        let local_base = self.next_local;
        let register = self.temporary(span)?;
        // Evaluate in the outer with context, then retain the value across statements.
        self.expression(object, register)?;
        self.next_local = self.next_temp;
        self.with_objects.push(register);
        self.scopes.push(Scope::default());
        self.reserve_scope([(body, false)])?;
        self.statement(body)?;
        self.scopes.pop();
        self.with_objects.pop();
        self.next_local = local_base;
        Ok(())
    }
}

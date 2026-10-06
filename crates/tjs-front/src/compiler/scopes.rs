use std::collections::HashMap;

use tjs_core::{Diagnostic, Instruction, Phase, Register, Span, ir::MAX_REGISTERS};

use super::{Compiler, Context};
use crate::ast::{Statement, StmtId};

#[derive(Default)]
pub(super) struct Scope {
    pub bindings: HashMap<Vec<u16>, Binding>,
}

pub(super) struct Binding {
    pub register: Register,
    pub active: bool,
    initialize: bool,
    span: Span,
}

impl Binding {
    pub fn active(register: Register, span: Span) -> Self {
        Self {
            register,
            active: true,
            initialize: false,
            span,
        }
    }
}

impl Compiler<'_> {
    pub(super) fn reserve_scope(
        &mut self,
        statements: impl IntoIterator<Item = (StmtId, bool)>,
    ) -> Result<(), Diagnostic> {
        for (statement, conditional) in statements {
            self.reserve_statement(statement, conditional)?;
        }
        self.next_temp = self.next_local;
        let mut initialize: Vec<_> = self
            .scopes
            .last()
            .expect("scope")
            .bindings
            .values()
            .filter(|binding| binding.initialize)
            .map(|binding| (binding.register, binding.span))
            .collect();
        initialize.sort_unstable_by_key(|&(register, _)| register.0);
        for (dst, span) in initialize {
            self.emit(Instruction::LoadVoid { dst }, span)?;
        }
        Ok(())
    }

    fn reserve_name(&mut self, span: Span, conditional: bool) -> Result<(), Diagnostic> {
        let name = self.name(span)?.to_vec();
        let bindings = &mut self.scopes.last_mut().expect("scope").bindings;
        if let Some(binding) = bindings.get_mut(&name) {
            binding.initialize |= conditional && !binding.active;
        } else {
            if self.next_local >= MAX_REGISTERS {
                return Err(Diagnostic::new(
                    Phase::Compile,
                    span,
                    "register limit exceeded",
                ));
            }
            bindings.insert(
                name,
                Binding {
                    register: Register(self.next_local),
                    active: false,
                    initialize: conditional,
                    span,
                },
            );
            self.next_local += 1;
            self.high_water = self.high_water.max(self.next_local);
        }
        Ok(())
    }

    fn reserve_statement(&mut self, id: StmtId, conditional: bool) -> Result<(), Diagnostic> {
        match *self.program.statement(id) {
            Statement::Var { ref variables, .. } => {
                for variable in variables {
                    self.reserve_name(variable.name, conditional)?;
                }
            }
            Statement::Function { function, .. }
                if self.context == Context::NamedFunction
                    && matches!(
                        self.program.function(function).kind,
                        crate::ast::FunctionKind::Function
                    ) =>
            {
                self.reserve_name(
                    self.program
                        .function(function)
                        .name
                        .expect("named declaration"),
                    conditional,
                )?;
            }
            Statement::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.reserve_statement(then_branch, true)?;
                if let Some(branch) = else_branch {
                    self.reserve_statement(branch, true)?;
                }
            }
            Statement::While { body, .. }
            | Statement::DoWhile { body, .. }
            | Statement::Try { body, .. } => {
                self.reserve_statement(body, true)?;
            }
            // Blocks, for, switch, with and catch create their own scopes.
            _ => {}
        }
        Ok(())
    }
}

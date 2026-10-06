//! Call argument/callee evaluation is shared by value and discard contexts.
use super::Compiler;
use crate::ast::{Argument, ExprId, ExprKind, MemberName};
use tjs_core::{
    ArgumentSource, CallArguments, CallSite, CallTarget, Diagnostic, Instruction, Register,
};

impl Compiler<'_> {
    pub(super) fn call_expression(
        &mut self,
        id: ExprId,
        dst: Option<Register>,
    ) -> Result<(), Diagnostic> {
        let expression = *self.program.expression(id);
        let (callee, arguments) = match expression.kind {
            ExprKind::Call { callee, arguments } | ExprKind::Construct { callee, arguments } => {
                (callee, arguments)
            }
            _ => unreachable!("call expression"),
        };
        let mut operands = Vec::new();
        let call_arguments = if let Some(arguments) = self.program.arguments(arguments) {
            let argument_start = Register(self.next_temp);
            let expanded = arguments
                .iter()
                .any(|argument| !matches!(argument, Argument::Value(_)));
            // Reserve the entire argument window before emitting expressions.
            // Nested calls then cannot overwrite earlier computed arguments.
            if !expanded {
                for _ in arguments {
                    self.temporary(expression.span)?;
                }
            }
            operands.reserve(arguments.len());
            for &argument in arguments {
                operands.push(match argument {
                    Argument::Value(value) => ArgumentSource::Value(self.operand(value)?),
                    Argument::Spread(value) => ArgumentSource::Array(self.operand(value)?),
                    Argument::ForwardRest(_) => ArgumentSource::Original {
                        start: self.unnamed_rest_start,
                    },
                });
            }
            if expanded {
                CallArguments::Expanded(std::mem::take(&mut operands).into_boxed_slice())
            } else {
                CallArguments::Registers {
                    start: argument_start,
                    count: arguments.len() as u32,
                }
            }
        } else {
            CallArguments::ForwardOriginal
        };
        // TJS evaluates arguments before the callee expression. Local
        // argument addresses are read only after both have finished.
        let target = if matches!(expression.kind, ExprKind::Construct { .. }) {
            CallTarget::Construct(self.operand(callee)?)
        } else {
            match self.program.expression(callee).kind {
                ExprKind::Name(name) => {
                    if let Some(local) = self.lookup(name)? {
                        CallTarget::Value(local)
                    } else {
                        let key = self.temporary(name)?;
                        self.load_name(name, key)?;
                        if self.global_context {
                            let object = self.temporary(name)?;
                            self.emit(Instruction::LoadGlobal { dst: object }, name)?;
                            CallTarget::Member {
                                object,
                                key,
                                computed: false,
                            }
                        } else {
                            CallTarget::Name { key }
                        }
                    }
                }
                ExprKind::Member { object, name } => {
                    let computed = matches!(name, MemberName::Computed(_));
                    let (object, key) = self.member_operands(object, name)?;
                    CallTarget::Member {
                        object,
                        key,
                        computed,
                    }
                }
                _ => CallTarget::Value(self.operand(callee)?),
            }
        };
        if let CallArguments::Registers { start, .. } = call_arguments {
            for (index, src) in operands.into_iter().enumerate() {
                let ArgumentSource::Value(src) = src else {
                    unreachable!("fixed call arguments")
                };
                self.emit(
                    Instruction::Move {
                        dst: Register(start.0 + index as u32),
                        src,
                    },
                    expression.span,
                )?;
            }
        }
        let site = self.calls.len() as u32;
        self.calls.push(CallSite {
            target,
            dst,
            arguments: call_arguments,
        });
        self.emit(Instruction::Call { site }, expression.span)?;
        Ok(())
    }
}

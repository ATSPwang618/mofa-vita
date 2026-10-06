//! The slow path used only when a native method requests a script callback.
use super::{
    CallError, Vm,
    calls::{Invocation, ReturnTo},
    dispatch::{Access, Action, ReadMode},
};
use crate::{Heap, NativeContinuation, NativeStep, ObjId};

pub(super) struct PendingNative {
    pub continuation: Box<dyn NativeContinuation>,
    pub context: ObjId,
    pub invocation: Invocation,
}
pub(super) struct PendingTryNative {
    pub continuation: Box<dyn crate::NativeTryContinuation>,
    pub context: ObjId,
    pub invocation: Invocation,
    pub frame: super::Frame,
    pub stack_len: usize,
    pub error: Option<crate::Value>,
}

impl Vm {
    /// Record a completion only. Its continuation executes in the next slice.
    pub fn resume_wait(
        &mut self,
        result: Result<crate::Value, crate::NativeError>,
    ) -> Result<(), crate::Diagnostic> {
        if !matches!(self.state, super::State::Waiting(_)) {
            return Err(self.diagnostic("VM has no outstanding native wait"));
        }
        self.state = super::State::Runnable;
        match result {
            Ok(value) => self.resume_value = Some(value),
            Err(error) => {
                self.frame.pc = self.frame.pc.saturating_sub(1);
                self.resume_error = Some(self.diagnostic(error.to_string()));
            }
        }
        Ok(())
    }

    pub(super) fn suspension(&self) -> Option<crate::VmExit> {
        match &self.state {
            super::State::Compiling => Some(crate::VmExit::CompileRequest(
                self.pending_compile
                    .as_ref()
                    .expect("compile request")
                    .request
                    .clone(),
            )),
            super::State::Inspecting(kind) => Some(crate::VmExit::Inspecting(*kind)),
            super::State::Waiting(request) => Some(crate::VmExit::Waiting(*request)),
            _ => None,
        }
    }

    pub(super) fn invoke_native_class(
        &mut self,
        heap: &mut Heap,
        class: ObjId,
        call: Invocation,
        construct: bool,
        advance: bool,
    ) -> Result<(), CallError> {
        let instance = if construct {
            heap.alloc_native_object(class)?
        } else {
            call.this.expect("native context")
        };
        heap.initialize_native(class, instance)?;
        if !construct {
            self.complete(call, crate::Value::Void, advance);
            return Ok(());
        }
        let name = heap.native_class(class)?.name;
        let units: Vec<_> = name.encode_utf16().collect();
        let symbol = heap.intern(&units);
        if heap.member(class, symbol)?.is_none() {
            self.complete(
                call,
                crate::Value::Obj(crate::ObjRef::bound(instance)),
                advance,
            );
            return Ok(());
        }
        let key = crate::Value::Str(heap.alloc_string(units));
        self.push_action(Action::Instance {
            instance,
            invocation: call,
        });
        let invocation = Invocation {
            destination: ReturnTo::ResumeDiscard,
            ..call
        };
        self.access(
            heap,
            crate::Value::Obj(crate::ObjRef {
                object: Some(class),
                this: Some(instance),
            }),
            key,
            Access::Call {
                invocation,
                instance: None,
            },
            None,
            advance,
        )
    }

    pub(super) fn native_step(
        &mut self,
        heap: &mut Heap,
        context: ObjId,
        call: Invocation,
        step: NativeStep,
        advance: bool,
    ) -> Result<(), CallError> {
        let construct = matches!(step, NativeStep::Construct { .. });
        let discard_result = matches!(step, NativeStep::CallDiscard { .. });
        let optional = matches!(step, NativeStep::GetOptional { .. });
        let read_fallback = match &step {
            NativeStep::GetOr { raw, fallback, .. } => Some((*raw, *fallback)),
            _ => None,
        };
        let required = matches!(step, NativeStep::GetRequired { .. });
        let required_fallback = match &step {
            NativeStep::GetRequiredOr { fallback, .. } => Some(*fallback),
            _ => None,
        };
        let member_result = match &step {
            NativeStep::CallMemberOr { result_needed, .. } => Some(*result_needed),
            _ => None,
        };
        let existing = matches!(step, NativeStep::SetExisting { .. });
        match step {
            NativeStep::GetProperty {
                object,
                key,
                flags,
                continuation,
            } => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.access(
                    heap,
                    object,
                    key,
                    Access::Read {
                        destination: ReturnTo::Resume,
                        mode: ReadMode::Flags(flags),
                    },
                    None,
                    advance,
                )
            }
            NativeStep::SetProperty {
                object,
                key,
                value,
                flags,
                continuation,
            } => {
                let ensure = crate::member::property_ensure(heap, object, key, flags)?;
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.access(
                    heap,
                    object,
                    key,
                    Access::Write {
                        destination: ReturnTo::Resume,
                        value,
                        ensure,
                        raw: flags.ignore_property,
                        hidden: flags.hidden,
                        class_only: flags.class_only,
                        ignore_invalid: false,
                    },
                    None,
                    advance,
                )
            }
            NativeStep::Throw(value) => {
                if !advance {
                    self.frame.pc = self.frame.pc.saturating_sub(1);
                }
                self.pending_exception = Some(super::PendingException {
                    value,
                    runtime: false,
                    depth: self.callers.len(),
                });
                Ok(())
            }
            NativeStep::Try { task, continuation } => {
                let mut frame = self.frame;
                frame.pc += usize::from(advance);
                self.push_action(Action::TryNative(Box::new(PendingTryNative {
                    continuation,
                    context,
                    invocation: call,
                    frame,
                    stack_len: self.registers.len(),
                    error: None,
                })));
                self.native_step(
                    heap,
                    context,
                    Invocation {
                        destination: ReturnTo::Resume,
                        ..call
                    },
                    NativeStep::Continue(task),
                    advance,
                )
            }
            NativeStep::TryInvalidate {
                object,
                continuation,
            } => {
                let mut frame = self.frame;
                frame.pc += usize::from(advance);
                self.push_action(Action::TryNative(Box::new(PendingTryNative {
                    continuation,
                    context,
                    invocation: call,
                    frame,
                    stack_len: self.registers.len(),
                    error: None,
                })));
                self.invalidate_native(heap, object, ReturnTo::Resume, advance)
            }
            NativeStep::Continue(continuation) => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                if advance {
                    self.frame.pc += 1;
                }
                self.resume_value = Some(crate::Value::Void);
                Ok(())
            }
            NativeStep::Inspect { kind, continuation } => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                if advance {
                    self.frame.pc += 1;
                }
                self.state = super::State::Inspecting(kind);
                Ok(())
            }
            NativeStep::Evaluate { request, context } => {
                self.evaluate_native(heap, request, context, call, advance);
                Ok(())
            }
            NativeStep::Wait {
                request,
                continuation,
            } => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                if advance {
                    self.frame.pc += 1;
                }
                self.state = super::State::Waiting(request);
                Ok(())
            }
            NativeStep::Return(value) => {
                self.complete(call, value, advance);
                Ok(())
            }
            NativeStep::Invalidate {
                object,
                continuation,
            } => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.invalidate_native(heap, object, ReturnTo::Resume, advance)
            }
            NativeStep::Get {
                object,
                key,
                continuation,
            }
            | NativeStep::GetOptional {
                object,
                key,
                continuation,
            }
            | NativeStep::GetRequired {
                object,
                key,
                continuation,
            }
            | NativeStep::GetRequiredOr {
                object,
                key,
                fallback: _,
                continuation,
            }
            | NativeStep::GetOr {
                object,
                key,
                raw: _,
                fallback: _,
                continuation,
            } => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.access(
                    heap,
                    object,
                    key,
                    Access::Read {
                        destination: ReturnTo::Resume,
                        mode: if let Some(fallback) = required_fallback {
                            ReadMode::RequiredOr(fallback)
                        } else if let Some((true, fallback)) = read_fallback {
                            ReadMode::RawOptional(fallback)
                        } else if let Some((false, fallback)) = read_fallback {
                            ReadMode::OptionalOr(fallback)
                        } else if optional {
                            ReadMode::Optional
                        } else if required {
                            ReadMode::Required
                        } else {
                            ReadMode::Value
                        },
                    },
                    None,
                    advance,
                )
            }
            NativeStep::CallMember {
                object,
                key,
                arguments,
                continuation,
            }
            | NativeStep::CallMemberOr {
                object,
                key,
                arguments,
                result_needed: _,
                continuation,
            } => {
                let start = self.registers.len();
                let end = start.checked_add(arguments.len()).ok_or(CallError::Stack)?;
                if end > self.limits.max_stack_values {
                    return Err(CallError::Stack);
                }
                self.registers.extend(arguments);
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.access(
                    heap,
                    object,
                    key,
                    Access::Call {
                        invocation: Invocation {
                            start,
                            end,
                            this: Some(context),
                            destination: member_result.map_or(ReturnTo::Resume, |result_needed| {
                                ReturnTo::ResumeStatus { result_needed }
                            }),
                        },
                        instance: None,
                    },
                    None,
                    advance,
                )
            }
            NativeStep::Set {
                object,
                key,
                value,
                continuation,
            }
            | NativeStep::SetExisting {
                object,
                key,
                value,
                continuation,
            } => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.access(
                    heap,
                    object,
                    key,
                    Access::Write {
                        destination: if existing {
                            ReturnTo::ResumeStatus {
                                result_needed: false,
                            }
                        } else {
                            ReturnTo::Resume
                        },
                        value,
                        ensure: !existing,
                        raw: false,
                        hidden: false,
                        class_only: false,
                        ignore_invalid: false,
                    },
                    None,
                    advance,
                )
            }
            NativeStep::CopyMember {
                object,
                key,
                value,
                class_only,
                continuation,
            } => {
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.access(
                    heap,
                    object,
                    key,
                    Access::Write {
                        destination: ReturnTo::Resume,
                        value,
                        ensure: true,
                        raw: true,
                        hidden: false,
                        class_only,
                        ignore_invalid: true,
                    },
                    None,
                    advance,
                )
            }
            NativeStep::Call {
                function,
                arguments,
                continuation,
            }
            | NativeStep::CallDiscard {
                function,
                arguments,
                continuation,
            }
            | NativeStep::Construct {
                class: function,
                arguments,
                continuation,
            } => {
                let start = self.registers.len();
                let end = start.checked_add(arguments.len()).ok_or(CallError::Stack)?;
                if end > self.limits.max_stack_values {
                    return Err(CallError::Stack);
                }
                self.registers.extend(arguments);
                self.push_action(Action::Native(Box::new(PendingNative {
                    continuation,
                    context,
                    invocation: call,
                })));
                self.invoke(
                    heap,
                    function,
                    Invocation {
                        start,
                        end,
                        this: Some(context),
                        destination: if discard_result {
                            ReturnTo::ResumeDiscard
                        } else {
                            ReturnTo::Resume
                        },
                    },
                    construct,
                    advance,
                )
            }
        }
    }
}

//! Runtime errors construct global.Exception at the catch boundary. Replacing
//! that class, its getter, or its trace setter follows ordinary VM dispatch.
use super::{
    CallError, Vm,
    calls::{Invocation, ReturnTo},
    dispatch::{Access, Action, ReadMode},
};
use super::{PendingException, State};
use crate::{Diagnostic, Heap, NativeError, ObjRef, Value, VmExit};

pub(super) enum Stage {
    Class,
    Constructed,
    Trace,
}
pub(super) struct BuildException {
    pub value: Value,
    pub message: Value,
    pub trace: Value,
    destination: usize,
    stage: Stage,
}
impl Vm {
    pub(super) fn build_exception(
        &mut self,
        heap: &mut Heap,
        value: Value,
        destination: usize,
    ) -> Result<(), CallError> {
        let Value::Obj(reference) = value else {
            unreachable!()
        };
        let object = reference.object.expect("runtime exception");
        let mut field = |name: &str| {
            let key = heap.intern_str(name);
            heap.member(object, key)
                .expect("live exception")
                .expect("exception field")
        };
        let message = field("message");
        let trace = field("trace");
        self.push_action(Action::Exception(BuildException {
            value,
            message,
            trace,
            destination,
            stage: Stage::Class,
        }));
        let global = self.ensure_global(heap);
        let key = Value::Str(heap.alloc_string("Exception".encode_utf16().collect::<Vec<_>>()));
        self.access(
            heap,
            Value::Obj(ObjRef::bound(global)),
            key,
            Access::Read {
                destination: ReturnTo::Resume,
                mode: ReadMode::Value,
            },
            None,
            false,
        )
    }
    pub(super) fn resume_exception(
        &mut self,
        heap: &mut Heap,
        mut build: BuildException,
        result: Value,
    ) -> Result<(), CallError> {
        match build.stage {
            Stage::Class => {
                if let Value::Obj(reference) = result {
                    if reference.object == heap.registered_class("Exception") {
                        self.registers[build.destination] = build.value;
                        self.frame.pc -= 1;
                        return Ok(());
                    }
                }
                let start = self.registers.len();
                if start >= self.limits.max_stack_values {
                    return Err(CallError::Stack);
                }
                self.registers.push(build.message);
                build.stage = Stage::Constructed;
                self.push_action(Action::Exception(build));
                self.invoke(
                    heap,
                    result,
                    Invocation {
                        start,
                        end: start + 1,
                        this: None,
                        destination: ReturnTo::Resume,
                    },
                    true,
                    false,
                )
            }
            Stage::Constructed => {
                if !matches!(
                    result,
                    Value::Obj(ObjRef {
                        object: Some(_),
                        ..
                    })
                ) {
                    return Err(NativeError::Type("an Exception object").into());
                }
                build.value = result;
                build.stage = Stage::Trace;
                let trace = build.trace;
                self.push_action(Action::Exception(build));
                let key = Value::Str(heap.alloc_string("trace".encode_utf16().collect::<Vec<_>>()));
                self.access(
                    heap,
                    result,
                    key,
                    Access::Write {
                        destination: ReturnTo::Resume,
                        value: trace,
                        ensure: true,
                        raw: false,
                        hidden: false,
                        class_only: false,
                        ignore_invalid: false,
                    },
                    None,
                    false,
                )?;
                Ok(())
            }
            Stage::Trace => {
                self.registers[build.destination] = build.value;
                self.frame.pc -= 1;
                Ok(())
            }
        }
    }
}

impl Vm {
    fn fault(&mut self, message: impl Into<String>) -> VmExit {
        let error = self.diagnostic(message);
        self.fail(error)
    }

    fn fail(&mut self, error: Diagnostic) -> VmExit {
        self.continuations.clear();
        self.resume_value = None;
        self.pending_compile = None;
        self.resume_error = None;
        self.state = State::Fault(error.clone());
        VmExit::Fault(error)
    }

    pub(super) fn runtime_error(
        &mut self,
        heap: &mut Heap,
        message: impl Into<String>,
    ) -> Option<VmExit> {
        let error = self.diagnostic(message);
        self.runtime_diagnostic(heap, error)
    }

    pub(super) fn runtime_diagnostic(
        &mut self,
        heap: &mut Heap,
        error: Diagnostic,
    ) -> Option<VmExit> {
        let caught = self
            .continuations
            .iter()
            .any(|c| matches!(c.action, Action::TryNative(_)))
            || self.modules[self.frame.module].module.functions()[self.frame.function]
                .handler_at(self.frame.pc)
                .is_some()
            || self.callers.iter().any(|frame| {
                self.modules[frame.module].module.functions()[frame.function]
                    .handler_at(frame.pc - 1)
                    .is_some()
            });
        if !caught {
            return Some(self.fail(error));
        }
        let install_global = heap.registered_class("Exception").is_none();
        match crate::exception::runtime(heap, &error) {
            Ok(value) => {
                if install_global {
                    let global = self.ensure_global(heap);
                    let key = heap.intern_str("Exception");
                    let class = heap.registered_class("Exception").expect("installed");
                    heap.set_member(global, key, Value::Obj(class.into()))
                        .expect("live global");
                }
                self.pending_exception = Some(PendingException {
                    value,
                    runtime: true,
                    depth: self.callers.len(),
                });
                None
            }
            Err(error) => Some(self.fault(error.to_string())),
        }
    }
}

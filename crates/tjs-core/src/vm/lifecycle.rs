use super::{
    CallError, Vm,
    calls::{Invocation, ReturnTo},
    dispatch::{Access, Action, ReadMode},
};
use crate::heap::lifecycle::{BeginFinalization, Finalization};
use crate::{
    Heap, HeapError, ObjRef, Value,
    member::{self, MemberError},
};

impl Vm {
    /// Claim one GC finalizer. The returned VM owns the object's root and uses
    /// ordinary slice/depth/exception rules. Dropping it cancels the callback;
    /// an automatic callback is attempted at most once, never from Rust Drop.
    pub fn take_finalizer(heap: &mut Heap) -> Option<Self> {
        use crate::{Instruction, Module, Register};
        static MODULE: std::sync::OnceLock<Module> = std::sync::OnceLock::new();
        let object = heap.take_finalizer()?;
        let module = MODULE.get_or_init(|| {
            Module::new(
                1,
                vec![
                    Instruction::LoadThis { dst: Register(0) },
                    Instruction::Invalidate {
                        dst: Register(0),
                        src: Register(0),
                    },
                    Instruction::Return { src: Register(0) },
                ],
                vec![None; 3],
            )
            .expect("finalizer entry")
        });
        Some(Self::with_global(module, object))
    }

    pub(super) fn invalidate(
        &mut self,
        heap: &mut Heap,
        value: Value,
        destination: ReturnTo,
    ) -> Result<(), CallError> {
        self.invalidate_native(heap, value, destination, true)
    }

    pub(super) fn invalidate_native(
        &mut self,
        heap: &mut Heap,
        value: Value,
        destination: ReturnTo,
        advance: bool,
    ) -> Result<(), CallError> {
        let Value::Obj(reference) = value else {
            self.deliver(destination, Value::Int(0));
            if advance {
                self.frame.pc += 1;
            }
            return Ok(());
        };
        let object = reference.object.ok_or(MemberError::NullObject)?;
        let guard = match heap.begin_finalization(object)? {
            BeginFinalization::Unsupported | BeginFinalization::AlreadyInvalid => {
                self.deliver(destination, Value::Int(0));
                if advance {
                    self.frame.pc += 1;
                }
                return Ok(());
            }
            BeginFinalization::Reentrant => {
                self.deliver(destination, Value::Int(1));
                if advance {
                    self.frame.pc += 1;
                }
                return Ok(());
            }
            BeginFinalization::Started(guard) => guard,
        };
        if !heap.calls_finalize(object)? {
            return self.finish_invalidation(heap, guard, destination, advance);
        }
        if matches!(self.finalize_name, Value::Void) {
            self.finalize_name =
                Value::Str(heap.alloc_string("finalize".encode_utf16().collect::<Vec<_>>()));
        }
        // Only plain objects call finalize; their script members are copied at
        // construction, so no class-proxy search is needed here.
        match member::callable(heap, Value::Obj(ObjRef::bound(object)), self.finalize_name) {
            Ok(function) => self.call_finalizer(heap, guard, destination, function, advance),
            Err(MemberError::Invoke { function, .. }) => {
                self.push_action(Action::Finalize {
                    guard,
                    destination,
                    getter: true,
                });
                self.invoke_accessor(heap, function, None, ReturnTo::Resume, advance)
            }
            Err(MemberError::MissingHook { object, .. }) => {
                // CustomObject::Finalize uses FuncCall, including CallGetMissing.
                // Keep the finalization guard across that script callback. A
                // declined lookup or a non-callable result means no finalizer;
                // exceptions raised inside the handler still propagate.
                self.push_action(Action::Finalize {
                    guard,
                    destination,
                    getter: true,
                });
                let receiver = Value::Obj(ObjRef::bound(object));
                self.access(
                    heap,
                    receiver,
                    self.finalize_name,
                    Access::Read {
                        destination: ReturnTo::Resume,
                        mode: ReadMode::Optional,
                    },
                    None,
                    advance,
                )
            }
            Err(MemberError::Missing | MemberError::AccessDenied) => {
                self.finish_invalidation(heap, guard, destination, advance)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub(super) fn call_finalizer(
        &mut self,
        heap: &mut Heap,
        guard: Finalization,
        destination: ReturnTo,
        function: Value,
        advance: bool,
    ) -> Result<(), CallError> {
        let object = guard.object;
        self.push_action(Action::Finalize {
            guard,
            destination,
            getter: false,
        });
        let start = self.registers.len();
        let result = self.invoke(
            heap,
            function,
            Invocation {
                start,
                end: start,
                this: Some(object),
                destination: ReturnTo::ResumeDiscard,
            },
            false,
            advance,
        );
        // CustomObject::Finalize ignores dispatch status, but exceptions raised
        // by an executing script/native callback still escape normally.
        if matches!(
            result,
            Err(CallError::NotCallable | CallError::Heap(HeapError::InvalidObject))
        ) {
            let Action::Finalize {
                guard, destination, ..
            } = self.continuations.pop().expect("finalizer call").action
            else {
                unreachable!("finalizer action")
            };
            self.finish_invalidation(heap, guard, destination, advance)
        } else {
            result
        }
    }

    pub(super) fn finish_invalidation(
        &mut self,
        heap: &mut Heap,
        mut guard: Finalization,
        destination: ReturnTo,
        advance: bool,
    ) -> Result<(), CallError> {
        if let Some(call) = heap.next_native_invalidator(&mut guard)? {
            let context = guard.object;
            self.push_action(Action::Finalize {
                guard,
                destination,
                getter: false,
            });
            let mut cx = crate::NativeCx::new(heap, context, false);
            let step = match call {
                crate::NativeCallable::Leaf(call) | crate::NativeCallable::EmptyFinalizer(call) => {
                    crate::NativeStep::Return(call(&mut cx, &[])?)
                }
                crate::NativeCallable::Resumable(call) => call(&mut cx, &[])?,
            };
            let start = self.registers.len();
            return self.native_step(
                heap,
                context,
                Invocation {
                    start,
                    end: start,
                    this: Some(context),
                    destination: ReturnTo::ResumeDiscard,
                },
                step,
                advance,
            );
        }
        heap.finish_finalization(guard)?;
        self.deliver(destination, Value::Int(1));
        if advance {
            self.frame.pc += 1;
        }
        Ok(())
    }
}

use super::dispatch::{Access, Action};
use super::{CallError, Frame, LoadedModule, Vm};
use crate::{
    ArgumentSource, CallArguments, CallSite, CallTarget, FunctionId, FunctionKind, Heap, HeapError,
    NativeCallable, NativeCx, NativeStep, ObjId, Value,
};

#[derive(Clone, Copy)]
pub(super) enum ReturnTo {
    Register(usize),
    Discard,
    Resume,
    /// Wake the pending continuation, but pass no result slot to the callee.
    ResumeDiscard,
    /// A native member dispatch may ignore structural failure statuses. This
    /// is not an exception boundary around the function that is called.
    ResumeStatus {
        result_needed: bool,
    },
}

impl ReturnTo {
    pub(super) fn result_needed(self) -> bool {
        matches!(
            self,
            Self::Register(_)
                | Self::Resume
                | Self::ResumeStatus {
                    result_needed: true
                }
        )
    }
    pub(super) fn ignores_status(self) -> bool {
        matches!(self, Self::ResumeStatus { .. })
    }
    fn discard_result(self) -> Self {
        match self {
            Self::Resume | Self::ResumeDiscard => Self::ResumeDiscard,
            Self::ResumeStatus { .. } => Self::ResumeStatus {
                result_needed: false,
            },
            Self::Register(_) | Self::Discard => Self::Discard,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Invocation {
    pub start: usize,
    pub end: usize,
    pub this: Option<ObjId>,
    pub destination: ReturnTo,
}

impl Vm {
    fn argument_array<'heap>(
        &self,
        heap: &'heap Heap,
        register: crate::Register,
    ) -> Result<&'heap [Value], CallError> {
        let Value::Obj(reference) = self.registers[self.frame.base + register.0 as usize] else {
            return Err(HeapError::NotArray.into());
        };
        Ok(heap.array(reference.object.ok_or(HeapError::NotArray)?)?)
    }

    fn append_arguments(
        &mut self,
        heap: &Heap,
        arguments: &CallArguments,
        source: usize,
        count: usize,
    ) {
        if let CallArguments::Expanded(sources) = arguments {
            self.registers.reserve(count);
            for source in sources {
                match *source {
                    ArgumentSource::Value(register) => {
                        self.registers
                            .push(self.registers[self.frame.base + register.0 as usize]);
                    }
                    ArgumentSource::Array(register) => {
                        let values = self
                            .argument_array(heap, register)
                            .expect("arguments validated before stack growth");
                        self.registers.extend_from_slice(values);
                    }
                    ArgumentSource::Original { start } => {
                        let start = self.frame.argument_start
                            + (start as usize).min(self.original_arguments().len());
                        self.registers.extend_from_within(start..self.frame.base);
                    }
                }
            }
        } else {
            self.registers.extend_from_within(source..source + count);
        }
    }

    pub(super) fn call(&mut self, heap: &mut Heap, call: &CallSite) -> Result<(), CallError> {
        let stack_start = self.registers.len();
        let result = self.call_inner(heap, call);
        if result.is_err() {
            self.registers.truncate(stack_start);
        }
        result
    }

    fn call_inner(&mut self, heap: &mut Heap, call: &CallSite) -> Result<(), CallError> {
        // CallFunctionIndirect stringifies before validating/copying expanded
        // arguments. Keep the converted value through suspended member lookup.
        // Integers use callable's full-width stack formatter, never Prop*ByNum.
        let member_key = match call.target {
            CallTarget::Member {
                key,
                computed: true,
                ..
            } => {
                let value = self.registers[self.frame.base + key.0 as usize];
                Some(if matches!(value, Value::Int(_) | Value::Str(_)) {
                    value
                } else {
                    crate::value::to_string(heap, value)?
                })
            }
            _ => None,
        };
        let (source, count) = match &call.arguments {
            CallArguments::Registers { start, count } => {
                (self.frame.base + start.0 as usize, *count as usize)
            }
            CallArguments::ForwardOriginal => {
                (self.frame.argument_start, self.original_arguments().len())
            }
            CallArguments::Expanded(sources) => {
                let mut count = 0_usize;
                for source in sources {
                    let size = match *source {
                        ArgumentSource::Value(_) => 1,
                        ArgumentSource::Array(register) => {
                            self.argument_array(heap, register)?.len()
                        }
                        ArgumentSource::Original { start } => self
                            .original_arguments()
                            .len()
                            .saturating_sub(start as usize),
                    };
                    count = count.checked_add(size).ok_or(CallError::Stack)?;
                }
                (0, count)
            }
        };
        let argument_start = self.registers.len();
        let base = argument_start.checked_add(count).ok_or(CallError::Stack)?;
        if base > self.limits.max_stack_values {
            return Err(CallError::Stack);
        }
        self.append_arguments(heap, &call.arguments, source, count);
        let invocation = Invocation {
            start: argument_start,
            end: base,
            this: self.frame.this,
            destination: call.dst.map_or(ReturnTo::Discard, |dst| {
                ReturnTo::Register(self.frame.base + dst.0 as usize)
            }),
        };
        match call.target {
            CallTarget::Direct(function) => {
                if matches!(
                    self.modules[self.frame.module].module.functions()[function.0 as usize].kind(),
                    FunctionKind::SuperResolver
                ) {
                    self.complete_unchanged(invocation, true);
                    Ok(())
                } else {
                    self.enter(heap, self.frame.module, function, None, invocation, true)
                }
            }
            CallTarget::Value(register) | CallTarget::Construct(register) => {
                let value = self.registers[self.frame.base + register.0 as usize];
                self.invoke(
                    heap,
                    value,
                    invocation,
                    matches!(call.target, CallTarget::Construct(_)),
                    true,
                )
            }
            CallTarget::Name { key } => {
                let key = self.registers[self.frame.base + key.0 as usize];
                let (receiver, fallback) = self.name_receivers(heap);
                self.access(
                    heap,
                    receiver,
                    key,
                    Access::Call {
                        invocation,
                        instance: None,
                    },
                    fallback,
                    true,
                )
            }
            CallTarget::Member {
                object,
                key,
                computed: _,
            } => {
                let receiver = self.registers[self.frame.base + object.0 as usize];
                let key = member_key.unwrap_or(self.registers[self.frame.base + key.0 as usize]);
                if let Value::Octet(octet) = receiver {
                    let Value::Str(name) = crate::value::to_string(heap, key)? else {
                        unreachable!()
                    };
                    if !crate::string::c_string(heap.string(name)?)
                        .iter()
                        .copied()
                        .eq("unpack".encode_utf16())
                    {
                        return Err(crate::member::MemberError::Missing.into());
                    }
                    let template = *self.registers[invocation.start..invocation.end]
                        .first()
                        .ok_or(crate::NativeError::Missing(1))?;
                    if !matches!(template, Value::Str(_)) {
                        return Err(
                            crate::NativeError::Type("a pack/unpack template string").into()
                        );
                    }
                    // An empty octet has a null reference pointer in TJS.
                    if heap.octet(octet)?.is_empty() {
                        return Err(crate::NativeError::Message(
                            "unpack requires a nonempty octet",
                        )
                        .into());
                    }
                    if !invocation.destination.result_needed() {
                        self.complete(invocation, Value::Void, true);
                        return Ok(());
                    }
                    let value = crate::octet::unpack(heap, octet, template)?;
                    self.complete(invocation, value, true);
                    return Ok(());
                }
                if let Value::Str(string) = receiver {
                    return self.call_string(heap, string, key, invocation);
                }
                let receiver = self.member_receiver(heap, receiver);
                self.access(
                    heap,
                    receiver,
                    key,
                    Access::Call {
                        invocation,
                        instance: None,
                    },
                    None,
                    true,
                )
            }
        }
    }

    pub(super) fn invoke(
        &mut self,
        heap: &mut Heap,
        value: Value,
        mut call: Invocation,
        construct: bool,
        advance: bool,
    ) -> Result<(), CallError> {
        let Value::Obj(reference) = value else {
            return Err(CallError::NotCallable);
        };
        let object = reference.object.ok_or(CallError::NotCallable)?;
        heap.ensure_valid(object)?;
        let fallback = self.this(heap);
        call.this = Some(reference.this.or(call.this).unwrap_or(fallback));
        if heap.object(object)?.kind() == crate::ObjectKind::NativeClass {
            return self.invoke_native_class(heap, object, call, construct, advance);
        }
        if let Some(native) = heap.native_callable(object, construct)? {
            let context = call.this.expect("context");
            if heap.is_native_constructor(object)? {
                self.push_action(Action::ConstructorReturn { invocation: call });
                call.destination = ReturnTo::ResumeDiscard;
            }
            let mut cx = NativeCx::new(heap, context, call.destination.result_needed())
                .with_function(object);
            let args = &self.registers[call.start..call.end];
            let step = match native {
                NativeCallable::Leaf(native) | NativeCallable::EmptyFinalizer(native) => {
                    NativeStep::Return(native(&mut cx, args)?)
                }
                NativeCallable::Resumable(native) => native(&mut cx, args)?,
            };
            return self.native_step(heap, context, call, step, advance);
        }
        let function = heap.function(object)?.ok_or(CallError::NotCallable)?;
        if matches!(function.kind(), FunctionKind::Property { .. }) {
            return Err(CallError::NotCallable);
        }
        if construct && !matches!(function.kind(), FunctionKind::Class { .. }) {
            return Err(CallError::NotCallable);
        }
        if matches!(function.kind(), FunctionKind::SuperResolver) {
            self.complete_unchanged(call, advance);
            return Ok(());
        }
        let module = if self.modules[self.frame.module].function_pool == Some(function.pool) {
            self.frame.module
        } else {
            self.modules
                .iter()
                .position(|loaded| loaded.function_pool == Some(function.pool))
                .unwrap_or_else(|| {
                    let index = self.modules.len();
                    let mut loaded = LoadedModule::new(&function.module, Some(function.global));
                    loaded.function_pool = Some(function.pool);
                    self.modules.push(loaded);
                    index
                })
        };
        let id = function.function;
        if construct {
            let instance = heap.alloc_object();
            self.push_action(Action::Construct {
                class: object,
                instance,
                invocation: call,
            });
            let init = Invocation {
                start: self.registers.len(),
                end: self.registers.len(),
                this: Some(instance),
                destination: ReturnTo::ResumeDiscard,
            };
            self.enter(heap, module, id, Some(object), init, advance)
        } else {
            self.enter(heap, module, id, Some(object), call, advance)
        }
    }

    pub(super) fn enter(
        &mut self,
        _heap: &Heap,
        module: usize,
        function: FunctionId,
        callee_object: Option<ObjId>,
        call: Invocation,
        advance: bool,
    ) -> Result<(), CallError> {
        if self.call_depth() >= self.limits.max_call_depth {
            return Err(CallError::Depth);
        }
        let callee = &self.modules[module].module.functions()[function.0 as usize];
        let base = call.end;
        let end = base
            .checked_add(callee.register_count() as usize)
            .ok_or(CallError::Stack)?;
        if end > self.limits.max_stack_values {
            return Err(CallError::Stack);
        }
        let copied = (call.end - call.start).min(callee.parameter_count() as usize);
        if copied != 0 {
            // Register zero is the return slot. Copy supplied parameters once;
            // missing parameters and all other locals still start as void.
            self.registers.push(Value::Void);
            self.registers
                .extend_from_within(call.start..call.start + copied);
        }
        self.registers.resize(end, Value::Void);
        let frame = Frame {
            module,
            function: function.0 as usize,
            this: call.this,
            callee: callee_object,
            pc: 0,
            base,
            argument_start: call.start,
            return_destination: call.destination,
        };
        if advance {
            self.frame.pc += 1;
        }
        self.callers.push(self.frame);
        self.frame = frame;
        Ok(())
    }

    pub(super) fn complete(&mut self, call: Invocation, result: Value, advance: bool) {
        self.registers.truncate(call.start);
        if advance {
            self.frame.pc += 1;
        }
        self.deliver(call.destination, result);
    }

    fn complete_unchanged(&mut self, call: Invocation, advance: bool) {
        let result = match call.destination {
            ReturnTo::Register(index) => self.registers[index],
            ReturnTo::Discard
            | ReturnTo::Resume
            | ReturnTo::ResumeDiscard
            | ReturnTo::ResumeStatus { .. } => Value::Void,
        };
        self.complete(call, result, advance);
    }

    pub(super) fn deliver(&mut self, destination: ReturnTo, value: Value) {
        match destination {
            ReturnTo::Register(dst) => self.registers[dst] = value,
            ReturnTo::Discard => {}
            ReturnTo::Resume => self.resume_value = Some(value),
            ReturnTo::ResumeDiscard => self.resume_value = Some(Value::Void),
            ReturnTo::ResumeStatus { result_needed } => {
                self.resume_value = Some(if result_needed { value } else { Value::Void })
            }
        }
    }

    pub(super) fn invoke_accessor(
        &mut self,
        heap: &mut Heap,
        function: Value,
        argument: Option<Value>,
        destination: ReturnTo,
        advance: bool,
    ) -> Result<(), CallError> {
        let start = self.registers.len();
        if let Some(argument) = argument {
            if start >= self.limits.max_stack_values {
                return Err(CallError::Stack);
            }
            self.registers.push(argument);
        }
        self.invoke(
            heap,
            function,
            Invocation {
                start,
                end: self.registers.len(),
                this: None,
                destination: if argument.is_some() {
                    destination.discard_result()
                } else {
                    destination
                },
            },
            false,
            advance,
        )
    }
}

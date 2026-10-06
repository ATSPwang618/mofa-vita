use std::sync::Arc;
use tjs_bind::{Heap, NativeCx, NativeError, NativeResult, NativeStep, ObjId, RestArgs, Value};
use tjs_core::{CompileRequest, string, value};

mod hooks;

fn units(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), value)? else {
        unreachable!()
    };
    Ok(string::c_string(cx.heap().string(id)?).to_vec())
}
fn context(value: Option<&Value>) -> NativeResult<Option<ObjId>> {
    match value {
        None | Some(Value::Void) => Ok(None),
        Some(Value::Obj(reference)) => Ok(reference.object),
        _ => Err(NativeError::Type("an object context")),
    }
}

#[tjs_bind::class(name = "Scripts", static_class = true)]
mod implementation {
    use super::*;
    pub struct State {
        pub encoding: Vec<u16>,
        pub operations: Option<crate::operations::Shared>,
        pub(super) after_load: hooks::Registry,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                encoding: "UTF-8".encode_utf16().collect(),
                operations: None,
                after_load: hooks::Registry::default(),
            }
        }
    }
    impl tjs_bind::Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.after_load.trace(visit);
        }
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method(name = "afterLoad", class_only = true)]
        fn after_load(cx: &mut NativeCx<'_>, name: Value, handler: Value) -> NativeResult<()> {
            let name = units(cx, name)?;
            let Value::Obj(reference) = handler else {
                return Err(NativeError::Type("an afterLoad function"));
            };
            let object = reference
                .object
                .ok_or(NativeError::Type("an afterLoad function"))?;
            if !matches!(
                cx.heap().object(object)?.kind(),
                tjs_core::ObjectKind::Function | tjs_core::ObjectKind::NativeFunction
            ) {
                return Err(NativeError::Type("an afterLoad function"));
            }
            // Class-only functions are unbound in TJS. Their state belongs to
            // the registered class, independently of the caller's this.
            let class = cx
                .heap()
                .registered_class("Scripts")
                .expect("installed Scripts");
            cx.heap_mut().with_native_state::<Self, _>(class, |state| {
                state.after_load.register(&name, handler)
            })?
        }
        #[tjs::method(class_only = true, resumable = true)]
        fn exec(
            cx: &mut NativeCx<'_>,
            source: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            evaluate(cx, source, args, false, cx.result_needed())
        }
        #[tjs::method(class_only = true, resumable = true)]
        fn eval(
            cx: &mut NativeCx<'_>,
            source: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            evaluate(cx, source, args, true, cx.result_needed())
        }
        #[tjs::method(name = "execStorage", class_only = true, resumable = true)]
        fn exec_storage(
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            storage(cx, name, args, false, cx.result_needed(), None)
        }
        #[tjs::method(name = "evalStorage", class_only = true, resumable = true)]
        fn eval_storage(
            cx: &mut NativeCx<'_>,
            name: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            storage(cx, name, args, true, cx.result_needed(), None)
        }
        #[tjs::method(name = "getTraceString", class_only = true, resumable = true)]
        fn trace(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let limit = args
                .first()
                .map(|&value| value::to_integer(cx.heap(), value))
                .transpose()?
                .unwrap_or(0) as i32;
            Ok(NativeStep::Inspect {
                kind: tjs_core::Inspection::StackTrace { limit },
                continuation: Box::new(Inspected::Return),
            })
        }
        #[tjs::method(class_only = true, resumable = true)]
        fn dump(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let debug = cx
                .heap()
                .registered_class("Debug")
                .ok_or(NativeError::Message("Debug is not installed"))?;
            let key = cx.heap_mut().intern_str("message");
            let function = cx
                .heap()
                .member(debug, key)?
                .ok_or(NativeError::Message("Debug.message is missing"))?;
            Ok(NativeStep::Inspect {
                kind: tjs_core::Inspection::Dump,
                continuation: Box::new(Inspected::Log(function)),
            })
        }
        #[tjs::method(name = "compileStorage", class_only = true, resumable = true)]
        fn compile_storage(
            cx: &mut NativeCx<'_>,
            name: Value,
            output: Value,
            args: RestArgs<'_>,
        ) -> NativeResult<NativeStep> {
            let flag = |index: usize| -> NativeResult<bool> {
                Ok(args
                    .get(index)
                    .map(|&v| value::to_integer(cx.heap(), v))
                    .transpose()?
                    .unwrap_or(0)
                    != 0)
            };
            let result_needed = flag(0)?;
            let debug = flag(1)?;
            let expression = flag(2)?;
            let output = units(cx, output)?;
            let output = Some(tjs_core::CompileOutput {
                name: output,
                debug,
            });
            storage(cx, name, &[], expression, result_needed, output)
        }
        #[tjs::method(name = "setCallMissing", class_only = true)]
        fn set_call_missing(cx: &mut NativeCx<'_>, object: Value) -> NativeResult<()> {
            let Value::Obj(reference) = object else {
                return Err(NativeError::Type("an object"));
            };
            if let Some(object) = reference.object {
                cx.heap_mut().set_call_missing(object)?;
            }
            Ok(())
        }
        #[tjs::method(name = "getClassNames", class_only = true)]
        fn get_class_names(cx: &mut NativeCx<'_>, object: Value) -> NativeResult<Value> {
            let object = context(Some(&object))?.ok_or(NativeError::This)?;
            let names = cx.heap().class_names(object)?;
            let values = names
                .into_iter()
                .map(|name| Value::Str(cx.heap_mut().alloc_string(name)))
                .collect::<Vec<_>>();
            Ok(Value::Obj(tjs_core::ObjRef::bound(
                cx.heap_mut().alloc_array_from(&values)?,
            )))
        }
        #[tjs::getter(name = "textEncoding", class_only = true)]
        fn encoding(&self, cx: &mut NativeCx<'_>) -> Value {
            Value::Str(cx.heap_mut().alloc_string(self.encoding.clone()))
        }
        #[tjs::setter(name = "textEncoding", class_only = true)]
        fn set_encoding(&mut self, cx: &mut NativeCx<'_>, encoding: Value) -> NativeResult<()> {
            self.encoding = units(cx, encoding)?;
            Ok(())
        }
    }
}

pub(crate) fn evaluate_expression(
    cx: &mut NativeCx<'_>,
    source: Value,
) -> NativeResult<NativeStep> {
    evaluate(cx, source, &[], true, true)
}
fn evaluate(
    cx: &mut NativeCx<'_>,
    source: Value,
    args: &[Value],
    expression: bool,
    result_needed: bool,
) -> NativeResult<NativeStep> {
    let source = units(cx, source)?;
    let name = match args.first().copied().filter(|v| !matches!(v, Value::Void)) {
        Some(name) => String::from_utf16_lossy(&units(cx, name)?),
        None => String::new(),
    };
    let line_offset = match args.get(1).copied().filter(|v| !matches!(v, Value::Void)) {
        Some(value) => value::to_integer(cx.heap(), value)? as i32,
        None => 0,
    };
    Ok(NativeStep::Evaluate {
        request: CompileRequest {
            source: tjs_core::ScriptSource::Text(Arc::from(source)),
            output: None,
            expression,
            result_needed,
            name,
            line_offset,
        },
        context: context(args.get(2))?,
    })
}

fn storage(
    cx: &mut NativeCx<'_>,
    name: Value,
    args: &[Value],
    expression: bool,
    result_needed: bool,
    output: Option<tjs_core::CompileOutput>,
) -> NativeResult<NativeStep> {
    let name = units(cx, name)?;
    let mode = match args.first().copied().filter(|v| !matches!(v, Value::Void)) {
        Some(mode) => units(cx, mode)?,
        None => Vec::new(),
    };
    let class = cx
        .heap()
        .registered_class("Scripts")
        .expect("installed Scripts");
    let (encoding, operations) = cx
        .heap_mut()
        .with_native_state::<implementation::State, _>(class, |state| {
            (state.encoding.clone(), state.operations.clone())
        })?;
    let load = Loaded {
        name: String::from_utf16_lossy(&name),
        expression,
        result_needed,
        context: context(args.get(1))?,
        output,
        delivery: None,
    };
    if cx.heap().registered_class("Storages").is_some() {
        return crate::storages::managed::plans(
            cx,
            vec![(name, true)],
            ManagedScript {
                load,
                encoding,
                mode,
                operations,
            },
            |load, cx, mut plans| {
                load.open(cx, plans.pop().flatten().expect("required script plan"))
            },
        );
    }
    let storage = cx.heap_mut().storage()?;
    let limit = storage.max_read_bytes();
    let bytes = storage.read_binary(&name, &mode)?;
    let source = if tjs_runtime::bytecode::is_bytecode(&bytes) {
        tjs_core::ScriptSource::Bytecode(Arc::from(bytes))
    } else {
        tjs_core::ScriptSource::Text(Arc::from(
            krkr_assets::text::decode(&bytes, &encoding, limit)
                .map_err(|error| NativeError::Detail(error.to_string()))?,
        ))
    };
    load.evaluate(cx, source)
}

struct Loaded {
    name: String,
    expression: bool,
    result_needed: bool,
    context: Option<ObjId>,
    output: Option<tjs_core::CompileOutput>,
    delivery: Option<crate::io::Delivery>,
}
struct ManagedScript {
    load: Loaded,
    encoding: Vec<u16>,
    mode: Vec<u16>,
    operations: Option<crate::operations::Shared>,
}
impl tjs_core::Trace for ManagedScript {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.load.trace(visit);
    }
}
impl ManagedScript {
    fn open(
        self,
        cx: &mut NativeCx<'_>,
        plan: crate::storages::ReadPlan,
    ) -> NativeResult<NativeStep> {
        let Self {
            mut load,
            encoding,
            mode,
            operations,
        } = self;
        let limit = crate::storages::service(cx)?
            .borrow()
            .limits()
            .max_read_bytes;
        let offset = krkr_assets::text::offset(&mode)
            .map_err(|e| NativeError::Detail(e.to_string()))?
            .unwrap_or(0);
        if let Some(operations) = operations {
            let read = crate::io::Read {
                plan,
                offset,
                encoding,
                limit,
            };
            let delivery = crate::io::Delivery::default();
            load.delivery = Some(delivery.clone());
            crate::operations::Operations::wait(
                &operations,
                crate::operations::Request::Read(Box::new(read.into()), delivery),
                tjs_core::WaitMode::Internal,
                Box::new(load),
            )
        } else {
            let bytes = plan
                .read(offset)
                .map_err(|e| NativeError::Detail(e.to_string()))?;
            let source = if tjs_runtime::bytecode::is_bytecode(&bytes) {
                tjs_core::ScriptSource::Bytecode(Arc::from(bytes))
            } else {
                tjs_core::ScriptSource::Text(Arc::from(
                    krkr_assets::text::decode(&bytes, &encoding, limit)
                        .map_err(|e| NativeError::Detail(e.to_string()))?,
                ))
            };
            load.evaluate(cx, source)
        }
    }
}
impl Loaded {
    fn evaluate(
        self,
        cx: &mut NativeCx<'_>,
        source: tjs_core::ScriptSource,
    ) -> NativeResult<NativeStep> {
        hooks::evaluate(
            cx,
            CompileRequest {
                source,
                output: self.output,
                expression: self.expression,
                result_needed: self.result_needed,
                name: self.name,
                line_offset: 0,
            },
            self.context,
        )
    }
}
impl tjs_core::Trace for Loaded {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(context) = self.context {
            visit(Value::Obj(context.into()));
        }
    }
}
impl tjs_core::NativeContinuation for Loaded {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let data = self
            .delivery
            .as_ref()
            .expect("async script delivery")
            .borrow_mut()
            .take()
            .ok_or(NativeError::Message("script IO completed without data"))?;
        let source = match data {
            crate::io::Data::Text(text) => tjs_core::ScriptSource::Text(Arc::from(text)),
            crate::io::Data::Bytecode(bytes) => tjs_core::ScriptSource::Bytecode(Arc::from(bytes)),
            _ => return Err(NativeError::Message("unexpected script read response")),
        };
        self.evaluate(cx, source)
    }
}

pub(crate) fn attach(heap: &mut Heap, operations: crate::operations::Shared) -> NativeResult<()> {
    if let Some(class) = heap.registered_class("Scripts") {
        heap.with_native_state::<implementation::State, _>(class, |state| {
            state.operations = Some(operations)
        })?;
    }
    Ok(())
}

pub fn install(heap: &mut Heap) -> NativeResult<ObjId> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    Ok(class)
}

pub(crate) fn reset(heap: &mut Heap) {
    if let Some(class) = heap.registered_class("Scripts") {
        heap.with_native_state::<implementation::State, _>(class, |state| {
            state.after_load = hooks::Registry::default();
        })
        .expect("installed Scripts state");
    }
}

/// Shared text-stream encoding for portable providers that read script data.
pub fn text_encoding(heap: &mut Heap) -> NativeResult<Vec<u16>> {
    let class = heap
        .registered_class("Scripts")
        .ok_or(NativeError::Message("Scripts is not installed"))?;
    heap.with_native_state::<implementation::State, _>(class, |state| state.encoding.clone())
}
/// Shared default used by script reads and plugins that open text streams.
pub fn set_text_encoding(heap: &mut Heap, encoding: Vec<u16>) -> NativeResult<()> {
    let class = heap.registered_class("Scripts").ok_or(NativeError::This)?;
    heap.with_native_state::<implementation::State, _>(class, |state| state.encoding = encoding)
}

enum Inspected {
    Return,
    Log(Value),
}
impl tjs_core::Trace for Inspected {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Self::Log(value) = self {
            visit(*value);
        }
    }
}
impl tjs_core::NativeContinuation for Inspected {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, result: Value) -> NativeResult<NativeStep> {
        Ok(match *self {
            Self::Return => NativeStep::Return(result),
            Self::Log(function) => NativeStep::Call {
                function,
                arguments: vec![result],
                continuation: Box::new(Self::Return),
            },
        })
    }
}

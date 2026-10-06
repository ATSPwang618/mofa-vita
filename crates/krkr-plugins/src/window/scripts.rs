//! windowEx eval switch: preserve expressions, source names and line offsets.
use crate::exports::{Exports, arg, class};
use krkr_engine::{extensions, plugins::Context};
use std::sync::Arc;
use tjs_core::{
    CompileRequest, NativeCallable, NativeContinuation, NativeCx, NativeResult, NativeStep,
    NativeTryContinuation, Value, value,
};

#[derive(Default, tjs_bind::Trace)]
struct State {
    original: Value,
    enabled: bool,
}
pub(super) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let scripts = class(cx, "Scripts")?;
    let key = cx.heap.intern(&"eval".encode_utf16().collect::<Vec<_>>());
    let original = cx.heap.member(scripts, key)?.unwrap_or(Value::Void);
    cx.heap.initialize_class_state::<State>(scripts)?;
    cx.heap.with_native_state::<State, _>(scripts, |s| {
        s.original = original;
        s.enabled = true;
    })?;
    exports.function(cx, scripts, "eval", NativeCallable::Resumable(eval))?;
    exports.function(
        cx,
        scripts,
        "setEvalErrorLog",
        NativeCallable::Leaf(set_log),
    )
}
fn set_log(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let enabled = value::to_integer(cx.heap(), arg(args, 0)?)? != 0;
    let scripts = cx.heap().registered_class("Scripts").unwrap();
    let previous = cx
        .heap_mut()
        .with_native_state::<State, _>(scripts, |s| std::mem::replace(&mut s.enabled, enabled))?;
    Ok(Value::Int(previous.into()))
}
fn eval(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    arg(args, 0)?;
    let scripts = cx.heap().registered_class("Scripts").unwrap();
    let (enabled, original) = cx
        .heap_mut()
        .with_native_state::<State, _>(scripts, |s| (s.enabled, s.original))?;
    Ok(NativeStep::Try {
        task: Box::new(Eval {
            original: enabled.then_some(original),
            args: args.to_vec(),
            result_needed: cx.result_needed(),
        }),
        continuation: Box::new(Finished(enabled)),
    })
}
#[derive(tjs_bind::Trace)]
struct Eval {
    original: Option<Value>,
    args: Vec<Value>,
    result_needed: bool,
}
fn text(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = value::to_string(cx.heap_mut(), value)? else {
        unreachable!()
    };
    Ok(tjs_core::string::c_string(cx.heap().string(id)?).to_vec())
}
impl NativeContinuation for Eval {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if let Some(original) = self.original {
            return Ok(if self.result_needed {
                NativeStep::Call {
                    function: original,
                    arguments: self.args,
                    continuation: tjs_bind::flow::identity(),
                }
            } else {
                NativeStep::CallDiscard {
                    function: original,
                    arguments: self.args,
                    continuation: tjs_bind::flow::identity(),
                }
            });
        }
        let source = text(cx, self.args[0])?;
        let name = self
            .args
            .get(1)
            .map(|&v| text(cx, v))
            .transpose()?
            .unwrap_or_default();
        let line_offset = self
            .args
            .get(2)
            .map(|&v| value::to_integer(cx.heap(), v))
            .transpose()?
            .unwrap_or(0) as i32;
        Ok(NativeStep::Evaluate {
            request: CompileRequest {
                source: tjs_core::ScriptSource::Text(Arc::from(source)),
                output: None,
                expression: true,
                result_needed: self.result_needed,
                name: String::from_utf16_lossy(&name),
                line_offset,
            },
            context: None,
        })
    }
}
#[derive(tjs_bind::Trace)]
struct Finished(bool);
impl NativeTryContinuation for Finished {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        match result {
            Ok(value) => Ok(NativeStep::Return(value)),
            Err(error) if self.0 => extensions::log_eval_error(cx, error, Box::new(Rethrow(error))),
            Err(error) => Ok(NativeStep::Throw(error)),
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Rethrow(Value);
impl NativeContinuation for Rethrow {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Throw(self.0))
    }
}

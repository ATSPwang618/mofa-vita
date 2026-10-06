use krkr_engine::plugins::Context;
pub(crate) use krkr_engine::plugins::Exports;
use tjs_core::{NativeResult, ObjId, Value};

pub(crate) fn class(cx: &mut Context<'_>, name: &str) -> NativeResult<ObjId> {
    cx.heap
        .registered_class(name)
        .ok_or(tjs_core::NativeError::Message(
            "required engine class is not installed",
        ))
}

pub(crate) fn arg(args: &[Value], index: usize) -> NativeResult<Value> {
    args.get(index)
        .copied()
        .ok_or(tjs_core::NativeError::Missing(index))
}
pub(crate) fn object(value: Value) -> NativeResult<ObjId> {
    if let Value::Obj(value) = value {
        value
            .object
            .ok_or(tjs_core::NativeError::Type("a non-null object"))
    } else {
        Err(tjs_core::NativeError::Type("an object"))
    }
}

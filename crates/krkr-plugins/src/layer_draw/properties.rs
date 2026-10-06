use tjs_core::{NativeCx, NativeResult, NativeStep, Trace, Value};
pub fn get<S: Trace + 'static>(
    cx: &mut NativeCx<'_>,
    object: Value,
    name: &str,
    fallback: Value,
    state: S,
    next: fn(S, &mut NativeCx<'_>, Value) -> NativeResult<NativeStep>,
) -> NativeResult<NativeStep> {
    let key = Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    );
    Ok(NativeStep::GetOr {
        object,
        key,
        raw: false,
        fallback,
        continuation: tjs_bind::flow::callback(state, next),
    })
}
pub fn index<S: Trace + 'static>(
    object: Value,
    key: i64,
    fallback: Value,
    state: S,
    next: fn(S, &mut NativeCx<'_>, Value) -> NativeResult<NativeStep>,
) -> NativeStep {
    NativeStep::GetOr {
        object,
        key: Value::Int(key),
        raw: false,
        fallback,
        continuation: tjs_bind::flow::callback(state, next),
    }
}

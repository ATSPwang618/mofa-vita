//! Complete saveStruct.dll surface, krkr2@dca72645/cpp/plugins/saveStruct.cpp.
mod writer;

use crate::exports::{arg, class};
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeResult, NativeStep, ObjRef, Value};

#[derive(tjs_bind::Trace)]
struct Count(Value);
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Root {
    Array,
    Dictionary,
    Lines,
}
krkr_engine::native_plugin! {
    pub(crate) SaveStruct {
        names: ["saveStruct.dll", "saveStruct.tpm"],
        link(cx, exports) {
        let array = class(cx, "Array")?;
        let dictionary = class(cx, "Dictionary")?;
        let key = cx.heap.intern(&[99, 111, 117, 110, 116]);
        let count = cx.heap.member(array, key)?;
        let Some(Value::Obj(ObjRef {
            object: Some(count),
            ..
        })) = count
        else {
            return Err(NativeError::Type("an Array.count property object"));
        };
        for (owner, name, call, class_only) in [
            (array, "save2", NativeCallable::Resumable(save_lines), false),
            (
                array,
                "saveStruct2",
                NativeCallable::Resumable(save_array),
                false,
            ),
            (
                array,
                "toStructString",
                NativeCallable::Resumable(array_string),
                false,
            ),
            (
                dictionary,
                "saveStruct2",
                NativeCallable::Resumable(save_dictionary),
                true,
            ),
            (
                dictionary,
                "toStructString",
                NativeCallable::Resumable(dictionary_string),
                true,
            ),
        ] {
            // Each loaded generation owns its capture. Retained methods keep
            // working after unlink/relink without rooting it on a global class.
            exports.captured_function(
                cx,
                owner,
                name,
                call,
                Count(Value::Obj(count.into())),
                class_only,
            )?;
        }
        Ok(())
        }
    }
}
fn start(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    root: Root,
    file: bool,
) -> NativeResult<NativeStep> {
    if !file && !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    let path = if file { Some(arg(args, 0)?) } else { None };
    let function = cx
        .function()
        .ok_or(NativeError::Message("missing saveStruct callable"))?;
    let api = cx
        .heap_mut()
        .with_native_state::<Count, _>(function, |s| s.0)?;
    // The reference's utf argument is commented out, including its conversion.
    writer::start(
        cx,
        root,
        api,
        path,
        args.get(if file { 2 } else { 0 }).copied(),
    )
}
fn save_lines(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    start(cx, args, Root::Lines, true)
}
fn save_array(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    start(cx, args, Root::Array, true)
}
fn save_dictionary(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    start(cx, args, Root::Dictionary, true)
}
fn array_string(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    start(cx, args, Root::Array, false)
}
fn dictionary_string(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    start(cx, args, Root::Dictionary, false)
}

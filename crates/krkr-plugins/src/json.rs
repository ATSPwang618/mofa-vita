//! Go Watanabe's JSON plugin, krkrz/krkrz@49c4d53506edecb824cd7b2cff8d32959b1f1b70,
//! src/plugins/win32/json/{Main.cpp,Writer.hpp}. This is its permissive TJS
//! format, including raw property enumeration, rather than serde's JSON model.
mod parser;
mod writer;

use crate::exports::{arg, class};
use krkr_engine::storages;
use tjs_core::{
    Heap, NativeCallable, NativeCx, NativeError, NativeResult, NativeStep, ObjRef, Value, value,
};

const LIMIT: usize = 16 * 1024 * 1024;
const DEPTH: usize = 128;

#[derive(Clone, tjs_bind::Trace)]
struct ArrayApi(Value);
krkr_engine::native_plugin! {
    pub(crate) Json {
        names: ["json.dll", "json.tpm"],
        link(cx, exports) {
        let scripts = class(cx, "Scripts")?;
        let array = class(cx, "Array")?;
        let key = cx.heap.intern(&[99, 111, 117, 110, 116]);
        let count = cx
            .heap
            .member(array, key)?
            .ok_or(NativeError::Message("can't get member:count"))?;
        if !matches!(
            count,
            Value::Obj(ObjRef {
                object: Some(_),
                ..
            })
        ) {
            return Err(NativeError::Type("an Array.count property object"));
        }
        cx.heap.initialize_native_state(scripts, ArrayApi(count))?;
        cx.heap
            .with_native_state::<ArrayApi, _>(scripts, |api| api.0 = count)?;
        for (name, function) in [
            ("evalJSON", parse::CALL),
            ("evalJSONStorage", NativeCallable::Resumable(read)),
            ("toJSONString", stringify::CALL),
            ("saveJSON", NativeCallable::Resumable(save)),
        ] {
            exports.function(cx, scripts, name, function)?;
        }
        Ok(())
        }
    }
}
fn string_arg(heap: &Heap, input: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = input else {
        return Err(NativeError::Type("a string"));
    };
    Ok(tjs_core::string::c_string(heap.string(id)?).to_vec())
}
#[tjs_bind::function]
fn parse(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<Value> {
    let text = string_arg(cx.heap(), arg(args, 0)?)?;
    if !cx.result_needed() {
        return Ok(Value::Void);
    }
    parser::parse(cx.heap_mut(), &text)
}
fn read(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let path = string_arg(cx.heap(), arg(args, 0)?)?;
    storages::managed::plans(
        cx,
        vec![(path, true)],
        cx.result_needed(),
        |needed, cx, mut plans| {
            read_plan(
                cx,
                plans.pop().flatten().expect("required JSON plan"),
                needed,
            )
            .map(NativeStep::Return)
        },
    )
}
fn read_plan(
    cx: &mut NativeCx<'_>,
    plan: krkr_engine::assets::ReadPlan,
    needed: bool,
) -> NativeResult<Value> {
    if !needed {
        drop(plan.open().map_err(error)?);
        return Ok(Value::Void);
    }
    if plan.bytes > LIMIT as u64 {
        return Err(NativeError::Message("JSON exceeds input size limit"));
    }
    let bytes = plan.read(0).map_err(error)?;
    // Main.cpp tests (int*)param[1], not the argument value: any second
    // argument selects UTF-8. CP_ACP is the engine's modern UTF-8 default;
    // platform process locales are deliberately not used by this portable plugin.
    // IFileStorage decodes each line separately, drops CR/LF, and appends its
    // C-string prefix. No BOM detection, TJS text decoding, or retained line
    // endings for // and # comments. Invalid UTF-8 gets Windows' replacement.
    let mut text = Vec::new();
    for line in bytes.split(|b| matches!(b, b'\r' | b'\n')) {
        let line = &line[..line.iter().position(|&b| b == 0).unwrap_or(line.len())];
        text.extend(String::from_utf8_lossy(line).encode_utf16());
    }
    parser::parse(cx.heap_mut(), &text)
}
#[tjs_bind::function(resumable = true)]
fn stringify(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
    let input = arg(args, 0)?;
    if !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    let newline =
        value::to_integer(cx.heap(), args.get(1).copied().unwrap_or(Value::Int(0)))? as i32;
    writer::serialize(cx, input, newline, None, false)
}
fn save(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = arg(args, 1)?;
    let path = string_arg(cx.heap(), arg(args, 0)?)?;
    let utf8 =
        value::to_integer(cx.heap(), args.get(2).copied().unwrap_or(Value::Int(0)))? as i32 != 0;
    let newline =
        value::to_integer(cx.heap(), args.get(3).copied().unwrap_or(Value::Int(0)))? as i32;
    // IFileWriter opens/truncates before traversal. Its destructor flushes the
    // prefix even when a getter throws; the writer preserves that ordering.
    writer::serialize(cx, input, newline, Some(path), utf8)
}
fn error(error: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(error.to_string())
}

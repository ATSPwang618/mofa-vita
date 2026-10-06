//! Only loading windowEx introduces the legacy console/Pad compatibility objects.
//! The engine's normal Z environment continues to have no Debug.console.
use crate::exports::{Exports, arg, class};
use krkr_engine::plugins::Context;
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeResult, ObjId, ObjRef, Trace, Value};

fn member(cx: &mut Context<'_>, owner: ObjId, name: &str) -> NativeResult<Option<Value>> {
    let key = cx.heap.intern(&name.encode_utf16().collect::<Vec<_>>());
    Ok(cx.heap.member(owner, key)?)
}
pub(super) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let debug = class(cx, "Debug")?;
    let console = match member(cx, debug, "console")? {
        Some(Value::Obj(ObjRef {
            object: Some(id), ..
        })) => id,
        None | Some(Value::Void) => {
            let console = cx.heap.alloc_dictionary();
            exports.value(cx, debug, "console", Value::Obj(ObjRef::bound(console)))?;
            console
        }
        _ => return Err(NativeError::Type("Debug.console object")),
    };
    for (name, call) in [
        (
            "restoreMaximize",
            absent as fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<Value>,
        ),
        ("maximize", absent),
        ("getRect", void),
        ("getPlacement", void),
        ("bringAfter", void),
        ("setPos", set_pos),
        ("setPlacement", set_placement),
    ] {
        exports.function(cx, console, name, NativeCallable::Leaf(call))?;
    }
    // Upstream creates a plain object on Z, not a fake constructible Pad class.
    let pad = match member(cx, cx.global, "Pad")? {
        Some(Value::Obj(ObjRef {
            object: Some(id), ..
        })) => id,
        None | Some(Value::Void) => {
            let pad = cx.heap.alloc_dictionary();
            exports.value(cx, cx.global, "Pad", Value::Obj(ObjRef::bound(pad)))?;
            pad
        }
        _ => return Err(NativeError::Type("Pad object")),
    };
    exports.function(
        cx,
        pad,
        "registerExEvent",
        NativeCallable::Leaf(register_pad),
    )
}
fn void(_: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    Ok(Value::Void)
}
fn absent(_: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    Ok(Value::Int(0))
}
fn set_pos(_: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    arg(args, 1)?;
    Ok(Value::Void)
}
fn set_placement(_: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    if !matches!(arg(args, 0)?, Value::Obj(_)) {
        return Err(NativeError::Type("a placement object"));
    }
    Ok(Value::Int(0))
}
#[derive(Default)]
struct PadEvents {
    registered: bool,
}
impl Trace for PadEvents {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
fn register_pad(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    let owner = cx.this();
    cx.heap_mut()
        .initialize_native_default::<PadEvents>(owner)?;
    cx.heap_mut()
        .with_native_state::<PadEvents, _>(owner, |s| s.registered = true)?;
    // Registration records intent; no native tool window exists to emit close.
    Ok(Value::Void)
}

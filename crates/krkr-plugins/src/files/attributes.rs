use super::{error, path};
use crate::exports::arg;
use krkr_engine::{assets::local, system::files};
use tjs_core::{NativeCx, NativeResult, Value, value};

pub(super) const MASK: u32 = 0x1a7;
pub(super) fn get(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let path = local::units(&path(cx, arg(args, 0)?)?).map_err(error)?;
    Ok(Value::Int(i64::from(files::attributes(cx, &path)?)))
}
pub(super) fn set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    change(cx, args, true)
}
pub(super) fn reset(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    change(cx, args, false)
}
fn change(cx: &mut NativeCx<'_>, args: &[Value], set: bool) -> NativeResult<Value> {
    let filename = arg(args, 0)?;
    let mask = value::to_integer(cx.heap(), arg(args, 1)?)? as u32 & MASK;
    let path = local::units(&path(cx, filename)?).map_err(error)?;
    Ok(Value::Int(i64::from(files::change_attributes(
        cx, &path, mask, set,
    )?)))
}

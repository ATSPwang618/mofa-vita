use super::{error, path};
use crate::exports::arg;
use krkr_engine::{
    assets::{Stream, name},
    storages,
};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
};
use tjs_core::{NativeContinuation, NativeCx, NativeResult, NativeStep, Trace, Value, value};

struct Export {
    source: Box<dyn Stream>,
    target: File,
    vfs: storages::Shared,
}
impl Trace for Export {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Export {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let mut bytes = [0; 64 * 1024];
        let count = self.source.read(&mut bytes).map_err(error)?;
        if count == 0 {
            self.target.flush().map_err(error)?;
            return Ok(NativeStep::Return(Value::Void));
        }
        self.target.write_all(&bytes[..count]).map_err(error)?;
        self.vfs.borrow_mut().clear_archive_cache();
        Ok(NativeStep::Continue(self))
    }
}
pub(super) fn export(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let from = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    let to = arg(args, 1)?;
    // Convert both arguments before IO, then open input before truncating output.
    let target = path(cx, to)?;
    storages::managed::plans(
        cx,
        vec![(name::c_string(&from).to_vec(), true)],
        Destination(target),
        |target, cx, mut plans| {
            export_plan(cx, target.0, plans.pop().flatten().expect("export source"))
        },
    )
}
struct Destination(std::path::PathBuf);
impl Trace for Destination {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
fn export_plan(
    cx: &mut NativeCx<'_>,
    target: std::path::PathBuf,
    plan: krkr_engine::assets::ReadPlan,
) -> NativeResult<NativeStep> {
    let vfs = storages::service(cx)?;
    let source = plan.open().map_err(error)?;
    let target = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(target)
        .map_err(error)?;
    vfs.borrow_mut().clear_archive_cache();
    Ok(NativeStep::Continue(Box::new(Export {
        source,
        target,
        vfs,
    })))
}

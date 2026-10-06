use crate::exports::{Exports, class};
use krkr_engine::{extensions, plugins::Context};
use tjs_core::{NativeCallable, NativeCx, NativeResult, NativeStep, Value};

pub(super) fn install(exports: &mut Exports, cx: &mut Context<'_>) -> NativeResult<()> {
    let window = class(cx, "Window")?;
    exports.function(cx, window, "setWindowIcon", NativeCallable::Resumable(set))?;
    exports.function(
        cx,
        window,
        "resetWindowIcon",
        NativeCallable::Resumable(reset),
    )?;
    let system = class(cx, "System")?;
    exports.function(
        cx,
        system,
        "setApplicationIcon",
        NativeCallable::Resumable(application),
    )
}
fn set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let with_application = args
        .get(1)
        .copied()
        .unwrap_or(Value::Void)
        .truthy(cx.heap())?;
    extensions::set_window_icon(cx, args.first().copied(), with_application)
}
fn reset(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<NativeStep> {
    extensions::reset_window_icon(cx)
}
fn application(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    extensions::set_application_icon(cx, args.first().copied())
}

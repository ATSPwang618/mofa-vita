use crate::exports::{arg, class};
mod icons;
use extensions::{WindowProperty, WindowRectangle};
use krkr_engine::extensions;
use krkr_engine::protocol::window::Control;
use tjs_core::{NativeCallable, NativeCx, NativeProperty, NativeResult, NativeStep, Value, value};
mod environment;
mod legacy;
mod lookup;
mod names;
mod scripts;
mod system;
pub(crate) use system::about;
mod tools;

krkr_engine::native_plugin! {
    pub(crate) Window {
        names: ["windowEx.dll", "windowEx.tpm"],
        link(cx, exports) {
        let window = class(cx, "Window")?;
        exports.function(
            cx,
            window,
            "registerExEvent",
            register::CALL,
        )?;
        for (name, call) in [
            ("minimize", minimize::CALL),
            ("maximize", maximize::CALL),
            ("showRestore", restore::CALL),
            ("getWindowRect", window_rect::CALL),
            ("getClientRect", client_rect::CALL),
            ("setClientRect", set_client_rect::CALL),
            ("getNormalRect", normal_rect::CALL),
            ("bringTo", bring_to::CALL),
            ("sendToBack", send_back::CALL),
        ] {
            exports.function(cx, window, name, call)?;
        }
        for property in PROPERTIES {
            exports.property(cx, window, property)?;
        }
        environment::install(cx, exports)?;
        icons::install(exports, cx)?;
        system::install(cx, exports)?;
        scripts::install(cx, exports)?;
        legacy::install(cx, exports)?;
        Ok(())
        }
    }
}
#[tjs_bind::function(resumable = true)]
fn register(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let owner = cx.this();
    extensions::window_id(cx, Value::Obj(owner.into()))?;
    let missing = Value::Obj(tjs_core::ObjRef::bound(cx.heap_mut().alloc_dictionary()));
    Register {
        owner,
        missing,
        index: 0,
        has_move: false,
    }
    .next(cx)
}
#[derive(tjs_bind::Trace)]
struct Register {
    owner: tjs_core::ObjId,
    missing: Value,
    index: usize,
    has_move: bool,
}
impl Register {
    fn next(self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index == 4 {
            extensions::register_window_events(cx.heap_mut(), self.owner, self.has_move)?;
            return Ok(NativeStep::Return(Value::Void));
        }
        let key = Value::Str(
            cx.heap_mut().alloc_string(
                ["onResizing", "onMoving", "onMove", "onNcMouseMove"][self.index]
                    .encode_utf16()
                    .collect::<Vec<_>>(),
            ),
        );
        Ok(NativeStep::GetRequiredOr {
            object: Value::Obj(tjs_core::ObjRef::bound(self.owner)),
            key,
            fallback: self.missing,
            continuation: Box::new(self),
        })
    }
}
impl tjs_core::NativeContinuation for Register {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if self.index == 2 {
            self.has_move = !value::strict_equal(cx.heap(), value, self.missing)?;
        }
        self.index += 1;
        self.next(cx)
    }
}

#[tjs_bind::function(resumable = true)]
fn minimize(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    extensions::window_control(cx, Control::Minimize)
}
#[tjs_bind::function(resumable = true)]
fn maximize(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    extensions::window_control(cx, Control::Maximize)
}
#[tjs_bind::function(resumable = true)]
fn restore(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    extensions::window_control(cx, Control::Restore)
}
#[tjs_bind::function(resumable = true)]
fn window_rect(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    extensions::window_rectangle(cx, WindowRectangle::Window)
}
#[tjs_bind::function(resumable = true)]
fn client_rect(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    extensions::window_rectangle(cx, WindowRectangle::Client)
}
#[tjs_bind::function(resumable = true)]
fn set_client_rect(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<NativeStep> {
    extensions::set_client_rect(cx, arg(args, 0)?)
}
#[tjs_bind::function(resumable = true)]
fn normal_rect(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
    let nofix = args
        .first()
        .map(|v| value::to_integer(cx.heap(), *v))
        .transpose()?
        .unwrap_or(0)
        != 0;
    extensions::window_rectangle(cx, WindowRectangle::Normal { nofix })
}
#[tjs_bind::function(resumable = true)]
fn bring_to(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<NativeStep> {
    let activate = args
        .get(1)
        .map(|v| value::to_integer(cx.heap(), *v))
        .transpose()?
        .unwrap_or(0)
        != 0;
    extensions::window_z_order(cx, args.first().copied(), activate)
}
#[tjs_bind::function(resumable = true)]
fn send_back(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    extensions::window_z_order(cx, Some(Value::Int(1)), false)
}
macro_rules! property {
    ($get:ident, $set:ident, $property:ident) => {
        fn $get(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<NativeStep> {
            extensions::window_get(cx, WindowProperty::$property)
        }
        fn $set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
            let enabled = value::to_integer(cx.heap(), arg(args, 0)?)? != 0;
            extensions::window_set(cx, WindowProperty::$property, enabled)
        }
    };
}
property!(get_maximized, set_maximized, Maximized);
property!(get_minimized, set_minimized, Minimized);
property!(get_maximize_box, set_maximize_box, MaximizeBox);
property!(get_minimize_box, set_minimize_box, MinimizeBox);
property!(get_disable_resize, set_disable_resize, DisableResize);
property!(get_disable_move, set_disable_move, DisableMove);
const fn descriptor(
    name: &'static str,
    get: fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<NativeStep>,
    set: fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<NativeStep>,
) -> NativeProperty {
    NativeProperty {
        hidden: false,
        class_only: false,
        name,
        doc: "windowEx portable window state",
        get: Some(NativeCallable::Resumable(get)),
        set: Some(NativeCallable::Resumable(set)),
    }
}
static PROPERTIES: &[NativeProperty] = &[
    descriptor("maximized", get_maximized, set_maximized),
    descriptor("minimized", get_minimized, set_minimized),
    descriptor("maximizeBox", get_maximize_box, set_maximize_box),
    descriptor("minimizeBox", get_minimize_box, set_minimize_box),
    descriptor("disableResize", get_disable_resize, set_disable_resize),
    descriptor("disableMove", get_disable_move, set_disable_move),
];

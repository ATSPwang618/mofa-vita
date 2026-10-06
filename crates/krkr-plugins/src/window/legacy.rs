//! Deprecated script compatibility. No native chrome, handles or message ABI.
use crate::exports::{Exports, arg, class, object};
use krkr_engine::{extensions, plugins::Context, protocol::window::desktop::Command};
use tjs_core::{
    NativeCallable, NativeCx, NativeError, NativeProperty, NativeResult, NativeStep, Trace, Value,
    value,
};

type PropertyCall = fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<Value>;

#[derive(Default)]
struct WindowState {
    menu: Value,
    overlay: Value,
    nc_mouse: bool,
}
impl Trace for WindowState {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.menu.trace(visit);
        self.overlay.trace(visit);
    }
}
fn window_state<R>(
    cx: &mut NativeCx<'_>,
    f: impl FnOnce(&mut WindowState) -> R,
) -> NativeResult<R> {
    let owner = cx.this();
    extensions::window_id(cx, Value::Obj(owner.into()))?;
    cx.heap_mut()
        .initialize_native_default::<WindowState>(owner)?;
    cx.heap_mut().with_native_state::<WindowState, _>(owner, f)
}
fn menu_get(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    window_state(cx, |s| {
        if matches!(s.menu, Value::Void) {
            Value::Obj(Default::default())
        } else {
            s.menu
        }
    })
}
fn menu_set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = arg(args, 0)?;
    let Value::Obj(reference) = value else {
        return Err(NativeError::Type("a menu object or null"));
    };
    let value = Value::Obj(
        reference
            .object
            .map(tjs_core::ObjRef::bound)
            .unwrap_or_default(),
    );
    window_state(cx, |s| s.menu = value)?;
    Ok(Value::Void)
}
fn nc_get(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    window_state(cx, |s| Value::Int(s.nc_mouse.into()))
}
fn nc_set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = value::to_integer(cx.heap(), arg(args, 0)?)? != 0;
    window_state(cx, |s| s.nc_mouse = value)?;
    Ok(Value::Void)
}
fn overlay(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = args.first().copied().unwrap_or(Value::Void);
    window_state(cx, |s| s.overlay = Value::Void)?;
    if let Value::Obj(_) = value {
        let id = object(value)?;
        if !cx
            .heap()
            .class_names(id)?
            .iter()
            .any(|name| name.iter().copied().eq("Layer".encode_utf16()))
        {
            return Err(NativeError::Type("a Layer object"));
        }
    }
    // The native overlay is excluded; retain the assigned managed source only.
    window_state(cx, |s| {
        s.overlay = if matches!(value, Value::Obj(_)) {
            value
        } else {
            Value::Void
        }
    })?;
    Ok(Value::Void)
}
fn window_no_ui(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    window_state(cx, |_| Value::Void)
}
fn hit_test(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    value::to_integer(cx.heap(), arg(args, 0)?)?;
    value::to_integer(cx.heap(), arg(args, 1)?)?;
    window_state(cx, |_| ())?;
    // HTERROR: no non-client hit-test service, never a fabricated client hit.
    Ok(Value::Int(-2))
}
fn message_hook(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    window_state(cx, |_| ())?;
    if let Some(&enabled) = args.first() {
        value::to_integer(cx.heap(), enabled)?;
    }
    if let Some(&key) = args.get(1) {
        if matches!(key, Value::Str(_)) {
            return Ok(NativeStep::Try {
                task: Box::new(HookLookup(key)),
                continuation: Box::new(HookChecked),
            });
        }
        validate_hook(value::to_integer(cx.heap(), key)? as i32)?;
    }
    Ok(NativeStep::Return(Value::Int(0)))
}
fn validate_hook(number: i32) -> NativeResult<()> {
    if !(0..0x400).contains(&number) {
        Err(NativeError::Message("invalid window notification number"))
    } else {
        Ok(())
    }
}
struct HookLookup(Value);
impl Trace for HookLookup {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.0.trace(visit);
    }
}
impl tjs_core::NativeContinuation for HookLookup {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        super::lookup::lookup(cx, false, self.0, false)
    }
}
struct HookChecked;
impl Trace for HookChecked {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl tjs_core::NativeTryContinuation for HookChecked {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        match result {
            Err(error) => Ok(NativeStep::Throw(error)),
            Ok(value) => {
                validate_hook(value::to_integer(cx.heap(), value)? as i32)?;
                Ok(NativeStep::Return(Value::Int(0)))
            }
        }
    }
}
fn notification_number(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let Value::Str(key) = value::to_string(cx.heap_mut(), arg(args, 0)?)? else {
        unreachable!()
    };
    let key = tjs_core::string::c_string(cx.heap().string(key)?).to_vec();
    let key = Value::Str(cx.heap_mut().alloc_string(key));
    super::lookup::lookup(cx, false, key, false)
}
fn notification_name(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let number = value::to_integer(cx.heap(), arg(args, 0)?)? as i32;
    let key = Value::Str(
        cx.heap_mut()
            .alloc_string(number.to_string().encode_utf16().collect::<Vec<_>>()),
    );
    super::lookup::lookup(cx, false, key, true)
}
fn device_change(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    window_state(cx, |_| ())?;
    if let Some(Value::Octet(id)) = args.first()
        && cx.heap().octet(*id)?.len() != 16
    {
        return Err(NativeError::Message("device class GUID requires 16 bytes"));
    }
    Err(NativeError::Message(
        "native device-interface notifications are not provided",
    ))
}
fn hotkey(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    window_state(cx, |_| ())?;
    let id = value::to_integer(cx.heap(), arg(args, 0)?)? as i32;
    if !(0..0xc000).contains(&id) {
        return Err(NativeError::Message("invalid hotkey identifier"));
    }
    for &value in args.iter().skip(1).take(2) {
        value::to_integer(cx.heap(), value)?;
    }
    Ok(Value::Int(0))
}
fn acquire_ime(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<NativeStep> {
    let window = extensions::window_id(cx, Value::Obj(cx.this().into()))?;
    extensions::desktop_request(
        cx,
        Command::Ime {
            window,
            enabled: true,
        },
    )
}
fn reset_ime(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let enabled = args
        .first()
        .filter(|v| !matches!(v, Value::Void))
        .map(|v| v.truthy(cx.heap()))
        .transpose()?
        .unwrap_or(true);
    let window = extensions::window_id(cx, Value::Obj(cx.this().into()))?;
    extensions::desktop_request(cx, Command::Ime { window, enabled })
}
fn corner(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let preference = args
        .first()
        .map(|&v| value::to_integer(cx.heap(), v))
        .transpose()?
        .unwrap_or(0) as u32;
    let window = extensions::window_id(cx, Value::Obj(cx.this().into()))?;
    extensions::desktop_request(cx, Command::Corner { window, preference })
}
fn no_handle(_: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    Ok(Value::Int(0))
}
fn dpi(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    value::to_integer(cx.heap(), arg(args, 0)?)?;
    Ok(Value::Int(0))
}
fn load_cursor(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = arg(args, 0)?;
    if !matches!(value, Value::Str(_)) {
        value::to_integer(cx.heap(), value)?;
    }
    if let Some(&module) = args.get(1) {
        value::to_integer(cx.heap(), module)?;
    }
    Ok(Value::Int(0))
}
fn class_long(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    arg(args, 0)?;
    let Value::Str(key) = value::to_string(cx.heap_mut(), arg(args, 1)?)? else {
        unreachable!()
    };
    let key =
        String::from_utf16_lossy(tjs_core::string::c_string(cx.heap().string(key)?)).to_uppercase();
    Ok(
        if ["CURSOR", "ICON", "ICONSM", "BRBACKGROUND"].contains(&key.as_str()) {
            Value::Int(0)
        } else {
            Value::Void
        },
    )
}

#[derive(Default)]
struct MenuState {
    right: bool,
    images: [i64; 3],
}
impl Trace for MenuState {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
fn menu_state<R>(cx: &mut NativeCx<'_>, f: impl FnOnce(&mut MenuState) -> R) -> NativeResult<R> {
    let owner = cx.this();
    if !cx
        .heap()
        .class_names(owner)?
        .iter()
        .any(|n| n.iter().copied().eq("MenuItem".encode_utf16()))
    {
        return Err(NativeError::This);
    }
    cx.heap_mut()
        .initialize_native_default::<MenuState>(owner)?;
    cx.heap_mut().with_native_state::<MenuState, _>(owner, f)
}
fn right_get(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    menu_state(cx, |s| Value::Int(s.right.into()))
}
fn right_set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let value = value::to_integer(cx.heap(), arg(args, 0)?)? != 0;
    menu_state(cx, |s| s.right = value)?;
    Ok(Value::Void)
}
fn bitmap_set(cx: &mut NativeCx<'_>, args: &[Value], index: usize) -> NativeResult<Value> {
    menu_state(cx, |s| s.images[index] = 0)?;
    let value = arg(args, 0)?;
    let number = match value {
        Value::Int(_) | Value::Str(_) | Value::Void => value::to_integer(cx.heap(), value)?,
        Value::Obj(_) => {
            let id = object(value)?;
            if !cx
                .heap()
                .class_names(id)?
                .iter()
                .any(|n| n.iter().copied().eq("Layer".encode_utf16()))
            {
                return Err(NativeError::Type("a Layer object"));
            }
            -1
        }
        _ => 0,
    };
    menu_state(cx, |s| s.images[index] = number)?;
    Ok(Value::Void)
}
macro_rules! bitmap {
    ($get:ident,$set:ident,$index:expr) => {
        fn $get(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
            menu_state(cx, |s| Value::Int(s.images[$index]))
        }
        fn $set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
            bitmap_set(cx, args, $index)
        }
    };
}
bitmap!(item_get, item_set, 0);
bitmap!(checked_get, checked_set, 1);
bitmap!(unchecked_get, unchecked_set, 2);
fn popup(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    menu_state(cx, |_| ())?;
    // Native menu selection is excluded by the frozen MenuItem contract.
    for &value in args.iter().take(3) {
        value::to_integer(cx.heap(), value)?;
    }
    Ok(Value::Int(0))
}
const fn property(
    name: &'static str,
    get: Option<PropertyCall>,
    set: Option<PropertyCall>,
) -> NativeProperty {
    NativeProperty {
        name,
        hidden: false,
        class_only: false,
        doc: "Deprecated compatibility state; no native interface is created",
        get: match get {
            Some(f) => Some(NativeCallable::Leaf(f)),
            None => None,
        },
        set: match set {
            Some(f) => Some(NativeCallable::Leaf(f)),
            None => None,
        },
    }
}
static WINDOW_PROPERTIES: &[NativeProperty] = &[
    property("exSystemMenu", Some(menu_get), Some(menu_set)),
    property("enableNCMouseEvent", Some(nc_get), Some(nc_set)),
];
static MENU_PROPERTIES: &[NativeProperty] = &[
    property("rightJustify", Some(right_get), Some(right_set)),
    property("bmpItem", Some(item_get), Some(item_set)),
    property("bmpChecked", Some(checked_get), Some(checked_set)),
    property("bmpUnchecked", Some(unchecked_get), Some(unchecked_set)),
];
pub(super) fn install(cx: &mut Context<'_>, exports: &mut Exports) -> NativeResult<()> {
    let window = class(cx, "Window")?;
    for p in WINDOW_PROPERTIES {
        exports.property(cx, window, p)?;
    }
    for (name, call) in [
        ("setOverlayBitmap", NativeCallable::Leaf(overlay)),
        ("resetExSystemMenu", NativeCallable::Leaf(window_no_ui)),
        ("focusMenuByKey", NativeCallable::Leaf(window_no_ui)),
        ("ncHitTest", NativeCallable::Leaf(hit_test)),
        ("setMessageHook", NativeCallable::Resumable(message_hook)),
        (
            "getNotificationNum",
            NativeCallable::Resumable(notification_number),
        ),
        (
            "getNotificationName",
            NativeCallable::Resumable(notification_name),
        ),
        ("registerDeviceChange", NativeCallable::Leaf(device_change)),
        ("registerHotKey", NativeCallable::Leaf(hotkey)),
        ("acquireImeControl", NativeCallable::Resumable(acquire_ime)),
        ("resetImeContext", NativeCallable::Resumable(reset_ime)),
        (
            "setWindowCornerPreference",
            NativeCallable::Resumable(corner),
        ),
    ] {
        exports.function(cx, window, name, call)?;
    }
    for &(name, number) in &[
        ("nchtError", 65534),
        ("nchtTransparent", 65535),
        ("nchtNoWhere", 0),
        ("nchtClient", 1),
        ("nchtCaption", 2),
        ("nchtSysMenu", 3),
        ("nchtSize", 4),
        ("nchtGrowBox", 4),
        ("nchtMenu", 5),
        ("nchtHScroll", 6),
        ("nchtVScroll", 7),
        ("nchtMinButton", 8),
        ("nchtReduce", 8),
        ("nchtMaxButton", 9),
        ("nchtZoom", 9),
        ("nchtLeft", 10),
        ("nchtRight", 11),
        ("nchtTop", 12),
        ("nchtTopLeft", 13),
        ("nchtTopRight", 14),
        ("nchtBottom", 15),
        ("nchtBottomLeft", 16),
        ("nchtBottomRight", 17),
        ("nchtBorder", 18),
    ] {
        exports.value(cx, window, name, Value::Int(number))?;
    }
    let menu = class(cx, "MenuItem")?;
    for p in MENU_PROPERTIES {
        exports.property(cx, menu, p)?;
    }
    exports.function(cx, menu, "popupEx", NativeCallable::Leaf(popup))?;
    for &(name, number) in &[
        ("biSystem", 1),
        ("biRestore", 2),
        ("biMinimize", 3),
        ("biClose", 5),
        ("biCloseDisabled", 6),
        ("biMinimizeDisabled", 7),
        ("biPopupClose", 8),
        ("biPopupRestore", 9),
        ("biPopupMaximize", 10),
        ("biPopupMinimize", 11),
    ] {
        exports.value(cx, menu, name, Value::Int(number))?;
    }
    let system = class(cx, "System")?;
    for (name, call) in [
        (
            "findWindowEx",
            no_handle as fn(&mut NativeCx<'_>, &[Value]) -> NativeResult<Value>,
        ),
        ("setDpiAwareness", dpi),
        ("loadCursor", load_cursor),
        ("classLongPtr", class_long),
    ] {
        exports.function(cx, system, name, NativeCallable::Leaf(call))?;
    }
    super::tools::install(cx, exports)
}

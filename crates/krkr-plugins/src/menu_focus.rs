//! `MenuFocus.dll`/`.tpm` adds `System.menuFocus`. The original Win32 plugin
//! pressed and released `VK_MENU` through `keybd_event` so the window menu bar
//! took focus. Native menus are out of scope, so the portable provider keeps
//! the script entry and returns the original integer result without
//! synthesizing input or touching a menu.
use tjs_core::{NativeCx, NativeResult, Value};

krkr_engine::native_plugin! {
    pub(crate) MenuFocus {
        names: ["MenuFocus.dll", "MenuFocus.tpm"],
        classes: [],
        extensions: [("System", "menuFocus", tjs_core::NativeCallable::Leaf(menu_focus))],
    }
}
fn menu_focus(_: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    Ok(Value::Int(0))
}

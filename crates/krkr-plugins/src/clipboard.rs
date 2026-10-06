//! clipboardEx exports; the reusable clipboard and pixel services live in engine.
use crate::exports::class;
use krkr_engine::clipboard;
use tjs_core::{NativeCallable, NativeProperty, Value};
krkr_engine::native_plugin! {
    pub(crate) Clipboard {
        names: ["clipboardEx.dll", "clipboardEx.tpm"],
        link(cx, exports) {
        let target = class(cx, "Clipboard")?;
        let window = class(cx, "Window")?;
        let api = clipboard::capture_array_count(cx.heap)?;
        exports.function(
            cx,
            target,
            "hasFormat",
            NativeCallable::Leaf(clipboard::has_format),
        )?;
        for (name, call) in [
            ("setAsBitmap", clipboard::set_bitmap as _),
            ("getAsBitmap", clipboard::get_bitmap as _),
            ("setMultipleData", clipboard::set_multiple as _),
        ] {
            exports.bound_function(cx, target, name, NativeCallable::Resumable(call), api)?;
        }
        exports.bound_property(cx, target, &TJS, api)?;
        exports.property(cx, window, &WATCH)?;
        for (name, value) in [("cbfBitmap", 2), ("cbfTJS", 3)] {
            exports.value(cx, cx.global, name, Value::Int(value))?;
        }
        Ok(())
        }
    unlink(cx, exports) {
        clipboard::release_watches(cx.heap)?;
        exports.unlink(cx)
    }
    }
}
static TJS: NativeProperty = NativeProperty {
    name: "asTJS",
    doc: "Kirikiri clipboard expression",
    hidden: false,
    class_only: true,
    get: Some(NativeCallable::Resumable(clipboard::get_tjs)),
    set: Some(NativeCallable::Resumable(clipboard::set_tjs)),
};
static WATCH: NativeProperty = NativeProperty {
    name: "clipboardWatchEnabled",
    doc: "Subscribe this window to clipboard changes",
    hidden: false,
    class_only: false,
    get: Some(NativeCallable::Leaf(clipboard::get_watch)),
    set: Some(NativeCallable::Leaf(clipboard::set_watch)),
};

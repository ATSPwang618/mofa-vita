//! Independent parser class replacement; unloading restores the previous export.
use tjs_core::Value;

krkr_engine::native_plugin! {
    pub(crate) KagEx {
        names: ["KAGParserEx.dll", "KAGParserEx.tpm"],
        link(cx, exports) {
            let class = krkr_engine::kag::install_extended(cx.heap)?;
            exports.value(cx, cx.global, "KAGParser", Value::Obj(class.into()))
        }
    }
}
krkr_engine::native_plugin! {
    pub(crate) ExtKag {
        names: ["ExtKAGParser.dll", "ExtKAGParser.tpm"],
        link(cx, exports) {
            let class = krkr_engine::kag::install_advanced(cx.heap)?;
            exports.value(cx, cx.global, "KAGParser", Value::Obj(class.into()))
        }
    }
}

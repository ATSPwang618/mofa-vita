//! Reference author/license notices: reference-notices.txt.
//! Kirikiri2 registory/main.cpp: legacy writes fail when no registry backend exists.
use tjs_core::{NativeError, NativeResult, Value};
krkr_engine::native_plugin! {
    pub(crate) Registory {
        names: ["registory.dll", "registory.tpm"], classes: [],
        extensions: [("System","writeRegValue",write::CALL),("System","deleteRegValue",delete_value::CALL),("System","deleteRegKey",delete_key::CALL)],
    }
}
#[tjs_bind::function]
fn write(_key: Value, _value: Value) -> NativeResult<()> {
    unavailable()
}
#[tjs_bind::function]
fn delete_value(_key: Value) -> NativeResult<()> {
    unavailable()
}
#[tjs_bind::function]
fn delete_key(_key: Value) -> NativeResult<()> {
    unavailable()
}
fn unavailable() -> NativeResult<()> {
    Err(NativeError::Message(
        "System registry is unavailable on this engine",
    ))
}

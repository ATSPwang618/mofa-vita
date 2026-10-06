//! Host-only path helpers used by terminal logging.
use std::path::PathBuf;
use tjs_core::{NativeError, NativeResult};
pub(crate) fn path(name: &[u16]) -> NativeResult<PathBuf> {
    use krkr_engine::assets::{local, name as storage_name};
    let path = if name.starts_with(&storage_name::units("file://")) {
        local::from_storage(name)
    } else {
        local::path(name)
    }
    .map_err(|error| NativeError::Detail(error.to_string()))?;
    local::resolve(&path).map_err(|error| NativeError::Detail(error.to_string()))
}
pub(crate) fn io(error: std::io::Error) -> NativeError {
    NativeError::Detail(error.to_string())
}

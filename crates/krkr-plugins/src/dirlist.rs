//! dirlist.dll, following krkrsdl3's SDL3 directory enumeration.
use krkr_engine::{
    assets::{local, name},
    storages,
};
use tjs_core::{NativeCx, NativeError, NativeResult, Value, value};
krkr_engine::native_plugin! {
    pub(crate) DirList {
        names: ["dirlist.dll", "dirlist.tpm"],
        link(cx, exports) {
            exports.function(cx, cx.global, "getDirList", list::CALL)?;
            Ok(())
        }
    }
}
#[tjs_bind::function]
fn list(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<Value> {
    let text = value::to_string_units(cx.heap(), crate::exports::arg(args, 0)?)?;
    if name::c_string(&text).last() != Some(&47) {
        return Err(NativeError::Message(
            "'/' must be specified at the end of given directory name.",
        ));
    }
    let full = storages::service(cx)?
        .borrow()
        .full_path(&text)
        .map_err(error)?;
    // Normalization still occurs when the return value is discarded, but local
    // conversion and enumeration do not (unlike the older fstat helper).
    if !cx.result_needed() {
        return Ok(Value::Void);
    }
    let path = local::from_storage(&full).map_err(error)?;
    let path = match local::resolve(&path) {
        Ok(path) => path,
        Err(krkr_engine::assets::Error::Io(_)) => path,
        Err(e) => return Err(error(e)),
    };
    let mut values = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() && !metadata.is_dir() {
                continue;
            }
            if values.len() == 100_000 {
                return Err(NativeError::Message("directory exceeds entry limit"));
            }
            let mut units =
                local::units(std::path::Path::new(&entry.file_name())).map_err(error)?;
            for unit in &mut units {
                if (65..=90).contains(unit) {
                    *unit += 32;
                }
            }
            // SDL3 returns bare names: no ./, ../, or slash on directories.
            values.push(Value::Str(cx.heap_mut().alloc_string(units)));
        }
    }
    let array = cx.heap_mut().alloc_array();
    cx.heap_mut().array_replace(array, values)?;
    Ok(Value::Obj(tjs_core::ObjRef::bound(array)))
}
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}

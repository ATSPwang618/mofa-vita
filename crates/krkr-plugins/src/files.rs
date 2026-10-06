//! Portable fstat storage operations. Behavior reference: krkrz/krkrz
//! 49c4d53506edecb824cd7b2cff8d32959b1f1b70, src/plugins/win32/fstat/Main.cpp.
use crate::exports::{arg, class};
use krkr_engine::{
    assets::{Error as AssetError, local, name},
    storages,
};
use std::{fs, path::PathBuf};
use tjs_core::{NativeCallable, NativeCx, NativeError, NativeProperty, NativeResult, Value, value};

mod attributes;
mod directories;
mod paths;
mod selection;
mod temporary;
mod times;
mod transfer;

krkr_engine::native_plugin! {
    pub(crate) Files {
        names: ["fstat.dll", "fstat.tpm"],
        link(cx, exports) {
        let storage = class(cx, "Storages")?;
        times::install(cx, storage)?;
        let temporary = temporary::install(cx.heap)?;
        exports.value(
            cx,
            cx.global,
            "TemporaryFiles",
            Value::Obj(temporary.into()),
        )?;
        for (name, call) in [
            ("setFileAttributes", NativeCallable::Leaf(attributes::set)),
            (
                "resetFileAttributes",
                NativeCallable::Leaf(attributes::reset),
            ),
            ("getFileAttributes", NativeCallable::Leaf(attributes::get)),
            (
                "selectDirectory",
                NativeCallable::Resumable(selection::select),
            ),
            ("dirlist", list::CALL),
            ("dirlistEx", NativeCallable::Resumable(directories::list_ex)),
            ("dirtree", NativeCallable::Resumable(directories::tree)),
            (
                "isExistentStorageNoSearchNoNormalize",
                NativeCallable::Leaf(paths::exists_exact),
            ),
            ("getDisplayName", NativeCallable::Leaf(paths::display_name)),
            ("getMD5HashString", NativeCallable::Resumable(paths::md5)),
            ("searchPath", NativeCallable::Leaf(paths::search)),
            (
                "getTemporaryName",
                NativeCallable::Leaf(paths::temporary_name),
            ),
            ("fstat", NativeCallable::Resumable(times::stat)),
            ("getTime", NativeCallable::Resumable(times::get)),
            ("setTime", NativeCallable::Resumable(times::set)),
            (
                "getLastModifiedFileTime",
                NativeCallable::Leaf(times::modified),
            ),
            (
                "setLastModifiedFileTime",
                NativeCallable::Leaf(times::set_modified),
            ),
            ("clearStorageCaches", NativeCallable::Leaf(clear)),
            ("createDirectory", NativeCallable::Leaf(mkdir)),
            (
                "createDirectoryNoNormalize",
                NativeCallable::Leaf(mkdir_exact),
            ),
            ("removeDirectory", NativeCallable::Leaf(rmdir)),
            ("deleteFile", NativeCallable::Leaf(delete)),
            ("isExistentDirectory", NativeCallable::Leaf(is_dir)),
            ("exportFile", NativeCallable::Resumable(transfer::export)),
            ("truncateFile", NativeCallable::Leaf(truncate)),
            ("moveFile", NativeCallable::Leaf(move_file)),
            ("copyFile", NativeCallable::Leaf(copy)),
            ("copyFileNoNormalize", NativeCallable::Leaf(copy_exact)),
            ("changeDirectory", NativeCallable::Leaf(chdir)),
        ] {
            exports.function(cx, storage, name, call)?;
        }
        exports
            .function(cx, cx.global, "getDirList", list::CALL)?;
        exports.property(cx, storage, &CURRENT_PATH)?;
        for (name, value) in [
            ("FILE_ATTRIBUTE_READONLY", 1),
            ("FILE_ATTRIBUTE_HIDDEN", 2),
            ("FILE_ATTRIBUTE_SYSTEM", 4),
            ("FILE_ATTRIBUTE_DIRECTORY", 16),
            ("FILE_ATTRIBUTE_ARCHIVE", 32),
            ("FILE_ATTRIBUTE_NORMAL", 128),
            ("FILE_ATTRIBUTE_TEMPORARY", 256),
        ] {
            exports.value(cx, cx.global, name, Value::Int(value))?;
        }
        Ok(())
        }
    }
}
fn error(error: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(error.to_string())
}
fn path(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<PathBuf> {
    let text = value::to_string_units(cx.heap(), value)?;
    let full = storages::service(cx)?
        .borrow()
        .full_path(&text)
        .map_err(error)?;
    local_path(&full)
}
fn local_path(full: &[u16]) -> NativeResult<PathBuf> {
    let path = local::from_storage(full).map_err(error)?;
    // Win32 fstat removes the trailing separator before opening a handle.
    let path: PathBuf = path.components().collect();
    match local::resolve(&path) {
        Ok(path) => Ok(path),
        // A missing/inaccessible/non-directory host component is an IO
        // outcome, not malformed storage syntax. Let the operation return its
        // documented false/zero (or throw for getTime/fstat) at the IO step.
        Err(AssetError::Io(_)) => Ok(path),
        Err(e) => Err(error(e)),
    }
}
fn directory(text: &[u16]) -> NativeResult<()> {
    if name::c_string(text).last() != Some(&47) {
        return Err(NativeError::Message(
            "'/' must be specified at the end of given directory name.",
        ));
    }
    Ok(())
}
fn directory_path(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<PathBuf> {
    directory(&value::to_string_units(cx.heap(), value)?)?;
    path(cx, value)
}
fn placed_local(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Option<PathBuf>> {
    let text = value::to_string_units(cx.heap(), value)?;
    let placed = match storages::service(cx)?.borrow_mut().placed_path(&text) {
        Ok(path) => path,
        Err(AssetError::Io(_)) => return Ok(None),
        Err(e) => return Err(error(e)),
    };
    if placed.is_empty() || name::split_archive(&placed).1.is_some() {
        return Ok(None);
    }
    local_path(&placed).map(Some)
}
fn clear(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    storages::service(cx)?.borrow_mut().clear_archive_cache();
    Ok(Value::Void)
}
#[tjs_bind::function(resumable = true)]
fn list(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<tjs_core::NativeStep> {
    use tjs_bind::IntoTjs;
    let text = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    if let Some(resolution) = storages::managed::find(cx, &text)? {
        directory(&resolution.path)?;
        return resolution.start(
            cx,
            storages::managed::Operation::List,
            tjs_bind::flow::callback((), |_, _, value| Ok(tjs_core::NativeStep::Return(value))),
        );
    }
    let values = list_plain(cx, args)?;
    Ok(tjs_core::NativeStep::Return(
        values.into_tjs(cx.heap_mut())?,
    ))
}
fn list_plain(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<tjs_bind::Array<impl Iterator<Item = tjs_bind::Utf16> + use<>>> {
    let text = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    // Unlike mkdir/rmdir, the original checks the delimiter after normalization.
    let full = storages::service(cx)?
        .borrow()
        .full_path(&text)
        .map_err(error)?;
    directory(&full)?;
    if let Some(names) = storages::service(cx)?
        .borrow_mut()
        .list_medium(&full)
        .map_err(error)?
    {
        return Ok(tjs_bind::Array(names.into_iter().map(tjs_bind::Utf16)));
    }
    let path = local_path(&full)?;
    // FindFirstFile includes these two directory entries (dirtree excludes them).
    let mut names = vec![name::units("./"), name::units("../")];
    for entry in fs::read_dir(path).map_err(error)? {
        if names.len() == 100_000 {
            return Err(NativeError::Message("directory exceeds entry limit"));
        }
        let entry = entry.map_err(error)?;
        let mut name = local::units(PathBuf::from(entry.file_name()).as_path()).map_err(error)?;
        if entry.path().is_dir() {
            name.push(47);
        }
        names.push(name);
    }
    let names = storages::service(cx)?
        .borrow_mut()
        .visible_names(&full, names)
        .map_err(error)?;
    Ok(tjs_bind::Array(names.into_iter().map(tjs_bind::Utf16)))
}
fn mkdir(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let p = directory_path(cx, arg(args, 0)?)?;
    Ok(Value::Int(i64::from(fs::create_dir(p).is_ok())))
}
fn mkdir_exact(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let text = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    directory(&text)?;
    let p = local::from_storage(name::c_string(&text)).map_err(error)?;
    // This later extension is implemented by Next with create_directories;
    // preserve its recursive behavior and fail-if-already-present result.
    Ok(Value::Int(i64::from(
        !p.exists() && fs::create_dir_all(p).is_ok(),
    )))
}
fn rmdir(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let p = directory_path(cx, arg(args, 0)?)?;
    Ok(Value::Int(i64::from(fs::remove_dir(p).is_ok())))
}
fn delete(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let Some(p) = placed_local(cx, arg(args, 0)?)? else {
        return Ok(Value::Int(0));
    };
    let deleted = fs::remove_file(p).is_ok();
    if deleted {
        clear(cx, &[])?;
    }
    Ok(Value::Int(i64::from(deleted)))
}
fn is_dir(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let p = directory_path(cx, arg(args, 0)?)?;
    // The original source's active branch returns false on absence. Its old
    // manual (and Next) still describes the disabled -1 branch.
    Ok(Value::Int(i64::from(p.is_dir())))
}
fn truncate(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let filename = arg(args, 0)?;
    let size = value::to_integer(cx.heap(), arg(args, 1)?)? as i32;
    let Some(p) = placed_local(cx, filename)? else {
        return Ok(Value::Int(0));
    };
    if size < 0 {
        return Ok(Value::Int(0));
    }
    Ok(Value::Int(i64::from(
        fs::OpenOptions::new()
            .write(true)
            .open(p)
            .and_then(|f| f.set_len(size as u64))
            .is_ok(),
    )))
}
fn move_file(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let from = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    let to = value::to_string_units(cx.heap(), arg(args, 1)?)?;
    if from.is_empty()
        || to.is_empty()
        || name::split_archive(&from).1.is_some()
        || name::split_archive(&to).1.is_some()
    {
        return Ok(Value::Int(0));
    }
    // moveFile takes already-normalized, full storage names for both sides.
    let from = local::from_storage(name::c_string(&from)).map_err(error)?;
    let to = local::from_storage(name::c_string(&to)).map_err(error)?;
    let moved = rename_no_replace(&from, &to).is_ok();
    if moved {
        clear(cx, &[])?;
    }
    Ok(Value::Int(i64::from(moved)))
}
fn rename_no_replace(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE).map_err(Into::into)
    }
    #[cfg(windows)]
    {
        // atomicwrites 0.4.4 calls MoveFileExW with WRITE_THROUGH only, without
        // REPLACE_EXISTING or COPY_ALLOWED. This preserves files/directories
        // and their attributes while the OS rejects an occupied destination,
        // including one created concurrently with this call.
        atomicwrites::move_atomic(from, to)
    }
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        windows
    )))]
    {
        let _ = (from, to);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "atomic no-replace rename is unavailable",
        ))
    }
}
fn copy(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    copy_file(cx, args, false)
}
fn copy_exact(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    copy_file(cx, args, true)
}
fn copy_file(cx: &mut NativeCx<'_>, args: &[Value], exact: bool) -> NativeResult<Value> {
    let source = arg(args, 0)?;
    let target = value::to_string_units(cx.heap(), arg(args, 1)?)?;
    // The cross-platform krkr2 plugin also supports copyFile(from, to),
    // overwriting the destination. Keep the original explicit third-argument
    // protection; copyFileNoNormalize still requires its declared third arg.
    let overwrite = if exact {
        arg(args, 2)?
    } else {
        args.get(2).copied().unwrap_or(Value::Int(0))
    };
    let fail_if_exists = value::to_integer(cx.heap(), overwrite)? != 0;
    let from = placed_local(cx, source)?;
    let target = if exact {
        name::c_string(&target).to_vec()
    } else {
        storages::service(cx)?
            .borrow()
            .full_path(&target)
            .map_err(error)?
    };
    let Some(from) = from else {
        return Ok(Value::Int(0));
    };
    if target.is_empty() || name::split_archive(&target).1.is_some() {
        return Ok(Value::Int(0));
    }
    let to = if exact {
        local::from_storage(&target).map_err(error)?
    } else {
        local_path(&target)?
    };
    let copied = (|| -> std::io::Result<()> {
        if fail_if_exists {
            // create_new makes the fail-if-present contract hold even if a
            // second writer creates the destination between lookup and open.
            let mut source = fs::File::open(&from)?;
            let metadata = source.metadata()?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&to)?;
            std::io::copy(&mut source, &mut output)?;
            output.set_times(times::copy_times(&metadata))?;
            output.set_permissions(metadata.permissions())?;
        } else {
            if to.exists() {
                if fs::canonicalize(&from)? == fs::canonicalize(&to)? {
                    return Err(std::io::Error::other("cannot copy a file onto itself"));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let source = fs::metadata(&from)?;
                    let target = fs::metadata(&to)?;
                    if source.dev() == target.dev() && source.ino() == target.ino() {
                        return Err(std::io::Error::other(
                            "cannot copy a file onto its hard link",
                        ));
                    }
                }
            }
            fs::copy(&from, &to)?;
            let metadata = fs::metadata(&from)?;
            times::open_for_times(&to)?.set_times(times::copy_times(&metadata))?;
        }
        Ok(())
    })()
    .is_ok();
    if copied {
        clear(cx, &[])?;
    }
    Ok(Value::Int(i64::from(copied)))
}
fn chdir(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let p = directory_path(cx, arg(args, 0)?)?;
    if !p.is_dir() {
        return Ok(Value::Int(0));
    }
    let dir = local::directory(&p).map_err(error)?;
    storages::service(cx)?
        .borrow_mut()
        .set_directory(&dir)
        .map_err(error)?;
    Ok(Value::Int(1))
}

static CURRENT_PATH: NativeProperty = NativeProperty {
    hidden: false,
    class_only: true,
    name: "currentPath",
    doc: "The current directory of this engine's storage service.",
    get: Some(NativeCallable::Leaf(current_path)),
    set: Some(NativeCallable::Leaf(set_current_path)),
};
fn current_path(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    let path = storages::service(cx)?.borrow().current_directory().to_vec();
    Ok(Value::Str(cx.heap_mut().alloc_string(path)))
}
fn set_current_path(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    if matches!(chdir(cx, args)?, Value::Int(0)) {
        return Err(NativeError::Message("setCurrentPath failed"));
    }
    Ok(Value::Void)
}

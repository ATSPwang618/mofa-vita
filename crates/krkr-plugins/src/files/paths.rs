use super::{error, local_path, path};
use crate::exports::arg;
use krkr_engine::{
    assets::{Stream, local, name},
    storages,
};
use std::{
    collections::hash_map::RandomState,
    hash::BuildHasher,
    io::Read,
    sync::atomic::{AtomicU64, Ordering},
};
use tjs_core::{NativeContinuation, NativeCx, NativeResult, NativeStep, Trace, Value, value};

pub(super) fn exists_exact(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let text = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    let exists = storages::service(cx)?
        .borrow_mut()
        .exists_no_search_no_normalize(&text)
        .map_err(error)?;
    Ok(Value::Int(i64::from(exists)))
}
pub(super) fn display_name(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let path = path(cx, arg(args, 0)?)?;
    let path = local::units(&path).map_err(error)?;
    let display = krkr_engine::system::files::display_name(cx, &path)?;
    Ok(Value::Str(cx.heap_mut().alloc_string(display)))
}

struct Digest {
    stream: Box<dyn Stream>,
    digest: md5::Context,
}
impl Trace for Digest {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Digest {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        // Keep the archive reader/filter and digest alive across budget yields.
        // No whole-file allocation and no TJS work on the IO stack.
        let mut bytes = [0u8; 64 * 1024];
        let count = self.stream.read(&mut bytes).map_err(error)?;
        if count == 0 {
            let hash = format!("{:x}", self.digest.finalize());
            return Ok(NativeStep::Return(Value::Str(
                cx.heap_mut().alloc_string(name::units(&hash)),
            )));
        }
        self.digest.consume(&bytes[..count]);
        Ok(NativeStep::Continue(self))
    }
}
pub(super) fn md5(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let text = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    storages::managed::plans(cx, vec![(text, true)], (), |_, _, mut plans| {
        let plan = plans.pop().flatten().expect("required digest plan");
        Ok(NativeStep::Continue(Box::new(Digest {
            stream: plan.open().map_err(error)?,
            digest: md5::Context::new(),
        })))
    })
}
pub(super) fn search(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let filename = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    let filename = local::path(&filename).map_err(error)?;
    let explicit = args
        .get(1)
        .filter(|v| !matches!(v, Value::Void))
        .map(|&v| value::to_string_units(cx.heap(), v))
        .transpose()?
        .unwrap_or_default();
    let service = storages::service(cx)?;
    let current = local_path(service.borrow().current_directory())?;
    let mut paths = Vec::new();
    if filename.is_absolute() || filename.components().count() > 1 {
        paths.push(current.clone());
    } else if !name::c_string(&explicit).is_empty() {
        // krkr2's native POSIX search lists use ':'. Keep legacy ';' lists
        // accepted so portable games do not need rewritten search paths.
        let explicit = name::c_string(&explicit);
        let delimiter = if cfg!(unix) && !explicit.contains(&59) {
            58
        } else {
            59
        };
        for directory in explicit.split(|&u| u == delimiter) {
            if delimiter == 58 && directory.is_empty() {
                continue;
            }
            let directory = local::path(directory).map_err(error)?;
            paths.push(if directory.is_absolute() {
                directory
            } else {
                current.join(directory)
            });
        }
    } else {
        // SearchPathW's default (SafeProcessSearchMode=0) includes application,
        // current, system, Windows, and PATH directories. The current directory
        // here belongs to the VFS. Host registry/process search-mode overrides
        // are deliberately not implied by this portable adapter.
        if let Ok(executable) = std::env::current_exe()
            && let Some(parent) = executable.parent()
        {
            paths.push(parent.to_owned());
        }
        paths.push(current.clone());
        #[cfg(windows)]
        if let Some(windows) = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("WINDIR"))
        {
            let windows = std::path::PathBuf::from(windows);
            paths.extend([windows.join("System32"), windows.join("System"), windows]);
        }
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(
                std::env::split_paths(&path)
                    .map(|p| if p.is_absolute() { p } else { current.join(p) }),
            );
        }
    }
    for directory in paths {
        let candidate = directory.join(&filename);
        if candidate.is_file() {
            let raw = local::units(&candidate).map_err(error)?;
            let found = service.borrow().full_path(&raw).map_err(error)?;
            return Ok(Value::Str(cx.heap_mut().alloc_string(found)));
        }
    }
    Ok(Value::Void)
}
pub(super) fn temporary_name(cx: &mut NativeCx<'_>, _: &[Value]) -> NativeResult<Value> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let entropy = RandomState::new();
    let directory = std::env::temp_dir();
    loop {
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let process = std::process::id();
        let random = entropy.hash_one((number, process)) & 0xffff_ffff_ffff;
        let path = directory.join(format!("krkr_{random:012x}_{number}_{process}"));
        if !path.try_exists().map_err(error)? {
            // TVPGetTemporaryName returns a native host path and creates no
            // file/directory. The caller must still create its resource safely.
            return Ok(Value::Str(
                cx.heap_mut()
                    .alloc_string(local::units(&path).map_err(error)?),
            ));
        }
    }
}

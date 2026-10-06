use super::{directory, error, local_path, times};
use crate::exports::arg;
use krkr_engine::{assets::local, storages};
use std::{fs, path::PathBuf};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, Trace,
    Value, value,
};

const ENTRY_LIMIT: usize = 100_000;

struct Listing {
    entries: fs::ReadDir,
    dots: std::collections::VecDeque<(Vec<u16>, PathBuf)>,
    array: ObjId,
    count: usize,
}
impl Trace for Listing {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.array.into()));
    }
}
impl NativeContinuation for Listing {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if !matches!(result, Value::Void) {
            cx.heap_mut().array_push(self.array, result)?;
        }
        let (filename, path) = if let Some(dot) = self.dots.pop_front() {
            dot
        } else if let Some(entry) = self.entries.next() {
            let entry = entry.map_err(error)?;
            (
                local::units(std::path::Path::new(&entry.file_name())).map_err(error)?,
                entry.path(),
            )
        } else {
            return Ok(NativeStep::Return(Value::Obj(ObjRef::bound(self.array))));
        };
        if self.count == ENTRY_LIMIT {
            return Err(NativeError::Message("directory exceeds entry limit"));
        }
        self.count += 1;
        let metadata = fs::metadata(&path).map_err(error)?;
        let dictionary = cx.heap_mut().alloc_dictionary();
        let filename = Value::Str(cx.heap_mut().alloc_string(filename));
        times::put(cx, dictionary, "name", filename)?;
        times::put(
            cx,
            dictionary,
            "size",
            Value::Int(if metadata.is_dir() {
                0
            } else {
                metadata.len() as i64
            }),
        )?;
        times::put(cx, dictionary, "attrib", Value::Int(attributes(&metadata)))?;
        // This unpublished dictionary becomes visible after all Date callbacks.
        // Preserve the original final key insertion order for enumeration.
        for field in ["mtime", "ctime", "atime"] {
            times::put(cx, dictionary, field, Value::Void)?;
        }
        times::metadata_dates(cx, dictionary, &metadata, Some(self))
    }
}
fn attributes(metadata: &fs::Metadata) -> i64 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        i64::from(metadata.file_attributes())
    }
    #[cfg(not(windows))]
    {
        // Preserve the portable directory/readonly bits, without claiming that
        // Unix permissions implement hidden/system/archive/Windows ACL flags.
        let attr =
            if metadata.is_dir() { 0x10 } else { 0 } | i64::from(metadata.permissions().readonly());
        if attr == 0 { 0x80 } else { attr }
    }
}
pub(super) fn list_ex(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let text = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    let full = storages::service(cx)?
        .borrow()
        .full_path(&text)
        .map_err(error)?;
    directory(&full)?;
    let path = local_path(&full)?;
    let entries = fs::read_dir(&path).map_err(error)?;
    Ok(NativeStep::Continue(Box::new(Listing {
        entries,
        dots: [(vec![46], path.clone()), (vec![46, 46], path.join(".."))].into(),
        array: cx.heap_mut().alloc_array(),
        count: 0,
    })))
}

struct Directory {
    entries: fs::ReadDir,
    prefix: Vec<u16>,
    canonical: PathBuf,
}
struct Tree {
    stack: Vec<Directory>,
    array: ObjId,
    count: usize,
    directories_only: bool,
}
impl Trace for Tree {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.array.into()));
    }
}
impl NativeContinuation for Tree {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        for _ in 0..64 {
            let Some(current) = self.stack.last_mut() else {
                return Ok(NativeStep::Return(Value::Obj(ObjRef::bound(self.array))));
            };
            let Some(entry) = current.entries.next() else {
                self.stack.pop();
                continue;
            };
            let entry = entry.map_err(error)?;
            let path = entry.path();
            let is_directory = path.is_dir();
            if !is_directory && self.directories_only {
                continue;
            }
            let mut relative = current.prefix.clone();
            relative.extend(local::units(std::path::Path::new(&entry.file_name())).map_err(error)?);
            if is_directory {
                relative.push(47);
            }
            if self.count == ENTRY_LIMIT {
                return Err(NativeError::Message("directory tree exceeds entry limit"));
            }
            self.count += 1;
            let name = Value::Str(cx.heap_mut().alloc_string(relative.clone()));
            cx.heap_mut().array_push(self.array, name)?;
            if is_directory {
                let canonical = fs::canonicalize(&path).map_err(error)?;
                // Next follows directory links. Keep that behavior for acyclic
                // links; list a back-edge once without infinitely recursing.
                if self.stack.iter().any(|d| d.canonical == canonical) {
                    continue;
                }
                if self.stack.len() == 256 {
                    return Err(NativeError::Message("directory tree exceeds depth limit"));
                }
                self.stack.push(Directory {
                    entries: fs::read_dir(path).map_err(error)?,
                    prefix: relative,
                    canonical,
                });
            }
        }
        Ok(NativeStep::Continue(self))
    }
}
pub(super) fn tree(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    // This is the real Next extension, absent from krkrz 49c4d535. It accepts
    // either form of the root and returns relative preorder entries.
    let text = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    let directories_only = args
        .get(1)
        .map(|&v| v.truthy(cx.heap()))
        .transpose()?
        .unwrap_or(false);
    let mut text = krkr_engine::assets::name::c_string(&text).to_vec();
    text.push(47);
    let full = storages::service(cx)?
        .borrow()
        .full_path(&text)
        .map_err(error)?;
    let path = local_path(&full)?;
    let stack = if path.is_dir() {
        vec![Directory {
            entries: fs::read_dir(&path).map_err(error)?,
            prefix: vec![],
            canonical: fs::canonicalize(path).map_err(error)?,
        }]
    } else {
        vec![]
    };
    Ok(NativeStep::Continue(Box::new(Tree {
        stack,
        array: cx.heap_mut().alloc_array(),
        count: 0,
        directories_only,
    })))
}

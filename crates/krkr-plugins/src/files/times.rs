use super::{class, error, local_path, path};
use crate::exports::arg;
use krkr_engine::{assets::name, plugins::Context, storages};
use std::{
    fs::{self, File, FileTimes, Metadata, OpenOptions},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tjs_core::{
    HeapError, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef,
    Trace, Value, value,
};

const EPOCH: u64 = 116_444_736_000_000_000;
const FIELDS: [&str; 3] = ["ctime", "atime", "mtime"];

// The original captures Date and its methods when the plugin is linked. Calls
// bind those method objects to the Date, so instance overrides do not intercept
// timestamp IO. This state is heap-local and traced, including saved closures.
#[derive(Clone)]
struct DateApi {
    class: Value,
    get: Value,
    set: Value,
}
impl Trace for DateApi {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.class.trace(visit);
        self.get.trace(visit);
        self.set.trace(visit);
    }
}
pub(super) fn install(cx: &mut Context<'_>, storage: ObjId) -> NativeResult<()> {
    let date = class(cx, "Date")?;
    let mut member = |name: &str| -> NativeResult<Value> {
        let key = cx.heap.intern(&name::units(name));
        cx.heap
            .member(date, key)?
            .ok_or(NativeError::Message("missing Date method"))
    };
    let api = DateApi {
        class: Value::Obj(date.into()),
        get: member("getTime")?,
        set: member("setTime")?,
    };
    cx.heap.initialize_native_state(storage, api.clone())?;
    cx.heap
        .with_native_state::<DateApi, _>(storage, |state| *state = api)
}
fn api(cx: &mut NativeCx<'_>) -> NativeResult<DateApi> {
    let storage = cx
        .heap()
        .registered_class("Storages")
        .expect("installed Storages");
    cx.heap_mut()
        .with_native_state::<DateApi, _>(storage, |state| state.clone())
}
fn bound(method: Value, object: Value) -> NativeResult<Value> {
    let (Value::Obj(mut method), Value::Obj(object)) = (method, object) else {
        return Err(NativeError::Type("a Date method and object"));
    };
    method.this = object.object;
    Ok(Value::Obj(method))
}
pub(super) fn put(
    cx: &mut NativeCx<'_>,
    object: ObjId,
    name: &str,
    value: Value,
) -> NativeResult<()> {
    let key = cx.heap_mut().intern(&name::units(name));
    cx.heap_mut().set_member(object, key, value)?;
    Ok(())
}
fn ticks(time: SystemTime) -> Option<u64> {
    let delta = match time.duration_since(UNIX_EPOCH) {
        Ok(delta) => delta.as_nanos() as i128 / 100,
        Err(delta) => -(delta.duration().as_nanos() as i128 / 100),
    };
    (i128::from(EPOCH) + delta).try_into().ok()
}
fn system_time(ticks: u64) -> Option<SystemTime> {
    let delta = i128::from(ticks) - i128::from(EPOCH);
    let duration = Duration::new(
        (delta.unsigned_abs() / 10_000_000) as u64,
        (delta.unsigned_abs() % 10_000_000) as u32 * 100,
    );
    if delta < 0 {
        UNIX_EPOCH.checked_sub(duration)
    } else {
        UNIX_EPOCH.checked_add(duration)
    }
}
fn millis(time: SystemTime) -> Option<i64> {
    let ticks = ticks(time)?;
    // FILETIME zero means no timestamp; the dictionary still includes the key.
    (ticks != 0).then(|| ((i128::from(ticks) - i128::from(EPOCH)) / 10_000) as i64)
}
pub(super) fn open_for_times(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_WRITE_ATTRIBUTES and FILE_FLAG_BACKUP_SEMANTICS support both
        // files and directories without an unsafe/native handle API.
        options.access_mode(0x100).custom_flags(0x0200_0000);
    }
    #[cfg(not(windows))]
    options.read(true);
    options.open(path)
}
pub(super) fn copy_times(metadata: &Metadata) -> FileTimes {
    let mut times = FileTimes::new();
    if let Ok(time) = metadata.accessed() {
        times = times.set_accessed(time);
    }
    if let Ok(time) = metadata.modified() {
        times = times.set_modified(time);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTimesExt;
        if let Ok(time) = metadata.created() {
            times = times.set_created(time);
        }
    }
    times
}

struct Dates {
    dictionary: ObjId,
    api: DateApi,
    dates: Vec<(&'static str, i64)>,
    pending: Option<(&'static str, i64)>,
    current: Value,
    next: Option<Box<dyn NativeContinuation>>,
}
impl Trace for Dates {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.dictionary.into()));
        self.api.trace(visit);
        self.current.trace(visit);
        if let Some(next) = &self.next {
            next.trace(visit);
        }
    }
}
impl NativeContinuation for Dates {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if let Some((name, time)) = self.pending {
            if matches!(self.current, Value::Void) {
                self.current = result;
                return Ok(NativeStep::CallDiscard {
                    function: bound(self.api.set, result)?,
                    arguments: vec![Value::Int(time)],
                    continuation: self,
                });
            }
            put(cx, self.dictionary, name, self.current)?;
            self.pending = None;
            self.current = Value::Void;
        }
        if let Some((name, time)) = self.dates.pop() {
            self.pending = Some((name, time));
            Ok(NativeStep::Construct {
                class: self.api.class,
                arguments: vec![],
                continuation: self,
            })
        } else {
            let result = Value::Obj(ObjRef::bound(self.dictionary));
            if let Some(next) = self.next {
                next.resume(cx, result)
            } else {
                Ok(NativeStep::Return(result))
            }
        }
    }
}
pub(super) fn stat(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    read(cx, args, true)
}
pub(super) fn get(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    read(cx, args, false)
}
fn read(cx: &mut NativeCx<'_>, args: &[Value], size: bool) -> NativeResult<NativeStep> {
    let filename = value::to_string_units(cx.heap(), arg(args, 0)?)?;
    let vfs = storages::service(cx)?;
    if size {
        let placed = vfs.borrow_mut().placed_path(&filename).map_err(error)?;
        if name::split_archive(&placed).1.is_some() {
            let bytes = vfs.borrow_mut().plan(&placed).map_err(error)?.bytes;
            let dictionary = cx.heap_mut().alloc_dictionary();
            put(cx, dictionary, "size", Value::Int(bytes as i64))?;
            return Ok(NativeStep::Return(Value::Obj(ObjRef::bound(dictionary))));
        }
    }
    // fstat deliberately uses the original name for a local file, whereas an
    // archive match above may come from an auto path. getTime never searches.
    let full = vfs.borrow().full_path(&filename).map_err(error)?;
    let physical = vfs
        .borrow_mut()
        .direct_plan(&full)
        .map_err(error)?
        .map(|plan| plan.physical_name().to_vec())
        .unwrap_or(full);
    let metadata = fs::metadata(local_path(&physical)?).map_err(error)?;
    let dictionary = cx.heap_mut().alloc_dictionary();
    if size && !metadata.is_dir() {
        put(cx, dictionary, "size", Value::Int(metadata.len() as i64))?;
    }
    metadata_dates(cx, dictionary, &metadata, None)
}
pub(super) fn metadata_dates(
    cx: &mut NativeCx<'_>,
    dictionary: ObjId,
    metadata: &Metadata,
    next: Option<Box<dyn NativeContinuation>>,
) -> NativeResult<NativeStep> {
    let mut dates = Vec::new();
    for name in ["mtime", "ctime", "atime"] {
        put(cx, dictionary, name, Value::Void)?;
    }
    for (name, time) in [
        ("mtime", metadata.modified()),
        ("atime", metadata.accessed()),
        ("ctime", metadata.created()),
    ] {
        if let Some(time) = time.ok().and_then(millis) {
            dates.push((name, time));
        }
    }
    Ok(NativeStep::Continue(Box::new(Dates {
        dictionary,
        api: api(cx)?,
        dates,
        pending: None,
        current: Value::Void,
        next,
    })))
}

struct SetTimes {
    path: PathBuf,
    dictionary: Value,
    api: DateApi,
    fields: [Value; 3],
    times: [Option<u64>; 3],
    index: usize,
    reading: bool,
    pending: bool,
    file: Option<File>,
}
impl Trace for SetTimes {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.dictionary.trace(visit);
        for value in self.fields {
            value.trace(visit);
        }
        self.api.trace(visit);
    }
}
impl NativeContinuation for SetTimes {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if self.reading {
            if self.pending {
                self.fields[self.index] = result;
                self.index += 1;
            }
            if self.index < FIELDS.len()
                && matches!(
                    self.dictionary,
                    Value::Obj(ObjRef {
                        object: Some(_),
                        ..
                    })
                )
            {
                self.pending = true;
                let key = Value::Str(cx.heap_mut().alloc_string(name::units(FIELDS[self.index])));
                return Ok(NativeStep::GetOptional {
                    object: self.dictionary,
                    key,
                    continuation: self,
                });
            }
            self.reading = false;
            self.index = 0;
            self.pending = false;
            // Open after reading dictionary properties, before Date conversion,
            // matching the original effect and callback order.
            let Ok(file) = open_for_times(&self.path) else {
                return Ok(NativeStep::Return(Value::Int(0)));
            };
            self.file = Some(file);
        }
        if self.pending {
            let millis = value::to_integer(cx.heap(), result)? as u64;
            self.times[self.index] = Some(millis.wrapping_mul(10_000).wrapping_add(EPOCH));
            self.index += 1;
            self.pending = false;
        }
        while self.index < FIELDS.len() {
            let field = self.fields[self.index];
            if matches!(
                field,
                Value::Obj(ObjRef {
                    object: Some(_),
                    ..
                })
            ) {
                let function = bound(self.api.get, field)?;
                match cx.try_call_leaf(function, &[]) {
                    Ok(Some(result)) => {
                        let millis = value::to_integer(cx.heap(), result)? as u64;
                        self.times[self.index] =
                            Some(millis.wrapping_mul(10_000).wrapping_add(EPOCH));
                    }
                    // Native Date rejects objects without its facet. Preserve
                    // that status without suppressing script callback throws.
                    Err(NativeError::This | NativeError::Heap(HeapError::InvalidObject)) => {}
                    Err(error) => return Err(error),
                    Ok(None) => {
                        self.pending = true;
                        return Ok(NativeStep::Call {
                            function,
                            arguments: vec![],
                            continuation: self,
                        });
                    }
                }
            }
            self.index += 1;
        }
        let success = apply(
            self.file.as_ref().expect("opened timestamp file"),
            self.times,
        )
        .is_ok();
        Ok(NativeStep::Return(Value::Int(i64::from(success))))
    }
}
pub(super) fn set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let target = arg(args, 0)?;
    let dictionary = arg(args, 1)?;
    let path = path(cx, target)?;
    if !matches!(dictionary, Value::Obj(_)) {
        return Err(NativeError::Type("an object"));
    }
    Ok(NativeStep::Continue(Box::new(SetTimes {
        path,
        dictionary,
        api: api(cx)?,
        fields: [Value::Void; 3],
        times: [None; 3],
        index: 0,
        reading: true,
        pending: false,
        file: None,
    })))
}
fn apply(file: &File, values: [Option<u64>; 3]) -> std::io::Result<()> {
    let mut times = FileTimes::new();
    for (index, value) in values.into_iter().enumerate() {
        let Some(ticks) = value.filter(|&ticks| ticks != 0 && ticks != u64::MAX) else {
            continue;
        };
        let time =
            system_time(ticks).ok_or_else(|| std::io::Error::other("timestamp out of range"))?;
        match index {
            0 => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::FileTimesExt;
                    times = times.set_created(time);
                }
                #[cfg(not(windows))]
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "creation time cannot be set on this host",
                ));
            }
            1 => times = times.set_accessed(time),
            _ => times = times.set_modified(time),
        }
    }
    file.set_times(times)
}
pub(super) fn modified(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let path = path(cx, arg(args, 0)?)?;
    let value = fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(ticks)
        .unwrap_or(0);
    Ok(Value::Int(value as i64))
}
pub(super) fn set_modified(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let target = arg(args, 0)?;
    let ticks = value::to_integer(cx.heap(), arg(args, 1)?)? as u64;
    let path = path(cx, target)?;
    let success = open_for_times(&path)
        .and_then(|file| apply(&file, [None, None, Some(ticks)]))
        .is_ok();
    Ok(Value::Int(i64::from(success)))
}

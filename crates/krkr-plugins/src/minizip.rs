//! Kirikiri2 plugins/win32/minizip: Zip, Unzip and the zip:// storage medium.
//! Compression and extraction use the engine IO queue, with portable streams.
mod archive;
mod storage;
mod writer;
use crate::exports::{Exports, arg};
use krkr_engine::{
    assets::{local, name},
    extensions::{self, WorkContinuation},
    plugins::{Context, Plugin},
    storages,
};
use std::{
    cell::Cell,
    io::{Read, Write},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tjs_bind::{IntoTjs, RestArgs, Utf16, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value, value};

fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
fn text(cx: &NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    Ok(tjs_core::string::c_string(&value::to_string_units(cx.heap(), value)?).to_vec())
}
fn password(cx: &NativeCx<'_>, value: Option<&Value>) -> NativeResult<Option<Vec<u8>>> {
    let Some(Value::Str(id)) = value else {
        return Ok(None);
    };
    let string = String::from_utf16_lossy(tjs_core::string::c_string(cx.heap().string(*id)?));
    let (bytes, _, malformed) = encoding_rs::SHIFT_JIS.encode(&string);
    if malformed {
        return Err(error("ZIP password cannot be represented in CP932"));
    }
    Ok(Some(bytes.into_owned()))
}
struct Done<S, T> {
    state: S,
    next: fn(S, &mut NativeCx<'_>, T) -> NativeResult<NativeStep>,
}
impl<S: Trace, T> Trace for Done<S, T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.state.trace(visit);
    }
}
impl<S: Trace, T> WorkContinuation<T> for Done<S, T> {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, result: T) -> NativeResult<NativeStep> {
        (self.next)(self.state, cx, result)
    }
}
fn work<S: Trace + 'static, T: Send + 'static>(
    cx: &mut NativeCx<'_>,
    state: S,
    job: impl FnOnce(&AtomicBool) -> NativeResult<T> + Send + 'static,
    next: fn(S, &mut NativeCx<'_>, T) -> NativeResult<NativeStep>,
) -> NativeResult<NativeStep> {
    extensions::run_work(cx, job, Box::new(Done { state, next }))
}
fn returned(value: impl Into<Value>) -> NativeResult<NativeStep> {
    Ok(NativeStep::Return(value.into()))
}

#[derive(Clone, tjs_bind::Trace)]
struct Medium(#[trace(skip = "Portable mount table without VM values")] Arc<storage::Medium>);
#[derive(Clone, tjs_bind::Trace)]
struct DateApi {
    class: Value,
    set: Value,
}
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Minizip {
    exports: Exports,
    #[trace(skip = "RAII storage registration without VM values")]
    registration: Option<storage::Registration>,
}
krkr_engine::native_plugin! { impl Minizip { names: ["minizip.dll", "minizip.tpm"] } }
impl Plugin for Minizip {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let registration = storage::Registration::new(storages::service_from_heap(cx.heap)?)?;
        let storage = crate::exports::class(cx, "Storages")?;
        let date = crate::exports::class(cx, "Date")?;
        let key = cx.heap.intern(&name::units("setTime"));
        let api = DateApi {
            class: Value::Obj(date.into()),
            set: cx
                .heap
                .member(date, key)?
                .ok_or(error("Date.setTime missing"))?,
        };
        let zip = zip_class::install(cx.heap)?;
        let unzip = unzip_class::install_with_state(
            cx.heap,
            unzip_class::State {
                date: Some(api),
                ..Default::default()
            },
        )?;
        self.exports
            .value(cx, cx.global, "Zip", Value::Obj(zip.into()))?;
        self.exports
            .value(cx, cx.global, "Unzip", Value::Obj(unzip.into()))?;
        for (name, call) in [("mountZip", mount::CALL), ("unmountZip", unmount::CALL)] {
            self.exports.captured_function(
                cx,
                storage,
                name,
                call,
                Medium(registration.medium.clone()),
                false,
            )?;
        }
        self.registration = Some(registration);
        Ok(())
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        self.exports.unlink(cx)?;
        self.registration = None;
        Ok(true)
    }
}
fn medium(cx: &mut NativeCx<'_>) -> NativeResult<Medium> {
    let function = cx.function().ok_or(NativeError::This)?;
    cx.heap_mut()
        .with_native_state::<Medium, _>(function, |s| s.clone())
}
#[derive(tjs_bind::Trace)]
struct Mount {
    medium: Medium,
    domain: String,
    serial: u64,
}
impl Drop for Mount {
    fn drop(&mut self) {
        self.medium.0.cancel(&self.domain, self.serial);
    }
}
#[tjs_bind::function(resumable = true)]
fn mount(cx: &mut NativeCx<'_>, domain: Utf16, filename: Utf16) -> NativeResult<NativeStep> {
    let medium = medium(cx)?;
    let domain = String::from_utf16_lossy(tjs_core::string::c_string(&domain.0));
    let limits = storages::service(cx)?.borrow().limits();
    let Some(serial) = medium.0.begin(&domain, limits) else {
        return returned(Value::Int(0));
    };
    storages::managed::plans(
        cx,
        vec![(filename.0, false)],
        Mount {
            medium,
            domain,
            serial,
        },
        |state, cx, mut plans| {
            let limits = storages::service(cx)?.borrow().limits();
            let Some(plan) = plans.pop().flatten() else {
                return returned(Value::Int(0));
            };
            work(
                cx,
                state,
                move |cancelled| {
                    let archive =
                        archive::Archive::load(plan, limits, &|| cancelled.load(Ordering::Relaxed));
                    archive::cancel(&|| cancelled.load(Ordering::Relaxed)).map_err(error)?;
                    Ok((archive.ok().map(Arc::new), limits))
                },
                |state, _, (archive, limits)| {
                    returned(Value::Int(i64::from(state.medium.0.finish(
                        state.domain.clone(),
                        state.serial,
                        archive,
                        limits,
                    ))))
                },
            )
        },
    )
}
#[tjs_bind::function]
fn unmount(cx: &mut NativeCx<'_>, domain: Utf16) -> NativeResult<bool> {
    Ok(medium(cx)?
        .0
        .unmount(&String::from_utf16_lossy(tjs_core::string::c_string(
            &domain.0,
        ))))
}

#[derive(tjs_bind::Trace)]
struct Busy {
    owner: ObjId,
    #[trace(skip = "VM-local operation flag without managed handles")]
    flag: Rc<Cell<bool>>,
}
impl Drop for Busy {
    fn drop(&mut self) {
        self.flag.set(false);
    }
}
fn busy(cx: &mut NativeCx<'_>) -> NativeResult<Busy> {
    let owner = cx.this();
    let flag = zip_class::with_state(cx, owner, |s| s.busy.clone())?;
    if flag.replace(true) {
        return Err(error("ZIP operation already in progress"));
    }
    Ok(Busy { owner, flag })
}
#[tjs_bind::class(name = "Zip")]
mod zip_class {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        #[trace(skip = "Owned portable ZIP writer")]
        pub(super) writer: Option<writer::Writer>,
        #[trace(skip = "VM-local operation flag")]
        pub(super) busy: Rc<Cell<bool>>,
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.writer = None;
        }
        #[tjs::method(resumable = true)]
        fn open(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let path = text(cx, arg(args, 0)?)?;
            let mode = args
                .get(1)
                .map(|v| value::to_integer(cx.heap(), *v))
                .transpose()?
                .unwrap_or(0) as i32;
            let guard = busy(cx)?;
            storages::managed::plans(
                cx,
                vec![(path.clone(), false)],
                (guard, (path, mode)),
                |(guard, (path, mode)), cx, mut plans| {
                    let existing = plans.pop().flatten();
                    if mode == 0 && existing.is_some() {
                        return Err(error("ZIP file exists"));
                    }
                    let path = if mode == 2 {
                        existing.as_ref().map_or(path.clone(), |p| p.name.clone())
                    } else {
                        path
                    };
                    let vfs = storages::service(cx)?;
                    let path = local::resolve(
                        &local::from_storage(&vfs.borrow().full_path(&path).map_err(error)?)
                            .map_err(error)?,
                    )
                    .map_err(error)?;
                    let limits = vfs.borrow().limits();
                    let previous = with_state(cx, guard.owner, |s| s.writer.take())?;
                    work(
                        cx,
                        guard,
                        move |cancelled| {
                            if let Some(mut previous) = previous {
                                previous.close()?;
                            }
                            archive::cancel(&|| cancelled.load(Ordering::Relaxed))
                                .map_err(error)?;
                            writer::Writer::open(&path, mode, limits)
                        },
                        |guard, cx, writer| {
                            with_state(cx, guard.owner, |s| s.writer = Some(writer))?;
                            storages::service(cx)?.borrow_mut().clear_archive_cache();
                            returned(Value::Void)
                        },
                    )
                },
            )
        }
        #[tjs::method(resumable = true)]
        fn close(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let guard = busy(cx)?;
            let writer = with_state(cx, guard.owner, |s| s.writer.take())?;
            work(
                cx,
                guard,
                move |_| {
                    if let Some(mut w) = writer {
                        w.close()?;
                    }
                    Ok(())
                },
                |_, cx, ()| {
                    storages::service(cx)?.borrow_mut().clear_archive_cache();
                    returned(Value::Void)
                },
            )
        }
        #[tjs::method(resumable = true)]
        fn add(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            arg(args, 1)?;
            let guard = busy(cx)?;
            if !with_state(cx, guard.owner, |s| s.writer.is_some())? {
                return Err(error("don't open zipfile"));
            }
            let source = text(cx, args[0])?;
            let destination = String::from_utf16_lossy(&text(cx, args[1])?);
            let level = args
                .get(2)
                .map(|v| value::to_integer(cx.heap(), *v))
                .transpose()?
                .unwrap_or(-1) as i32;
            let password = password(cx, args.get(3))?;
            storages::managed::plans(
                cx,
                vec![(source, true)],
                (guard, (destination, (level, password))),
                |(guard, (destination, (level, password))), cx, mut plans| {
                    let plan = plans.pop().flatten().expect("required ZIP source");
                    let mut writer = with_state(cx, guard.owner, |s| s.writer.take())?
                        .ok_or(error("don't open zipfile"))?;
                    work(
                        cx,
                        guard,
                        move |cancelled| {
                            let result = writer.add(plan, destination, level, password, &|| {
                                cancelled.load(Ordering::Relaxed)
                            });
                            Ok((writer, result))
                        },
                        |guard, cx, (writer, result)| {
                            with_state(cx, guard.owner, |s| s.writer = Some(writer))?;
                            storages::service(cx)?.borrow_mut().clear_archive_cache();
                            returned(Value::Int(i64::from(result?)))
                        },
                    )
                },
            )
        }
    }
}

#[tjs_bind::class(name = "Unzip")]
mod unzip_class {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        #[trace(skip = "ZIP archive contains only portable resources")]
        pub(super) archive: Option<Arc<archive::Archive>>,
        pub(super) date: Option<DateApi>,
        pub(super) generation: u64,
    }
    impl State {
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>) -> NativeResult<Self> {
            let class = cx
                .heap()
                .registered_class("Unzip")
                .ok_or(NativeError::This)?;
            let date = with_state(cx, class, |s| s.date.clone())?;
            Ok(Self {
                date,
                ..Self::default()
            })
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.close();
            self.date = None;
        }
        #[tjs::method]
        fn close(&mut self) {
            self.archive = None;
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::method(resumable = true)]
        fn open(cx: &mut NativeCx<'_>, filename: Utf16) -> NativeResult<NativeStep> {
            let owner = cx.this();
            let generation = with_state(cx, owner, |s| {
                s.close();
                s.generation
            })?;
            storages::managed::plans(
                cx,
                vec![(filename.0, true)],
                (owner, generation),
                |state, cx, mut plans| {
                    let plan = plans.pop().flatten().expect("required ZIP plan");
                    let limits = storages::service(cx)?.borrow().limits();
                    work(
                        cx,
                        state,
                        move |cancelled| {
                            archive::Archive::load(plan, limits, &|| {
                                cancelled.load(Ordering::Relaxed)
                            })
                            .map(Arc::new)
                            .map_err(error)
                        },
                        |(owner, generation), cx, archive| {
                            let installed = with_state(cx, owner, |s| {
                                if s.generation != generation {
                                    return false;
                                }
                                s.archive = Some(archive);
                                true
                            })?;
                            if !installed {
                                return Err(error("Unzip changed while opening"));
                            }
                            returned(Value::Void)
                        },
                    )
                },
            )
        }
        #[tjs::method(resumable = true)]
        fn list(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            let (archive, date) =
                with_state(cx, cx.this(), |s| (s.archive.clone(), s.date.clone()))?;
            let archive = archive.ok_or(error("don't open zipfile"))?;
            let date = date.ok_or(error("Date API unavailable"))?;
            let array = cx.heap_mut().alloc_array();
            list_next(
                List {
                    archive,
                    date,
                    array,
                    index: 0,
                },
                cx,
            )
        }
        #[tjs::method(resumable = true)]
        fn extract(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            arg(args, 1)?;
            let archive = with_state(cx, cx.this(), |s| s.archive.clone())?
                .ok_or(error("don't open zipfile"))?;
            let source = String::from_utf16_lossy(&text(cx, args[0])?);
            let destination = text(cx, args[1])?;
            let password = password(cx, args.get(2))?;
            let Some(index) = archive.find(&source) else {
                return returned(Value::Int(0));
            };
            let plan = storages::service(cx)?
                .borrow()
                .write_plan(&destination)
                .map_err(error)?;
            work(
                cx,
                (),
                move |cancelled| {
                    let check = || cancelled.load(Ordering::Relaxed);
                    archive::cancel(&check).map_err(error)?;
                    let mut input = match archive.open(index, password) {
                        Ok(input) => input,
                        Err(_) => return Ok(false),
                    };
                    let mut output = plan.create().map_err(error)?;
                    let mut buffer = [0; 16384];
                    loop {
                        archive::cancel(&check).map_err(error)?;
                        let n = match input.read(&mut buffer) {
                            Ok(n) => n,
                            Err(_) => return Ok(false),
                        };
                        if n == 0 {
                            break;
                        }
                        output.write_all(&buffer[..n]).map_err(error)?;
                    }
                    archive::cancel(&check).map_err(error)?;
                    output.finish().map_err(error)?;
                    Ok(true)
                },
                |(), cx, success| {
                    storages::service(cx)?.borrow_mut().clear_archive_cache();
                    returned(Value::Int(i64::from(success)))
                },
            )
        }
    }
}
#[derive(tjs_bind::Trace)]
struct List {
    #[trace(skip = "ZIP directory metadata without VM values")]
    archive: Arc<archive::Archive>,
    date: DateApi,
    array: ObjId,
    index: usize,
}
fn put(cx: &mut NativeCx<'_>, object: ObjId, name: &str, value: Value) -> NativeResult<()> {
    let key = cx.heap_mut().intern(&name::units(name));
    cx.heap_mut().set_member(object, key, value)?;
    Ok(())
}
fn list_next(mut s: List, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let Some(entry) = s.archive.entries.get(s.index) else {
        return returned(Value::Obj(tjs_core::ObjRef::bound(s.array)));
    };
    let row = cx.heap_mut().alloc_dictionary();
    let filename = entry.name.clone().into_tjs(cx.heap_mut())?;
    put(cx, row, "filename", filename)?;
    for (name, v) in [
        ("uncompressed_size", entry.size as i32),
        ("compressed_size", entry.compressed as i32),
        ("crypted", i32::from(entry.flags & 1)),
        ("deflated", i32::from(entry.deflated)),
        ("deflateLevel", i32::from((entry.flags & 6) / 2)),
        ("crc", entry.crc as i32),
    ] {
        put(cx, row, name, Value::Int(i64::from(v)))?;
    }
    let time = entry.date;
    cx.heap_mut()
        .array_push(s.array, Value::Obj(tjs_core::ObjRef::bound(row)))?;
    s.index += 1;
    if let Some(time) = time {
        Ok(NativeStep::Construct {
            class: s.date.class,
            arguments: vec![],
            continuation: flow::callback((s, (row, time)), |(s, (row, time)), _, date| {
                let Value::Obj(mut set) = s.date.set else {
                    return Err(error("invalid Date.setTime"));
                };
                let Value::Obj(object) = date else {
                    return Err(error("invalid Date object"));
                };
                set.this = object.object;
                Ok(NativeStep::CallDiscard {
                    function: Value::Obj(set),
                    arguments: vec![Value::Int(time)],
                    continuation: flow::callback((s, (row, date)), |(s, (row, date)), cx, _| {
                        put(cx, row, "date", date)?;
                        list_next(s, cx)
                    }),
                })
            }),
        })
    } else {
        Ok(NativeStep::Continue(flow::callback(s, |s, cx, _| {
            list_next(s, cx)
        })))
    }
}

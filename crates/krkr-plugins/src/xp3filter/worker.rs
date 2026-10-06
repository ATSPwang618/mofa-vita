use super::*;
use krkr_engine::{Engine, EngineEvent};
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tjs_core::{Callback, NativeCx, NativeStep, ObjId, ObjRef, RunBudget, Value};
use tjs_runtime::{ContextId, Runtime, RuntimeExit, clock::MonotonicClock};
type Reply<T> = mpsc::SyncSender<Result<T, String>>;
pub(super) enum Request {
    Open {
        file: Vec<u16>,
        archive: Vec<u16>,
        size: u64,
        hash: u32,
        reply: Reply<(u64, bool)>,
    },
    Apply {
        id: u64,
        offset: u64,
        bytes: Vec<u8>,
        reply: Reply<Vec<u8>>,
    },
    Close(u64),
}
#[derive(Default, tjs_bind::Trace)]
struct Callbacks {
    decoder: Value,
    content: Value,
}
#[tjs_bind::function]
fn extraction(cx: &mut NativeCx<'_>, callback: Value) -> NativeResult<()> {
    if !matches!(callback, Value::Obj(_)) {
        return Err(NativeError::Type("an object closure"));
    }
    let class = cx
        .heap()
        .registered_class("Storages")
        .ok_or(NativeError::This)?;
    cx.heap_mut().with_native_state::<Callbacks, _>(class, |s| {
        s.decoder = callback;
    })
}
#[tjs_bind::function]
fn content(cx: &mut NativeCx<'_>, callback: Value) -> NativeResult<()> {
    if !matches!(callback, Value::Obj(_)) {
        return Err(NativeError::Type("an object closure"));
    }
    let class = cx
        .heap()
        .registered_class("Storages")
        .ok_or(NativeError::This)?;
    cx.heap_mut().with_native_state::<Callbacks, _>(class, |s| {
        s.content = callback;
    })
}
#[tjs_bind::function(resumable = true)]
fn read_member(_cx: &mut NativeCx<'_>, object: Value, key: Value) -> NativeResult<NativeStep> {
    Ok(NativeStep::GetOr {
        object,
        key,
        raw: false,
        fallback: Value::Void,
        continuation: tjs_bind::flow::callback((), |_, _, value| Ok(NativeStep::Return(value))),
    })
}
struct ContextData {
    value: Value,
    file: Vec<u16>,
    hash: u32,
}
struct Decoder {
    engine: Engine<MonotonicClock>,
    contexts: HashMap<u64, ContextData>,
    serial: u64,
    storages: ObjId,
    reader: Value,
}
impl Decoder {
    fn new(spec: &Spec) -> Result<Self, String> {
        let mut runtime = Runtime::new();
        krkr_engine::install(&mut runtime, Log).map_err(message)?;
        let vfs = Vfs::new(&spec.directory, spec.limits).map_err(message)?;
        storages::install(&mut runtime.heap, vfs).map_err(message)?;
        let storages = runtime
            .heap
            .registered_class("Storages")
            .ok_or("missing Storages")?;
        runtime
            .heap
            .initialize_native_default::<Callbacks>(storages)
            .map_err(message)?;
        for (name, call) in [
            ("setXP3ArchiveExtractionFilter", extraction::CALL),
            ("setXP3ArchiveContentFilter", content::CALL),
        ] {
            let key = runtime.heap.intern(&assets::name::units(name));
            let function = runtime.heap.alloc_native_function(call);
            runtime
                .heap
                .set_member_flags(storages, key, Value::Obj(function.into()), false, true)
                .map_err(message)?;
        }
        let reader = runtime.heap.alloc_native_function(read_member::CALL);
        // Private native state roots the helper without introducing a script global.
        runtime
            .heap
            .initialize_native_state(storages, ReaderRoot(Value::Obj(reader.into())))
            .map_err(message)?;
        let source = runtime
            .sources
            .add_utf16(&spec.source_name, spec.source.as_ref().clone())
            .map_err(message)?;
        // Each decoder adds the same sole source. Share immutable code while
        // preserving independent heaps, callbacks and per-stream state.
        let module = spec
            .compiled
            .get_or_init(|| tjs_front::compile(&runtime.sources, source).map_err(message))
            .clone()?;
        let mut engine = Engine::new(
            runtime,
            MonotonicClock::default(),
            tjs_runtime::SchedulerLimits {
                max_contexts: 8,
                ..Default::default()
            },
            Default::default(),
        )
        .map_err(message)?;
        let reader = Value::Obj(ObjRef {
            object: Some(reader),
            this: Some(engine.global()),
        });
        let id = engine
            .submit(&module)
            .map_err(|_| "filter startup context capacity")?;
        let mut decoder = Self {
            engine,
            contexts: HashMap::new(),
            serial: 0,
            storages,
            reader,
        };
        decoder.drive(id)?;
        if spec.require_extraction && !callable(decoder.callbacks()?.0) {
            return Err("XP3 filter script did not register an extraction callback".into());
        }
        Ok(decoder)
    }
    fn drive(&mut self, id: ContextId) -> Result<Value, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if Instant::now() >= deadline {
                break;
            }
            match self.engine.poll(
                RunBudget::new(10_000).unwrap(),
                NonZeroUsize::new(64).unwrap(),
            ) {
                EngineEvent::Completed { context, result } if context == id => {
                    self.engine.take_result(id);
                    return match result {
                        RuntimeExit::Finished(value) => Ok(value),
                        RuntimeExit::Thrown(error) => Err(format!(
                            "XP3 filter: {}",
                            self.engine
                                .runtime()
                                .heap
                                .display(error.value)
                                .unwrap_or_else(|_| "script exception".into())
                        )),
                        other => Err(format!("XP3 filter: {other:?}")),
                    };
                }
                EngineEvent::Waiting { request, .. } => {
                    if !self.engine.owns_wait(request) {
                        self.engine.cancel(id);
                        return Err("filter requested an unavailable host operation".into());
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                EngineEvent::Terminated(code) => {
                    return Err(format!("filter VM terminated: {code}"));
                }
                _ => {}
            }
            self.engine
                .collect(self.contexts.values().map(|ctx| ctx.value));
        }
        self.engine.cancel(id);
        Err("XP3 filter work/time limit".into())
    }
    fn call(&mut self, callback: Value, arguments: Vec<Value>) -> Result<Value, String> {
        let id = self
            .engine
            .submit_callback(Callback::Function(callback), arguments)
            .map_err(|_| "filter callback capacity")?;
        self.drive(id)
    }
    fn get(&mut self, object: Value, index: i64) -> Result<Value, String> {
        self.call(self.reader, vec![object, Value::Int(index)])
    }
    fn callbacks(&mut self) -> Result<(Value, Value), String> {
        self.engine
            .runtime_mut()
            .heap
            .with_native_state::<Callbacks, _>(self.storages, |c| (c.decoder, c.content))
            .map_err(message)
    }
    fn open(
        &mut self,
        file: Vec<u16>,
        archive: Vec<u16>,
        size: u64,
        hash: u32,
    ) -> Result<(u64, bool), String> {
        if self.contexts.len() >= 4096 {
            return Err("too many open filtered streams".into());
        }
        let (_, content) = self.callbacks()?;
        let mut context = Value::Void;
        let mut full = false;
        if callable(content) {
            let heap = &mut self.engine.runtime_mut().heap;
            let arguments = vec![
                Value::Str(heap.alloc_string(file.clone())),
                Value::Str(heap.alloc_string(archive)),
                Value::Int(size as i64),
            ];
            let result = self.call(content, arguments)?;
            if callable(result) {
                let flags = self.get(result, 0)?;
                full = tjs_core::value::to_integer(&self.engine.runtime().heap, flags)
                    .map_err(message)? as i32
                    == 1;
                // Root the result between its two resumable property reads.
                let pin = ContextData {
                    value: result,
                    file: Vec::new(),
                    hash: 0,
                };
                self.contexts.insert(u64::MAX, pin);
                let value = self.get(result, 1);
                self.contexts.remove(&u64::MAX);
                context = value?;
            }
        }
        self.serial = self
            .serial
            .checked_add(1)
            .filter(|&id| id != u64::MAX)
            .ok_or("filter stream id exhausted")?;
        self.contexts.insert(
            self.serial,
            ContextData {
                value: context,
                file,
                hash,
            },
        );
        Ok((self.serial, full))
    }
    fn apply(&mut self, id: u64, offset: u64, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        let (decoder, _) = self.callbacks()?;
        if !callable(decoder) {
            return Ok(bytes);
        }
        let ctx = self.contexts.get(&id).ok_or("unknown filtered stream")?;
        let heap = &mut self.engine.runtime_mut().heap;
        let length = bytes.len();
        let object = buffer::create(heap, bytes).map_err(message)?;
        let arguments = vec![
            Value::Int(i64::from(ctx.hash)),
            Value::Int(offset as i64),
            Value::Obj(object.into()),
            Value::Int(length as i64),
            Value::Str(heap.alloc_string(ctx.file.clone())),
            ctx.value,
        ];
        // The callback can retain the accessor, but its data expires on return.
        let result = self.call(decoder, arguments);
        let bytes = buffer::take(&mut self.engine.runtime_mut().heap, object).map_err(message);
        result?;
        bytes
    }
}
#[derive(tjs_bind::Trace)]
struct ReaderRoot(Value);
fn callable(value: Value) -> bool {
    matches!(
        value,
        Value::Obj(ObjRef {
            object: Some(_),
            ..
        })
    )
}
fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}
pub(super) fn run(spec: Spec, receive: mpsc::Receiver<Request>) {
    let mut decoder = Decoder::new(&spec);
    while let Ok(request) = receive.recv() {
        match request {
            Request::Open {
                file,
                archive,
                size,
                hash,
                reply,
            } => {
                let result = match &mut decoder {
                    Ok(decoder) => decoder.open(file, archive, size, hash),
                    Err(error) => Err(error.clone()),
                };
                let _ = reply.send(result);
            }
            Request::Apply {
                id,
                offset,
                bytes,
                reply,
            } => {
                let result = match &mut decoder {
                    Ok(decoder) => decoder.apply(id, offset, bytes),
                    Err(error) => Err(error.clone()),
                };
                let _ = reply.send(result);
            }
            Request::Close(id) => {
                if let Ok(decoder) = &mut decoder {
                    decoder.contexts.remove(&id);
                    decoder
                        .engine
                        .collect(decoder.contexts.values().map(|c| c.value));
                }
            }
        }
    }
}
struct Log;
impl krkr_engine::debug::LogOutput for Log {
    fn timestamp(&mut self) -> String {
        String::new()
    }
    fn console(&mut self, line: &[u16]) {
        eprintln!("{}", String::from_utf16_lossy(line));
    }
    #[cfg(not(target_os = "vita"))]
    fn file_output(&mut self) -> Option<&mut dyn krkr_engine::debug::FileOutput> {
        Some(self)
    }
}
#[cfg(not(target_os = "vita"))]
impl krkr_engine::debug::FileOutput for Log {
    fn normalize_directory(&mut self, path: &[u16]) -> NativeResult<Vec<u16>> {
        Ok(path.to_vec())
    }
    fn write_file(&mut self, directory: &[u16], text: &[u16], clear: bool) -> NativeResult<()> {
        use std::io::Write;
        let path = assets::local::from_storage(directory).map_err(error)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(!clear)
            .truncate(clear)
            .open(path.join("xp3filter.log"))
            .map_err(error)?;
        file.write_all(String::from_utf16_lossy(text).as_bytes())
            .map_err(error)
    }
}

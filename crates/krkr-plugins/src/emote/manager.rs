//! Resource loading stays on the engine's resumable IO path. Decrypt callbacks
//! execute on the calling VM with a bounded mutable accessor, never a raw pointer.
use super::{container, resource::File};
use krkr_engine::{
    extensions,
    protocol::budget::{Budget, Permit},
    storages,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
};
use tjs_bind::{IntoTjs, Utf16, flow};
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value};

pub(super) struct Resource {
    pub file: Arc<File>,
}
#[derive(tjs_bind::Trace)]
pub(super) struct Entry {
    pub root: Value,
    #[trace(skip = "Shared decoded PSB data and budget permit have no VM handles")]
    pub resource: Arc<Resource>,
}
#[tjs_bind::class(name = "Motion.ResourceManager")]
pub(super) mod bindings {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub window: Value,
        pub generation: u64,
        pub cache: BTreeMap<Vec<u16>, Entry>,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.window.trace(visit);
            for entry in self.cache.values() {
                entry.trace(visit);
            }
        }
    }
    impl State {
        #[tjs::constructor(resumable = true)]
        fn construct(
            cx: &mut NativeCx<'_>,
            window: Value,
            #[tjs(coerce)] _cache_size: i32,
        ) -> NativeResult<NativeStep> {
            super::super::initialize::start(cx, window)
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.clear();
            self.window = Value::Void;
        }
        fn clear(&mut self) {
            self.cache.clear();
            self.generation = self.generation.wrapping_add(1);
        }
        #[tjs::method(name = "unloadAll")]
        fn unload_all(&mut self) {
            self.clear();
        }
        #[tjs::method(name = "clearCache")]
        fn clear_cache(&self) {} // The reference keeps loaded resources here.
        #[tjs::method(name = "setEmotePSBDecryptSeed")]
        fn seed(cx: &mut NativeCx<'_>, #[tjs(coerce)] seed: i32) -> NativeResult<()> {
            super::super::initialize::runtime(cx, |s| s.decrypt_seed = seed)
        }
        #[tjs::method(name = "setEmotePSBDecryptFunc")]
        fn callback(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            if !matches!(value, Value::Obj(_)) {
                return Err(NativeError::Type("an object closure"));
            }
            super::super::initialize::runtime(cx, |s| s.decrypt_callback = value)
        }
        #[tjs::method(resumable = true)]
        fn load(cx: &mut NativeCx<'_>, path: Utf16) -> NativeResult<NativeStep> {
            super::load(cx, path.0)
        }
        #[tjs::method(resumable = true)]
        fn unload(cx: &mut NativeCx<'_>, path: Utf16) -> NativeResult<NativeStep> {
            let owner = cx.this();
            storages::managed::plans(
                cx,
                vec![(trim(path.0), false)],
                owner,
                |owner, cx, mut plans| {
                    let name = plans.pop().flatten().map_or_else(Vec::new, |p| p.name);
                    with_state(cx, owner, |s| {
                        s.cache.remove(&name);
                        s.generation = s.generation.wrapping_add(1);
                    })?;
                    Ok(NativeStep::Return(Value::Void))
                },
            )
        }
    }
}
fn trim(path: Vec<u16>) -> Vec<u16> {
    let prefix: Vec<u16> = "lzfs://./".encode_utf16().collect();
    if path.starts_with(&prefix) {
        path[prefix.len()..].to_vec()
    } else {
        path
    }
}
#[derive(tjs_bind::Trace)]
struct Loading {
    owner: ObjId,
    name: Vec<u16>,
    generation: u64,
    callback: Value,
    #[trace(skip = "Shared byte budget contains no script values")]
    budget: Budget,
}
fn load(cx: &mut NativeCx<'_>, path: Vec<u16>) -> NativeResult<NativeStep> {
    let owner = cx.this();
    storages::managed::plans(
        cx,
        vec![(trim(path), true)],
        owner,
        |owner, cx, mut plans| {
            let plan = plans
                .pop()
                .flatten()
                .ok_or(NativeError::Message("E-mote resource not found"))?;
            let cached =
                bindings::with_state(cx, owner, |s| s.cache.get(&plan.name).map(|e| e.root))?;
            if let Some(root) = cached {
                return Ok(NativeStep::Return(root));
            }
            let generation = bindings::with_state(cx, owner, |s| s.generation)?;
            let (seed, callback) =
                super::initialize::runtime(cx, |s| (s.decrypt_seed, s.decrypt_callback))?;
            let budget = extensions::image_staging_budget(cx.heap_mut())?
                .unwrap_or_else(|| Budget::new(64 * 1024 * 1024));
            let loading = Loading {
                owner,
                name: plan.name.clone(),
                generation,
                callback,
                budget,
            };
            extensions::run_work(
                cx,
                move |stop| {
                    let cancelled = || stop.load(Ordering::Relaxed);
                    let bytes = plan
                        .read_interruptible(0, cancelled)
                        .map_err(|e| NativeError::Detail(e.to_string()))?;
                    container::unpack(bytes, seed, &cancelled).map_err(NativeError::Message)
                },
                Box::new(loading),
            )
        },
    )
}
impl extensions::WorkContinuation<Vec<u8>> for Loading {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, bytes: Vec<u8>) -> NativeResult<NativeStep> {
        let permit = self
            .budget
            .reserve(bytes.len())
            .map_err(|e| NativeError::Detail(e.to_string()))?;
        if matches!(self.callback,Value::Obj(r) if r.object.is_some()) {
            let length = bytes.len();
            let accessor = crate::xp3filter::buffer::create(cx.heap_mut(), bytes)?;
            cx.heap_mut()
                .initialize_native_state(accessor, BufferBudget(Some(permit)))?;
            Ok(NativeStep::Call {
                function: self.callback,
                arguments: vec![Value::Obj(accessor.into()), Value::Int(length as i64)],
                continuation: flow::callback(
                    Decrypt {
                        loading: *self,
                        accessor,
                    },
                    |s, cx, _| {
                        let bytes = crate::xp3filter::buffer::take(cx.heap_mut(), s.accessor)?;
                        let permit = cx
                            .heap_mut()
                            .with_native_state::<BufferBudget, _>(s.accessor, |s| s.0.take())?
                            .ok_or(NativeError::Message("E-mote buffer budget was released"))?;
                        s.loading.parse(cx, bytes, permit)
                    },
                ),
            })
        } else {
            self.parse(cx, bytes, permit)
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Decrypt {
    loading: Loading,
    accessor: ObjId,
}
struct BufferBudget(Option<Permit>);
impl Trace for BufferBudget {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl Loading {
    fn parse(
        self,
        cx: &mut NativeCx<'_>,
        bytes: Vec<u8>,
        permit: Permit,
    ) -> NativeResult<NativeStep> {
        let valid = bindings::with_state(cx, self.owner, |s| s.generation == self.generation)?;
        if !valid {
            return Err(NativeError::Message(
                "E-mote resources changed while loading",
            ));
        }
        let original = bytes.len();
        let budget = self.budget.clone();
        extensions::run_work(
            cx,
            move |stop| {
                let mut file = File::decode(bytes, &|| stop.load(Ordering::Relaxed))
                    .map_err(NativeError::Message)?;
                file.permits.push(permit);
                if file.bytes.len() > original {
                    file.permits.push(
                        budget
                            .reserve(file.bytes.len() - original)
                            .map_err(|e| NativeError::Detail(e.to_string()))?,
                    );
                }
                Ok(Resource {
                    file: Arc::new(file),
                })
            },
            Box::new(self),
        )
    }
}
impl extensions::WorkContinuation<Resource> for Loading {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        resource: Resource,
    ) -> NativeResult<NativeStep> {
        let valid = bindings::with_state(cx, self.owner, |s| s.generation == self.generation)?;
        if !valid {
            return Err(NativeError::Message(
                "E-mote resources changed while loading",
            ));
        }
        let root = crate::psb::Metadata(&resource.file.root, &resource.file.bytes)
            .into_tjs(cx.heap_mut())?;
        bindings::with_state(cx, self.owner, |s| {
            s.cache.insert(
                self.name,
                Entry {
                    root,
                    resource: Arc::new(resource),
                },
            );
        })?;
        Ok(NativeStep::Return(root))
    }
}

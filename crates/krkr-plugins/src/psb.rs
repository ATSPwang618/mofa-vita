//! PSBFile plus its psb:// binary-resource medium.
pub(crate) mod decode;
mod storage;
use crate::exports::Exports;
use krkr_engine::{
    extensions,
    plugins::{Context, Plugin},
    storages,
};
use std::sync::{Arc, atomic::Ordering};
use tjs_bind::{Array, Dictionary, IntoTjs, RestArgs};
use tjs_core::{Heap, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value};

#[derive(Default, tjs_bind::Trace)]
pub(crate) struct Psb {
    exports: Exports,
    #[trace(skip = "registration owns only portable VFS data")]
    registration: Option<storage::Registration>,
}
krkr_engine::native_plugin! { impl Psb { names: ["psbfile.dll", "psbfile.tpm"] } }
impl Plugin for Psb {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        let registration = storage::Registration::new(storages::service_from_heap(cx.heap)?)?;
        let class = bindings::install_with_state(
            cx.heap,
            bindings::State {
                medium: Some(registration.medium.clone()),
                root: Value::Void,
            },
        )?;
        self.exports
            .value(cx, cx.global, "PSBFile", Value::Obj(class.into()))?;
        self.registration = Some(registration);
        Ok(())
    }
    fn unlink(&mut self, cx: &mut Context<'_>) -> NativeResult<bool> {
        self.exports.unlink(cx)?;
        self.registration = None;
        Ok(true)
    }
}
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
#[tjs_bind::class(name = "PSBFile")]
mod bindings {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State {
        pub(super) root: Value,
        #[trace(skip = "medium contains parsed PSB bytes without TJS values")]
        pub(super) medium: Option<Arc<storage::Medium>>,
    }
    impl State {
        #[tjs::constructor(resumable = true)]
        fn create(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            let class = cx
                .heap()
                .registered_class("PSBFile")
                .ok_or(NativeError::This)?;
            let medium = with_state(cx, class, |s| s.medium.clone())?;
            if args.is_empty() {
                return cx
                    .construct(Self {
                        root: Value::Void,
                        medium,
                    })
                    .map(NativeStep::Return);
            }
            if args.len() != 1 {
                return Err(NativeError::Message("PSBFile expects zero or one argument"));
            }
            start(cx, args[0], medium, true)
        }
        #[tjs::method(resumable = true)]
        fn load(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
            if args.len() != 1 {
                return Err(NativeError::Message("PSBFile.load expects one argument"));
            }
            // Only the constructor accepts octets in the reference. load(octet)
            // explicitly returns void without changing the existing document.
            if matches!(args[0], Value::Octet(_)) {
                return Ok(NativeStep::Return(Value::Void));
            }
            let medium = with_state(cx, cx.this(), |s| s.medium.clone())?;
            start(cx, args[0], medium, false)
        }
        #[tjs::getter]
        fn root(&self) -> Value {
            self.root
        }
        #[tjs::invalidate]
        fn invalidate(&mut self) {
            self.root = Value::Void;
            self.medium = None;
        }
        #[tjs::method]
        fn finalize(&self) {}
    }
}
enum Input {
    Bytes(Vec<u8>),
    Storage(krkr_engine::assets::ReadPlan),
}
fn start(
    cx: &mut NativeCx<'_>,
    value: Value,
    medium: Option<Arc<storage::Medium>>,
    construct: bool,
) -> NativeResult<NativeStep> {
    if let Value::Str(id) = value {
        let name = tjs_core::string::c_string(cx.heap().string(id)?).to_vec();
        let next = Loaded {
            owner: cx.this(),
            medium,
            construct,
            name: name.clone(),
        };
        return storages::managed::plans(cx, vec![(name, true)], next, |next, cx, mut plans| {
            start_input(
                cx,
                Input::Storage(plans.pop().flatten().expect("required PSB plan")),
                next,
            )
        });
    }
    let (input, name) = match value {
        Value::Octet(id) if construct => (Input::Bytes(cx.heap().octet(id)?.to_vec()), Vec::new()),
        _ => {
            return Err(NativeError::Message(
                "PSBFile requires a storage string or constructor octet",
            ));
        }
    };
    let next = Loaded {
        owner: cx.this(),
        medium,
        construct,
        name,
    };
    start_input(cx, input, next)
}
fn start_input(cx: &mut NativeCx<'_>, input: Input, next: Loaded) -> NativeResult<NativeStep> {
    extensions::run_work(
        cx,
        move |stop| {
            let bytes = match input {
                Input::Bytes(bytes) => bytes,
                Input::Storage(plan) => {
                    if plan.bytes > 64 * 1024 * 1024 {
                        return Ok(None);
                    }
                    plan.read_interruptible(0, || stop.load(Ordering::Acquire))
                        .map_err(error)?
                }
            };
            let doc = decode::decode(bytes, &|| stop.load(Ordering::Acquire))
                .ok()
                .map(Arc::new);
            if stop.load(Ordering::Acquire) {
                return Err(NativeError::Message("PSB loading cancelled"));
            }
            Ok(doc)
        },
        Box::new(next),
    )
}
#[derive(tjs_bind::Trace)]
struct Loaded {
    owner: ObjId,
    #[trace(skip = "medium contains no TJS handles")]
    medium: Option<Arc<storage::Medium>>,
    construct: bool,
    name: Vec<u16>,
}
impl extensions::WorkContinuation<Option<Arc<decode::Document>>> for Loaded {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        doc: Option<Arc<decode::Document>>,
    ) -> NativeResult<NativeStep> {
        let success = doc.is_some();
        let root = if let Some(doc) = &doc {
            let root = Metadata(&doc.root, &doc.bytes).into_tjs(cx.heap_mut())?;
            if !self.name.is_empty()
                && let Some(medium) = &self.medium
            {
                medium.remember(&self.name, doc.clone());
            }
            root
        } else {
            Value::Void
        };
        if self.construct {
            cx.construct(bindings::State {
                root,
                medium: self.medium,
            })
            .map(NativeStep::Return)
        } else {
            if success {
                bindings::with_state(cx, self.owner, |s| s.root = root)?;
            }
            Ok(NativeStep::Return(Value::Int(i64::from(success))))
        }
    }
}
pub(crate) struct Metadata<'a>(pub(crate) &'a decode::Node, pub(crate) &'a [u8]);
impl IntoTjs for Metadata<'_> {
    fn into_tjs(self, heap: &mut Heap) -> NativeResult<Value> {
        use decode::Node;
        Ok(match self.0 {
            Node::Void => Value::Void,
            Node::Int(n) => Value::Int(*n),
            Node::Real(n) => Value::Real(*n),
            Node::String(s) => s.clone().into_tjs(heap)?,
            Node::Bytes(range) => Value::Octet(heap.alloc_octet(self.1[range.clone()].to_vec())),
            Node::Array(items) => {
                Array(items.iter().map(|n| Metadata(n, self.1))).into_tjs(heap)?
            }
            Node::Object(items) => Dictionary(
                items
                    .iter()
                    .map(|(key, n)| (key.as_str(), Metadata(n, self.1))),
            )
            .into_tjs(heap)?,
        })
    }
}

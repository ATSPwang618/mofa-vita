//! Shared engine storage service. Script wrappers and language container I/O
//! use one VFS; the VFS itself contains no TJS handles.
pub mod image;
pub mod managed;
use krkr_assets::{Vfs, name, text};
use std::{cell::RefCell, rc::Rc};
use tjs_bind::{Heap, NativeCx, NativeError, NativeResult, Trace, Value};

pub use krkr_assets::{Limits, ReadPlan};
pub type Shared = Rc<RefCell<Vfs>>;

pub(crate) fn clear_cache(heap: &mut Heap) -> NativeResult<()> {
    if let Some(class) = heap.registered_class("Storages") {
        heap.with_native_state::<implementation::State, _>(class, |state| {
            state
                .service
                .as_ref()
                .expect("storage service")
                .borrow_mut()
                .clear_archive_cache()
        })?;
    }
    Ok(())
}
fn error(error: krkr_assets::Error) -> NativeError {
    NativeError::Detail(error.to_string())
}
fn units(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = tjs_core::value::to_string(cx.heap_mut(), value)? else {
        unreachable!()
    };
    Ok(name::c_string(cx.heap().string(id)?).to_vec())
}
fn string(cx: &mut NativeCx<'_>, text: Vec<u16>) -> Value {
    Value::Str(cx.heap_mut().alloc_string(text))
}
/// Shared VFS for host-registered plugins; release its borrow before callbacks.
pub fn service(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    service_from_heap(cx.heap_mut())
}
pub fn service_from_heap(heap: &mut Heap) -> NativeResult<Shared> {
    let class = heap
        .registered_class("Storages")
        .expect("installed Storages");
    heap.with_native_state::<implementation::State, _>(class, |state| {
        state
            .service
            .as_ref()
            .expect("installed storage service")
            .clone()
    })
}

struct Storage(Shared);
impl Trace for Storage {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl tjs_core::storage::Storage for Storage {
    fn delegate(&self) -> Option<tjs_core::storage::Delegate> {
        Some(managed::container_io)
    }
    fn max_read_bytes(&self) -> usize {
        self.0.borrow().limits().max_read_bytes
    }
    fn read_text(&mut self, name: &[u16], mode: &[u16]) -> NativeResult<Vec<u16>> {
        let bytes = self.read_binary(name, mode)?;
        text::decode(&bytes, &name::units("utf-8"), self.max_read_bytes()).map_err(error)
    }
    fn write_text(&mut self, name: &[u16], mode: &[u16], source: &[u16]) -> NativeResult<()> {
        let bytes = text::encode(source, mode, self.max_read_bytes()).map_err(error)?;
        self.write_binary(name, mode, &bytes)
    }
    fn read_binary(&mut self, name: &[u16], mode: &[u16]) -> NativeResult<Vec<u8>> {
        let offset = text::offset(mode).map_err(error)?.unwrap_or(0);
        let plan = self.0.borrow_mut().plan(name).map_err(error)?;
        // Release VFS state before IO or filters; the plan owns its inputs.
        plan.read(offset).map_err(error)
    }
    fn write_binary(&mut self, name: &[u16], mode: &[u16], bytes: &[u8]) -> NativeResult<()> {
        let offset = text::offset(mode).map_err(error)?;
        self.0
            .borrow_mut()
            .write(name, offset, bytes)
            .map_err(error)
    }
}

#[tjs_bind::class(name = "Storages", static_class = true)]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub service: Option<Shared>,
        pub(super) managed: managed::Managed,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.managed.trace(visit);
        }
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method(name = "addAutoPath", class_only = true)]
        fn add(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<()> {
            service(cx)?
                .borrow_mut()
                .add_path(&units(cx, path)?)
                .map_err(error)
        }
        #[tjs::method(name = "removeAutoPath", class_only = true)]
        fn remove(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<()> {
            service(cx)?
                .borrow_mut()
                .remove_path(&units(cx, path)?)
                .map_err(error)
        }
        #[tjs::method(name = "getFullPath", class_only = true)]
        fn full(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<Value> {
            let path = service(cx)?
                .borrow()
                .full_path(&units(cx, path)?)
                .map_err(error)?;
            Ok(string(cx, path))
        }
        #[tjs::method(name = "getPlacedPath", class_only = true, resumable = true)]
        fn placed(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<tjs_core::NativeStep> {
            let text = units(cx, path)?;
            managed::placed(
                cx,
                &text,
                tjs_bind::flow::callback((), |_, _, value| Ok(tjs_core::NativeStep::Return(value))),
            )
        }
        #[tjs::method(name = "isExistentStorage", class_only = true, resumable = true)]
        fn exists(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<tjs_core::NativeStep> {
            let text = units(cx, path)?;
            managed::placed(
                cx,
                &text,
                tjs_bind::flow::callback((), |_, cx, value| {
                    let Value::Str(id) = value else {
                        unreachable!()
                    };
                    Ok(tjs_core::NativeStep::Return(Value::Int(i64::from(
                        !cx.heap().string(id)?.is_empty(),
                    ))))
                }),
            )
        }
        #[tjs::method(name = "getLocalName", class_only = true)]
        fn local(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<Value> {
            let path = service(cx)?
                .borrow()
                .local_name(&units(cx, path)?)
                .map_err(error)?;
            Ok(string(cx, path))
        }
        #[tjs::method(name = "extractStorageExt", class_only = true)]
        fn ext(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<Value> {
            let text = units(cx, path)?;
            Ok(string(cx, name::split_ext(&text).1.to_vec()))
        }
        #[tjs::method(name = "extractStorageName", class_only = true)]
        fn name(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<Value> {
            let text = units(cx, path)?;
            Ok(string(cx, name::split_name(&text).1.to_vec()))
        }
        #[tjs::method(name = "extractStoragePath", class_only = true)]
        fn path(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<Value> {
            let text = units(cx, path)?;
            Ok(string(cx, name::split_name(&text).0.to_vec()))
        }
        #[tjs::method(name = "chopStorageExt", class_only = true)]
        fn chop(cx: &mut NativeCx<'_>, path: Value) -> NativeResult<Value> {
            let text = units(cx, path)?;
            Ok(string(cx, name::split_ext(&text).0.to_vec()))
        }
        #[tjs::method(name = "clearArchiveCache", class_only = true)]
        fn clear(cx: &mut NativeCx<'_>) -> NativeResult<()> {
            service(cx)?.borrow_mut().clear_archive_cache();
            Ok(())
        }
    }
}

pub fn install(heap: &mut Heap, vfs: Vfs) -> NativeResult<Shared> {
    let shared = Rc::new(RefCell::new(vfs));
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |state| {
        state.service = Some(shared.clone());
    })?;
    heap.set_storage(Storage(shared.clone()));
    Ok(shared)
}

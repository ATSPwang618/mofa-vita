mod bindings;
pub(crate) mod tasks;
use crate::operations;
pub(crate) use bindings::State;
use krkr_protocol::{graphics::LayerId, text::Font};
use std::{
    cell::RefCell,
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Mutex},
};
use tjs_core::{Heap, NativeCx, NativeError, NativeResult, ObjId, Trace, Value};
pub(crate) type Shared = Rc<Service>;
pub(crate) struct Service {
    pub worker: Arc<Mutex<krkr_render::font::System>>,
    pub operations: operations::Shared,
    pub mapped: RefCell<HashMap<Font, Mapping>>,
}
pub(crate) struct Mapping {
    pub source: krkr_assets::ReadPlan,
    pub font: Arc<krkr_render::font::prerendered::Font>,
}
pub(crate) struct Link {
    pub layers: crate::layer::Shared,
    pub id: LayerId,
    pub owner: ObjId,
}
impl Trace for Link {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
pub(crate) enum Target {
    Owned(Font),
    Layer(Link),
}
impl Default for Target {
    fn default() -> Self {
        Self::Owned(Font::default())
    }
}
impl Target {
    pub fn read<R>(&self, read: impl FnOnce(&Font) -> R) -> NativeResult<R> {
        match self {
            Self::Owned(f) => Ok(read(f)),
            Self::Layer(l) => crate::layer::font_settings(&l.layers, l.id, read),
        }
    }
    pub fn update(&mut self, f: impl FnOnce(&mut Font)) -> NativeResult<()> {
        match self {
            Self::Owned(font) => {
                f(font);
                Ok(())
            }
            Self::Layer(l) => crate::layer::update_font(&l.layers, l.id, f),
        }
    }
    pub fn require_main(&self) -> NativeResult<()> {
        if let Self::Layer(l) = self {
            crate::layer::font_require_main(&l.layers, l.id)?;
        }
        Ok(())
    }
}
pub(crate) fn service(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let class = cx.heap().registered_class("Font").expect("installed Font");
    cx.heap_mut()
        .with_native_state::<State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::Message("font service is unavailable"))
}
pub(crate) fn linked(heap: &mut Heap, service: Shared, link: Link) -> NativeResult<ObjId> {
    let class = heap.registered_class("Font").expect("installed Font");
    heap.alloc_native(
        class,
        State {
            service: Some(service),
            target: Target::Layer(link),
        },
    )
}
pub(crate) fn install(heap: &mut Heap, operations: operations::Shared) -> NativeResult<Shared> {
    let shared = Rc::new(Service {
        worker: Arc::new(Mutex::new(Default::default())),
        operations,
        mapped: RefCell::new(HashMap::new()),
    });
    let class = bindings::install(heap)?;
    heap.initialize_class_state::<State>(class)?;
    heap.with_native_state::<State, _>(class, |s| s.service = Some(shared.clone()))?;
    Ok(shared)
}

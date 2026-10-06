//! Host-registered transition factories, independent of concrete plugins.
use super::*;
use krkr_protocol::{budget::Budget, pixels::Pixels, transition::custom::Instance};

type Factory = fn(
    &mut NativeCx<'_>,
    Size,
    &[Value],
    Option<Arc<Pixels>>,
    Budget,
) -> NativeResult<Arc<dyn Instance>>;
pub struct Definition {
    pub name: &'static str,
    pub kernel: &'static str,
    pub options: &'static [&'static str],
    /// Optional RGBA rule image, resolved once through the managed image loader.
    /// The option accepts a storage name or a Layer snapshot.
    pub rule_option: Option<usize>,
    /// Called immediately after each option getter, preserving conversion order.
    pub convert: fn(&mut NativeCx<'_>, Size, usize, &[Value], Value) -> NativeResult<Value>,
    pub create: Factory,
}
pub(in crate::layer) struct Provider {
    pub definition: Definition,
    pub lifetime: Arc<()>,
}
pub struct Registration {
    shared: Shared,
    providers: Vec<Arc<Provider>>,
}
impl Trace for Registration {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.shared.borrow().trace_transitions(visit);
    }
}
impl Registration {
    pub fn idle(&self) -> bool {
        self.providers
            .iter()
            .all(|p| Arc::strong_count(p) == 2 && Arc::strong_count(&p.lifetime) == 1)
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        let mut world = self.shared.borrow_mut();
        for p in &self.providers {
            let name = p.definition.name;
            if world
                .transition_providers
                .get(name)
                .is_some_and(|other| Arc::ptr_eq(other, p))
            {
                world.transition_providers.remove(name);
            }
        }
    }
}
pub fn register(heap: &mut Heap, definitions: Vec<Definition>) -> NativeResult<Registration> {
    let class = heap.registered_class("Layer").ok_or(NativeError::This)?;
    let shared = heap
        .with_native_state::<bindings::State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::This)?;
    let mut world = shared.borrow_mut();
    let mut seen = HashSet::new();
    for d in &definitions {
        if d.name.is_empty()
            || d.name.contains('\0')
            || d.kernel.is_empty()
            || d.rule_option.is_some_and(|i| i >= d.options.len())
            || matches!(d.name, "crossfade" | "universal" | "scroll")
            || world.transition_providers.contains_key(d.name)
            || !seen.insert(d.name)
        {
            return Err(NativeError::Message(
                "duplicate or invalid transition provider",
            ));
        }
    }
    let providers: Vec<_> = definitions
        .into_iter()
        .map(|definition| {
            Arc::new(Provider {
                definition,
                lifetime: Arc::new(()),
            })
        })
        .collect();
    for p in &providers {
        world
            .transition_providers
            .insert(p.definition.name, p.clone());
    }
    drop(world);
    Ok(Registration { shared, providers })
}

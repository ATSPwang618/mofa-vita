//! Host-registered Rust plugins. Registration does not execute scripts or load
//! foreign machine code; plugin classes use the ordinary native binding API.
mod context;
mod declaration;
mod exports;
mod journal;
mod lifecycle;
mod patch;
pub use context::Context;
pub use declaration::register_class_alias;
pub use exports::Exports;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};
#[doc(hidden)]
pub use tjs_core as __tjs;
use tjs_core::{
    Heap, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, Trace, Value,
};

/// Registration hooks are native leaves: do not collect or enter the VM here.
/// Trace all retained TJS values, including state retained after a failed link.
/// Context stages exports until the hook succeeds. Resource registrations must
/// use owned guards so a failed link releases them; unlink must check all leases
/// before releasing resources and may return false while they are in use.
/// Drop only releases Rust resources and must not execute scripts.
pub trait Plugin: Trace {
    /// Portable providers required before this provider's registration hook.
    fn dependencies(&self) -> &'static [&'static str] {
        &[]
    }
    fn link(&mut self, context: &mut Context<'_>) -> NativeResult<()>;
    /// Side-effect-free lease check before script unload hooks. Unlink must
    /// still check again before releasing resources.
    fn can_unlink(&self, _context: &Context<'_>) -> NativeResult<bool> {
        Ok(true)
    }
    fn unlink(&mut self, context: &mut Context<'_>) -> NativeResult<bool>;
}
type Provider = Rc<RefCell<Box<dyn Plugin>>>;
#[derive(Default)]
struct Identity {
    native: Option<Provider>,
    patches: BTreeMap<i64, Rc<patch::Spec>>,
    active: Option<Active>,
}
struct Installed {
    spec: Rc<patch::Spec>,
    exports: journal::Journal,
}
#[derive(Default)]
struct Active {
    serial: u64,
    complete: bool,
    native: Option<journal::Journal>,
    native_complete: bool,
    patches: Vec<Installed>,
    dependencies: Vec<usize>,
}
impl Active {
    fn journals(&self) -> impl Iterator<Item = &journal::Journal> {
        self.native
            .iter()
            .chain(self.patches.iter().map(|p| &p.exports))
    }
}
struct Loaded {
    name: Vec<u16>,
    provider: usize,
}
#[derive(Default)]
struct Registry {
    names: BTreeMap<Vec<u16>, usize>,
    providers: Vec<Identity>,
    loaded: Vec<Loaded>,
    global: Option<ObjId>,
    busy: bool,
    serial: u64,
    linking: Option<usize>,
    abandoned: Vec<usize>,
}
impl Trace for Registry {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for plugin in &self.providers {
            if let Some(native) = &plugin.native {
                native.borrow().trace(visit);
            }
            for spec in plugin.patches.values() {
                spec.trace(visit);
            }
            if let Some(active) = &plugin.active {
                for journal in active.journals() {
                    journal.trace(visit);
                }
            }
        }
        if let Some(global) = self.global {
            visit(Value::Obj(global.into()));
        }
    }
}
type Shared = Rc<RefCell<Registry>>;
fn service(cx: &mut NativeCx<'_>) -> NativeResult<Shared> {
    let class = cx
        .heap()
        .registered_class("Plugins")
        .expect("installed class");
    cx.heap_mut()
        .with_native_state::<implementation::State, _>(class, |s| s.registry.clone())
}

/// The engine global associated with this heap's plugin registry. Providers
/// read its properties through NativeStep so script accessors remain observable.
pub fn global(cx: &mut NativeCx<'_>) -> NativeResult<ObjId> {
    service(cx)?
        .borrow()
        .global
        .ok_or(NativeError::Message("Plugins requires an engine global"))
}
fn name(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = tjs_core::value::to_string(cx.heap_mut(), value)? else {
        unreachable!()
    };
    Ok(tjs_core::string::c_string(cx.heap().string(id)?).to_vec())
}
fn key(name: &[u16]) -> Vec<u16> {
    name.iter()
        .map(|&c| if (65..=90).contains(&c) { c + 32 } else { c })
        .collect()
}
struct Busy {
    registry: Shared,
    link: bool,
}
impl Trace for Busy {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.registry.borrow().trace(visit);
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        let mut r = self.registry.borrow_mut();
        if self.link
            && let Some(id) = r.linking.take()
        {
            r.abandoned.push(id);
        }
        r.busy = false;
    }
}

#[tjs_bind::class(name = "Plugins", static_class = true)]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub registry: Shared,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.registry.borrow().trace(visit);
        }
    }
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self::default()
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method(class_only = true, resumable = true)]
        fn link(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            lifecycle::link(cx, value)
        }
        #[tjs::method(class_only = true, resumable = true)]
        fn unlink(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            lifecycle::unlink(cx, value)
        }
        #[tjs::method(class_only = true)]
        fn mock(cx: &mut NativeCx<'_>, args: tjs_bind::RestArgs<'_>) -> NativeResult<()> {
            patch::register(
                cx,
                *args.first().ok_or(NativeError::Missing(0))?,
                args.get(1).copied().unwrap_or(Value::Void),
                true,
            )
        }
        #[tjs::method(class_only = true)]
        fn patch(cx: &mut NativeCx<'_>, name: Value, spec: Value) -> NativeResult<()> {
            patch::register(cx, name, spec, false)
        }
        #[tjs::method(class_only = true)]
        fn alias(cx: &mut NativeCx<'_>, name: Value, target: Value) -> NativeResult<()> {
            patch::alias(cx, name, target)
        }
        #[tjs::method(name = "getList", class_only = true)]
        fn list(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
            let registry = service(cx)?;
            let values = registry
                .borrow()
                .loaded
                .iter()
                .map(|p| Value::Str(cx.heap_mut().alloc_string(p.name.clone())))
                .collect::<Vec<_>>();
            Ok(Value::Obj(ObjRef::bound(
                cx.heap_mut().alloc_array_from(&values)?,
            )))
        }
    }
}
pub fn install(heap: &mut Heap) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    Ok(())
}
/// A provider may expose several explicitly registered legacy names. Multiple
/// linked names retain one Rust provider until the last name is unlinked.
pub fn register(
    heap: &mut Heap,
    names: &[&str],
    plugin: impl Plugin + 'static,
) -> NativeResult<()> {
    install(heap)?;
    let class = heap.registered_class("Plugins").expect("installed class");
    let registry =
        heap.with_native_state::<implementation::State, _>(class, |s| s.registry.clone())?;
    let mut registry = registry.borrow_mut();
    if registry.busy {
        return Err(NativeError::Message(
            "cannot register a plugin during a plugin hook",
        ));
    }
    let mut keys = BTreeSet::new();
    for name in names {
        let key = key(&name.encode_utf16().collect::<Vec<_>>());
        if key.is_empty()
            || key.contains(&0)
            || registry.names.contains_key(&key)
            || !keys.insert(key)
        {
            return Err(NativeError::Message(
                "plugin alias is empty, contains NUL, or is duplicated",
            ));
        }
    }
    if keys.is_empty() {
        return Err(NativeError::Message("plugin requires at least one name"));
    }
    let id = registry.providers.len();
    registry.providers.push(Identity {
        native: Some(Rc::new(RefCell::new(Box::new(plugin)))),
        ..Identity::default()
    });
    for key in keys {
        registry.names.insert(key, id);
    }
    Ok(())
}
pub(crate) fn attach(heap: &mut Heap, global: ObjId) -> NativeResult<()> {
    let class = heap.registered_class("Plugins").expect("installed class");
    heap.with_native_state::<implementation::State, _>(class, |s| {
        s.registry.borrow_mut().global = Some(global)
    })?;
    Ok(())
}

pub(crate) fn cancelled(heap: &mut Heap) {
    let Some(class) = heap.registered_class("Plugins") else {
        return;
    };
    let result = heap
        .with_native_state::<implementation::State, _>(class, |s| s.registry.clone())
        .and_then(|registry| lifecycle::recover(heap, &registry));
    if let Err(error) = result {
        krkr_protocol::log!(Warn, "plugin cancellation cleanup: {error}");
    }
}

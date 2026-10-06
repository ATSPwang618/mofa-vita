//! VM-local draw-device handles. IDs never contain addresses; the registry has
//! weak object identities, while a bound Window keeps its device alive.
mod basic;
use super::bindings::State;
pub(super) use basic::{create_default, install};
use std::collections::BTreeMap;
use tjs_bind::flow;
use tjs_core::{
    Heap, NativeCallable, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, Trace,
    Value, value,
};

#[derive(Default)]
struct Registry {
    next: i64,
    objects: BTreeMap<i64, ObjId>,
}
impl Trace for Registry {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
struct Interface {
    handle: i64,
    attach: NativeCallable,
    window: Option<ObjId>,
}
impl Trace for Interface {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
#[derive(Default, tjs_bind::Trace)]
struct Binding {
    handle: i64,
    object: Value,
    device: Value,
    revision: u64,
}

pub fn register(heap: &mut Heap, owner: ObjId, attach: NativeCallable) -> NativeResult<i64> {
    if let Ok(handle) = heap.with_native_state::<Interface, _>(owner, |s| s.handle) {
        return Ok(handle);
    }
    let class = heap.registered_class("Window").ok_or(NativeError::This)?;
    heap.initialize_native_default::<Registry>(class)?;
    let entries = heap.with_native_state::<Registry, _>(class, |s| s.objects.clone())?;
    let expired = entries
        .into_iter()
        .filter_map(|(handle, owner)| (!heap.is_valid(owner).unwrap_or(false)).then_some(handle))
        .collect::<Vec<_>>();
    let handle = heap.with_native_state::<Registry, _>(class, |s| {
        for handle in expired {
            s.objects.remove(&handle);
        }
        s.next = s
            .next
            .checked_add(1)
            .ok_or(NativeError::Message("draw device handles exhausted"))?;
        s.objects.insert(s.next, owner);
        Ok::<_, NativeError>(s.next)
    })??;
    heap.initialize_native_state(
        owner,
        Interface {
            handle,
            attach,
            window: None,
        },
    )?;
    Ok(handle)
}
pub(super) fn get(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    let owner = cx.this();
    super::bindings::id(cx.heap_mut(), owner)?;
    cx.heap_mut().initialize_native_default::<Binding>(owner)?;
    cx.heap_mut()
        .with_native_state::<Binding, _>(owner, |s| s.object)
}
fn callback(heap: &mut Heap, object: ObjId) -> NativeResult<Value> {
    let call = heap.with_native_state::<Interface, _>(object, |s| s.attach)?;
    let function = heap.alloc_native_function(call);
    Ok(Value::Obj(ObjRef {
        object: Some(function),
        this: Some(object),
    }))
}
pub(super) fn set(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
    let window = cx.this();
    super::bindings::id(cx.heap_mut(), window)?;
    if let Value::Obj(reference) = value {
        if reference.object.is_some() {
            let key = Value::Str(
                cx.heap_mut()
                    .alloc_string("interface".encode_utf16().collect::<Vec<_>>()),
            );
            return Ok(NativeStep::Get {
                object: value,
                key,
                continuation: flow::callback((window, value), |(window, object), cx, interface| {
                    let handle = value::to_integer(cx.heap(), interface)?;
                    bind(cx, window, object, handle)
                }),
            });
        }
        return bind(cx, window, Value::Void, 0);
    }
    // Retain VM-local handle assignment for existing managed providers. A read
    // always returns the device object, never an address or backend identifier.
    bind(
        cx,
        window,
        Value::Void,
        value::to_integer(cx.heap(), value)?,
    )
}
fn bind(
    cx: &mut NativeCx<'_>,
    window: ObjId,
    object: Value,
    handle: i64,
) -> NativeResult<NativeStep> {
    super::bindings::id(cx.heap_mut(), window)?;
    let owner = if handle == 0 {
        None
    } else {
        let class = cx
            .heap()
            .registered_class("Window")
            .ok_or(NativeError::This)?;
        cx.heap_mut().initialize_native_default::<Registry>(class)?;
        let owner = cx
            .heap_mut()
            .with_native_state::<Registry, _>(class, |s| s.objects.get(&handle).copied())?
            .ok_or(NativeError::Message("unknown draw device interface"))?;
        if !cx.heap().is_valid(owner)? {
            return Err(NativeError::Message("expired draw device interface"));
        }
        Some(owner)
    };
    cx.heap_mut().initialize_native_default::<Binding>(window)?;
    let object = if matches!(object, Value::Void) {
        owner.map_or(Value::Void, |owner| Value::Obj(ObjRef::bound(owner)))
    } else {
        object
    };
    let (old_handle, old, old_object) = cx
        .heap_mut()
        .with_native_state::<Binding, _>(window, |s| (s.handle, s.device, s.object))?;
    if old_handle == handle {
        cx.heap_mut()
            .with_native_state::<Binding, _>(window, |s| s.object = object)?;
        return Ok(NativeStep::Return(Value::Void));
    }
    let mut calls = Vec::new();
    if let Some(owner) = owner {
        let previous = cx
            .heap_mut()
            .with_native_state::<Interface, _>(owner, |s| s.window)?;
        if let Some(previous) = previous.filter(|&old| old != window) {
            detach(cx, Value::Obj(previous.into()), owner)?;
        }
        cx.heap_mut()
            .with_native_state::<Interface, _>(owner, |s| s.window = Some(window))?;
    }
    if let Value::Obj(r) = old
        && let Some(old) = r.object
        && cx.heap().is_valid(old)?
    {
        cx.heap_mut()
            .with_native_state::<Interface, _>(old, |s| s.window = None)?;
        calls.push(Action::Call(callback(cx.heap_mut(), old)?, Value::Void));
        calls.push(Action::Invalidate(old_object));
    }
    if let Some(owner) = owner {
        calls.push(Action::Call(
            callback(cx.heap_mut(), owner)?,
            Value::Obj(window.into()),
        ));
    }
    cx.heap_mut().with_native_state::<Binding, _>(window, |s| {
        s.handle = handle;
        s.object = object;
        s.device = owner.map_or(Value::Void, |o| Value::Obj(o.into()));
        s.revision = s.revision.wrapping_add(1);
    })?;
    let (world, id) = world(cx, Value::Obj(window.into()))?;
    world.borrow_mut().set_device_input(id, None);
    world.borrow_mut().clear_device_frame(id);
    invoke(calls, 0)
}
/// Release the window's frame and binding when a device dies or moves to a
/// different window. Does not invoke a dying native receiver recursively.
pub fn detach(cx: &mut NativeCx<'_>, window: Value, device: ObjId) -> NativeResult<()> {
    let Value::Obj(r) = window else {
        return Ok(());
    };
    let Some(owner) = r.object else {
        return Ok(());
    };
    let removed = cx
        .heap_mut()
        .with_native_state::<Binding, _>(owner, |s| {
            if !matches!(s.device, Value::Obj(r) if r.object == Some(device)) {
                return false;
            }
            s.device = Value::Void;
            s.object = Value::Void;
            s.handle = 0;
            s.revision = s.revision.wrapping_add(1);
            true
        })
        .unwrap_or(false);
    if removed && let Ok((world, id)) = world(cx, window) {
        world.borrow_mut().set_device_input(id, None);
        world.borrow_mut().clear_device_frame(id);
    }
    let _ = cx
        .heap_mut()
        .with_native_state::<Interface, _>(device, |s| {
            if s.window == Some(owner) {
                s.window = None;
            }
        });
    Ok(())
}
#[derive(Clone, Copy, tjs_bind::Trace)]
enum Action {
    Call(Value, Value),
    Invalidate(Value),
}
fn invoke(calls: Vec<Action>, index: usize) -> NativeResult<NativeStep> {
    let Some(&action) = calls.get(index) else {
        return Ok(NativeStep::Return(Value::Void));
    };
    let continuation = flow::callback((calls, index), |(calls, i), _, _| invoke(calls, i + 1));
    Ok(match action {
        Action::Call(function, window) => NativeStep::CallDiscard {
            function,
            arguments: vec![window],
            continuation,
        },
        Action::Invalidate(object) => NativeStep::Invalidate {
            object,
            continuation,
        },
    })
}
pub fn roots(cx: &mut NativeCx<'_>, window: Value) -> NativeResult<Vec<Value>> {
    let (world, id) = world(cx, window)?;
    Ok(world.borrow().root_objects(id))
}
pub fn input_manager(cx: &mut NativeCx<'_>, window: Value, index: usize) -> NativeResult<()> {
    if matches!(window, Value::Void) {
        return Ok(());
    }
    let (world, id) = world(cx, window)?;
    world.borrow_mut().set_device_input(id, Some(index));
    Ok(())
}
pub(crate) fn world(
    cx: &mut NativeCx<'_>,
    window: Value,
) -> NativeResult<(crate::layer::Shared, krkr_protocol::window::WindowId)> {
    let Value::Obj(r) = window else {
        return Err(NativeError::This);
    };
    let (world, id) = cx.heap_mut().with_native_state::<State, _>(
        r.object.ok_or(NativeError::This)?,
        |s| {
            let lease = s.lease()?;
            let layers = lease
                .shared
                .borrow()
                .layers
                .upgrade()
                .ok_or(NativeError::This)?;
            Ok::<_, NativeError>((layers, lease.id))
        },
    )??;
    Ok((world, id))
}
pub(crate) fn begin_frame(
    cx: &mut NativeCx<'_>,
    window: ObjId,
    device: ObjId,
) -> NativeResult<u64> {
    cx.heap_mut().with_native_state::<Binding, _>(window, |s| {
        if !matches!(s.device, Value::Obj(r) if r.object == Some(device)) {
            return Err(NativeError::Message(
                "draw device is not bound to this window",
            ));
        }
        s.revision = s.revision.wrapping_add(1);
        Ok(s.revision)
    })?
}

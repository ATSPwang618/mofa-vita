//! Desktop queries share the engine operation queue and cancellation lifetime.
use super::{Delivery, bindings::State};
use crate::operations::{Operations, Request};
use krkr_protocol::window::{Command, Rectangle, Response, WindowId, desktop};
use tjs_core::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef, Trace,
    Value, WaitMode,
};

pub fn window_id(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<WindowId> {
    let Value::Obj(ObjRef {
        object: Some(owner),
        ..
    }) = value
    else {
        return Err(NativeError::Type("a non-null Window object"));
    };
    super::bindings::id(cx.heap_mut(), owner)
}
pub fn request(cx: &mut NativeCx<'_>, command: desktop::Command) -> NativeResult<NativeStep> {
    let class = cx
        .heap()
        .registered_class("Window")
        .ok_or(NativeError::Message("Window is not installed"))?;
    let shared = cx
        .heap_mut()
        .with_native_state::<State, _>(class, |s| s.service.clone())?
        .ok_or(NativeError::Message("window service is unavailable"))?;
    let (host, operations) = {
        let world = shared.borrow();
        (
            world.host.clone().ok_or(NativeError::Message(
                "desktop operation requires a platform host",
            ))?,
            world.operations.clone(),
        )
    };
    let window = match &command {
        desktop::Command::Monitor {
            target: desktop::MonitorTarget::Window(window),
            ..
        }
        | desktop::Command::Clip(desktop::Clip::Window(window))
        | desktop::Command::Corner { window, .. }
        | desktop::Command::Ime { window, .. } => Some(*window),
        _ => None,
    };
    let owner = window.and_then(|id| shared.borrow().owner(id));
    let ticket = host
        .request(WindowId::default(), Command::Desktop(command))
        .map_err(NativeError::Detail)?;
    let delivery = Delivery::default();
    Operations::wait(
        &operations,
        Request::Window(ticket, delivery.clone()),
        WaitMode::Internal,
        Box::new(Readback { delivery, owner }),
    )
}
struct Readback {
    delivery: Delivery,
    owner: Option<tjs_core::ObjId>,
}
impl Trace for Readback {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Readback {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let response = self
            .delivery
            .borrow_mut()
            .take()
            .ok_or(NativeError::Message("missing desktop response"))?;
        let Response::Desktop(response) = response else {
            return Err(NativeError::Message("unexpected desktop response"));
        };
        let heap = cx.heap_mut();
        let value = match response {
            desktop::Response::Void => Value::Void,
            desktop::Response::Bool(value) => Value::Int(value.into()),
            desktop::Response::Integer(value) => Value::Int(value),
            desktop::Response::Point(None)
            | desktop::Response::Monitor(None)
            | desktop::Response::Monitors(None) => Value::Void,
            desktop::Response::Point(Some((x, y))) => dictionary(
                heap,
                &[("x", Value::Int(x.into())), ("y", Value::Int(y.into()))],
            )?,
            desktop::Response::Monitor(Some(monitor)) => monitor_value(heap, monitor)?,
            desktop::Response::Monitors(Some(monitors)) => {
                let mut values = Vec::with_capacity(monitors.len());
                for (monitor, intersection) in monitors {
                    let value = monitor_value(heap, monitor)?;
                    let Value::Obj(reference) = value else {
                        unreachable!()
                    };
                    let intersection = rectangle_value(heap, intersection)?;
                    let key = heap.intern_str("intersect");
                    heap.set_member(reference.object.unwrap(), key, intersection)?;
                    values.push(value);
                }
                Value::Obj(ObjRef::bound(heap.alloc_array_from(&values)?))
            }
        };
        Ok(NativeStep::Return(value))
    }
}
pub fn dictionary(heap: &mut Heap, members: &[(&str, Value)]) -> NativeResult<Value> {
    let object = heap.alloc_dictionary();
    for (name, value) in members {
        let key = heap.intern_str(name);
        heap.set_member(object, key, *value)?;
    }
    Ok(Value::Obj(ObjRef::bound(object)))
}
pub fn rectangle_value(heap: &mut Heap, rect: Rectangle) -> NativeResult<Value> {
    dictionary(
        heap,
        &[
            ("x", Value::Int(rect.x.into())),
            ("y", Value::Int(rect.y.into())),
            ("w", Value::Int(rect.width.into())),
            ("h", Value::Int(rect.height.into())),
        ],
    )
}
fn monitor_value(heap: &mut Heap, value: desktop::Monitor) -> NativeResult<Value> {
    let name = Value::Str(heap.alloc_string(value.name.encode_utf16().collect::<Vec<_>>()));
    let monitor = rectangle_value(heap, value.monitor)?;
    // windowEx always returns a rectangle here. Platforms without a separate
    // usable-work-area query use the monitor bounds as their placement area.
    let work = rectangle_value(heap, value.work.unwrap_or(value.monitor))?;
    dictionary(
        heap,
        &[
            ("name", name),
            ("primary", Value::Int(value.primary.into())),
            ("monitor", monitor),
            ("work", work),
        ],
    )
}

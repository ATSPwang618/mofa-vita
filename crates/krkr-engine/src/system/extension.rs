//! Portable equivalents of engine services exported by windowEx.
use super::bindings::service;
use tjs_core::{NativeContinuation, NativeCx, NativeResult, NativeStep, Trace, Value, WaitMode};

pub fn encode_image(
    cx: &mut NativeCx<'_>,
    request: krkr_image::export::Request,
) -> NativeResult<NativeStep> {
    let delivery = crate::io::Delivery::default();
    crate::operations::Operations::wait(
        &service(cx)?.borrow().operations,
        crate::operations::Request::Read(
            Box::new(crate::io::Work::ImageExport(request)),
            delivery.clone(),
        ),
        WaitMode::Internal,
        tjs_bind::flow::callback(ImageResult(delivery), |s, cx, _| {
            let Some(crate::io::Data::Exported(data)) = s.0.borrow_mut().take() else {
                return Err(tjs_core::NativeError::Message(
                    "missing image export response",
                ));
            };
            let value = if let Some(bytes) = data {
                let (data, _permit) = bytes.into_parts();
                Value::Octet(cx.heap_mut().alloc_octet(data))
            } else {
                Value::Void
            };
            Ok(NativeStep::Return(value))
        }),
    )
}
struct ImageResult(crate::io::Delivery);
impl Trace for ImageResult {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}

/// Native continuous hooks share pacing, event gates and exception handling
/// with System handlers. Drop unregisters without running script.
pub struct Continuous {
    shared: super::Shared,
    function: Value,
}
impl Trace for Continuous {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.function.trace(visit);
    }
}
impl Drop for Continuous {
    fn drop(&mut self) {
        self.shared.borrow_mut().remove(self.function);
    }
}
impl Continuous {
    pub fn active(&self) -> bool {
        self.shared
            .borrow()
            .handlers
            .iter()
            .any(|v| matches!((v,self.function),(Some(Value::Obj(a)),Value::Obj(b)) if *a==b))
    }
}
pub fn continuous(cx: &mut NativeCx<'_>, function: Value) -> NativeResult<Continuous> {
    let shared = service(cx)?;
    shared.borrow_mut().add(function)?;
    Ok(Continuous { shared, function })
}

pub fn breathe(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let shared = service(cx)?;
    let (operations, deadline) = {
        let mut system = shared.borrow_mut();
        system.event_disabled = true;
        system.breathing = true;
        (system.operations.clone(), system.clock.now())
    };
    // TVPBreathe pumps host messages with engine events disabled. The desktop
    // host runs independently; an internal wait yields without script reentry.
    crate::operations::Operations::wait(
        &operations,
        crate::operations::Request::Delay(deadline),
        WaitMode::Internal,
        Box::new(Returned(shared)),
    )
}
pub fn is_breathing(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    Ok(Value::Int(service(cx)?.borrow().breathing.into()))
}
struct Returned(super::Shared);
impl Drop for Returned {
    fn drop(&mut self) {
        let mut system = self.0.borrow_mut();
        system.breathing = false;
        // The original resets this flag even if it was already true on entry.
        system.event_disabled = false;
    }
}
impl Trace for Returned {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Returned {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Void))
    }
}
pub fn clear_graphic_cache(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    service(cx)?.borrow().operations.borrow().images.clear();
    Ok(Value::Void)
}
pub fn log_eval_error(
    cx: &mut NativeCx<'_>,
    error: Value,
    continuation: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    crate::debug::on_error(cx.heap_mut())?;
    crate::debug::native_error(cx, error, continuation)
}
pub(crate) fn operations(
    cx: &mut tjs_core::NativeCx<'_>,
) -> tjs_core::NativeResult<crate::operations::Shared> {
    Ok(super::bindings::service(cx)?.borrow().operations.clone())
}
/// The engine's monotonic animation clock, independent of desktop APIs.
pub fn tick_count(cx: &mut tjs_core::NativeCx<'_>) -> tjs_core::NativeResult<u64> {
    Ok(service(cx)?.borrow().clock.now().as_millis() as u64)
}

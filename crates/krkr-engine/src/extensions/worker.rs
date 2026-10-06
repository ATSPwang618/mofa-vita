//! Owned plugin work uses the engine's bounded IO queue and cancellation lease.
use crate::{
    io,
    operations::{Operations, Request},
};
use std::{any::Any, sync::atomic::AtomicBool};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, WaitMode,
};
pub trait WorkContinuation<T>: Trace {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, result: T) -> NativeResult<NativeStep>;
}
pub(crate) trait Job: Send {
    fn run(self: Box<Self>, cancelled: &AtomicBool) -> NativeResult<Box<dyn Any + Send>>;
}
struct Task<F>(F);
impl<T: Send + 'static, F: FnOnce(&AtomicBool) -> NativeResult<T> + Send> Job for Task<F> {
    fn run(self: Box<Self>, cancelled: &AtomicBool) -> NativeResult<Box<dyn Any + Send>> {
        (self.0)(cancelled).map(|data| Box::new(data) as Box<dyn Any + Send>)
    }
}
struct Reply<T> {
    delivery: io::Delivery,
    next: Box<dyn WorkContinuation<T>>,
}
impl<T> Trace for Reply<T> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl<T: Send + 'static> NativeContinuation for Reply<T> {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::Extension(data)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected plugin work response"));
        };
        let data = data
            .downcast::<T>()
            .map_err(|_| NativeError::Message("plugin response type mismatch"))?;
        self.next.resume(cx, *data)
    }
}
pub fn run_work<T: Send + 'static>(
    cx: &mut NativeCx<'_>,
    work: impl FnOnce(&AtomicBool) -> NativeResult<T> + Send + 'static,
    next: Box<dyn WorkContinuation<T>>,
) -> NativeResult<NativeStep> {
    let operations = crate::system::extension::operations(cx)?;
    let delivery = io::Delivery::default();
    Operations::wait(
        &operations,
        Request::Read(
            Box::new(io::Work::Extension(Box::new(Task(work)))),
            delivery.clone(),
        ),
        WaitMode::Internal,
        Box::new(Reply { delivery, next }),
    )
}

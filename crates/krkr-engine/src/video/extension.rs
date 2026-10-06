//! Owned media operations for host-installed plugins. No VideoOverlay object
//! or codec implementation is exposed to the caller.
use super::*;
use crate::{
    io,
    operations::{Operations, Request},
};
pub use krkr_video::Handle;
use tjs_core::WaitMode;

pub trait Opened: Trace {
    fn opened(self: Box<Self>, cx: &mut NativeCx<'_>, handle: Handle) -> NativeResult<NativeStep>;
}
struct Receive {
    delivery: io::Delivery,
    next: Box<dyn Opened>,
}
struct ResolveScale {
    plan: krkr_assets::ReadPlan,
    next: Box<dyn Opened>,
}
impl Trace for ResolveScale {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl Trace for Receive {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl NativeContinuation for Receive {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::Video(handle)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("missing movie open completion"));
        };
        self.next.opened(cx, handle)
    }
}
pub fn open(
    cx: &mut NativeCx<'_>,
    path: &[u16],
    next: Box<dyn Opened>,
) -> NativeResult<NativeStep> {
    crate::storages::managed::plans(
        cx,
        vec![(path.to_vec(), true)],
        next,
        |next, cx, mut plans| {
            let plan = plans.pop().flatten().expect("required movie plan");
            let path = [
                plan.name.clone(),
                krkr_assets::name::units(krkr_image::scale::SUFFIX),
            ]
            .concat();
            crate::storages::managed::plans(
                cx,
                vec![(path, false)],
                ResolveScale { plan, next },
                |resolved, cx, mut plans| {
                    let shared = bindings::service(cx)?;
                    let w = shared.borrow();
                    let delivery = io::Delivery::default();
                    Operations::wait(
                        &w.operations,
                        Request::Read(
                            Box::new(io::Work::VideoOpen {
                                plan: resolved.plan,
                                scale: plans.pop().flatten(),
                                service: w.backend.clone(),
                                silent: true,
                            }),
                            delivery.clone(),
                        ),
                        WaitMode::Internal,
                        Box::new(Receive {
                            delivery,
                            next: resolved.next,
                        }),
                    )
                },
            )
        },
    )
}
pub fn rewind(
    cx: &mut NativeCx<'_>,
    handle: Handle,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let shared = bindings::service(cx)?;
    Operations::wait(
        &shared.borrow().operations,
        Request::Read(
            Box::new(io::Work::VideoSeek(handle, 0.0)),
            io::Delivery::default(),
        ),
        WaitMode::Internal,
        next,
    )
}

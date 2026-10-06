use crate::events::ActionOwner;
use crate::timer_queue::{TimerId, Timers};
use std::{cell::RefCell, rc::Rc};
use tjs_bind::{Heap, NativeCx, NativeError, NativeResult, NativeStep, RestArgs, Trace, Value};
use tjs_core::value;

pub(crate) type Shared = Rc<RefCell<Timers>>;
struct Lease {
    id: TimerId,
    timers: Shared,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.timers.borrow_mut().remove(self.id);
    }
}

#[tjs_bind::class(name = "Timer")]
mod implementation {
    use super::*;
    #[derive(Default)]
    pub struct State {
        pub service: Option<Shared>,
        lease: Option<Lease>,
        action: ActionOwner,
    }
    impl Trace for State {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            self.action.trace(visit);
        }
    }
    impl State {
        fn lease(&self) -> NativeResult<&Lease> {
            self.lease.as_ref().ok_or(NativeError::This)
        }
        #[tjs::constructor]
        fn new(cx: &mut NativeCx<'_>, owner: Value, args: RestArgs<'_>) -> NativeResult<Self> {
            let action = ActionOwner::new(cx, owner, args.first().copied(), "onTimer")?;
            let class = cx
                .heap()
                .registered_class("Timer")
                .expect("installed Timer");
            let timers = cx
                .heap_mut()
                .with_native_state::<State, _>(class, |state| state.service.clone())?
                .ok_or(NativeError::Message("Timer requires an engine event pump"))?;

            let id = timers.borrow_mut().insert(cx.this())?;
            Ok(Self {
                service: None,
                lease: Some(Lease { id, timers }),
                action,
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method(name = "onTimer", resumable = true)]
        fn on_timer(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.lease()?;
            self.action.invoke(cx)
        }
        #[tjs::getter(name = "interval")]
        fn interval(&self) -> NativeResult<f64> {
            let lease = self.lease()?;
            Ok(lease.timers.borrow().get(lease.id).interval as f64 / 65536.0)
        }
        #[tjs::setter(name = "interval")]
        fn set_interval(&mut self, value: f64) -> NativeResult<()> {
            let lease = self.lease()?;
            lease
                .timers
                .borrow_mut()
                .interval(lease.id, (value * 65536.0 + 0.5) as i64 as u64);
            Ok(())
        }
        #[tjs::getter(name = "enabled")]
        fn enabled(&self) -> NativeResult<bool> {
            let lease = self.lease()?;
            Ok(lease.timers.borrow().get(lease.id).enabled)
        }
        #[tjs::setter(name = "enabled")]
        fn set_enabled(&mut self, value: bool) -> NativeResult<()> {
            let lease = self.lease()?;
            lease.timers.borrow_mut().enable(lease.id, value);
            Ok(())
        }
        #[tjs::getter(name = "capacity")]
        fn capacity(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(i64::from(lease.timers.borrow().get(lease.id).capacity))
        }
        #[tjs::setter(name = "capacity")]
        fn set_capacity(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            lease
                .timers
                .borrow_mut()
                .set_capacity(lease.id, value as i32)
        }
        #[tjs::getter(name = "mode")]
        fn mode(&self) -> NativeResult<i64> {
            let lease = self.lease()?;
            Ok(i64::from(lease.timers.borrow().get(lease.id).mode))
        }
        #[tjs::setter(name = "mode")]
        fn set_mode(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let value = value::to_integer(cx.heap(), value)?;
            let lease = self.lease()?;
            lease.timers.borrow_mut().mode(lease.id, value as i32);
            Ok(())
        }
    }
}
pub(crate) fn install(heap: &mut Heap, service: Shared) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |state| {
        state.service = Some(service)
    })?;
    Ok(())
}

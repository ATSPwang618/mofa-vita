use crate::events::{self, ActionOwner, Kind, SourceId};
use tjs_bind::{Heap, NativeCx, NativeError, NativeResult, NativeStep, RestArgs, Trace, Value};

struct Lease {
    source: SourceId,
    events: events::Shared,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.events.borrow_mut().remove(self.source);
    }
}
#[tjs_bind::class(name = "AsyncTrigger")]
mod implementation {
    use super::*;
    pub struct State {
        pub service: Option<events::Shared>,
        lease: Option<Lease>,
        action: ActionOwner,
        cached: bool,
        mode: i32,
    }
    impl Default for State {
        fn default() -> Self {
            Self {
                service: None,
                lease: None,
                action: ActionOwner::default(),
                cached: true,
                mode: 0,
            }
        }
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
            let action = ActionOwner::new(cx, owner, args.first().copied(), "onFire")?;
            let class = cx
                .heap()
                .registered_class("AsyncTrigger")
                .expect("installed class");
            let events = cx
                .heap_mut()
                .with_native_state::<State, _>(class, |s| s.service.clone())?
                .ok_or(NativeError::Message(
                    "AsyncTrigger requires an engine event pump",
                ))?;
            let source = events.borrow_mut().insert(cx.this(), Kind::Trigger, 1)?;
            Ok(Self {
                lease: Some(Lease { source, events }),
                action,
                ..Self::default()
            })
        }
        #[tjs::method]
        fn finalize(&self) {}
        #[tjs::method]
        fn trigger(&self) -> NativeResult<()> {
            let lease = self.lease()?;
            let mut events = lease.events.borrow_mut();
            if self.cached {
                events.cancel(lease.source);
            }
            events.post(lease.source, 1, self.mode)
        }
        #[tjs::method]
        fn cancel(&self) -> NativeResult<()> {
            let lease = self.lease()?;
            lease.events.borrow_mut().cancel(lease.source);
            Ok(())
        }
        #[tjs::method(name = "onFire", resumable = true)]
        fn fire(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
            self.lease()?;
            self.action.invoke(cx)
        }
        #[tjs::getter]
        fn cached(&self) -> bool {
            self.cached
        }
        #[tjs::setter(name = "cached")]
        fn set_cached(&mut self, value: bool) -> NativeResult<()> {
            if self.cached != value {
                self.cancel()?;
                self.cached = value;
            }
            Ok(())
        }
        #[tjs::getter]
        fn mode(&self) -> i64 {
            i64::from(self.mode)
        }
        #[tjs::setter(name = "mode")]
        fn set_mode(&mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<()> {
            let value = tjs_core::value::to_integer(cx.heap(), value)? as i32;
            if self.mode != value {
                self.cancel()?;
                self.mode = value;
            }
            Ok(())
        }
    }
}
pub(crate) fn install(heap: &mut Heap, events: events::Shared) -> NativeResult<()> {
    let class = implementation::install(heap)?;
    heap.initialize_class_state::<implementation::State>(class)?;
    heap.with_native_state::<implementation::State, _>(class, |s| s.service = Some(events))?;
    Ok(())
}

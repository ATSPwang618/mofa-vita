//! Client-to-screen conversion through the actual window host. No script
//! coordinate or zoom properties are read here; those belong to the caller.
use super::{
    Shared,
    bindings::State,
    tasks::{self, Change},
};
use krkr_protocol::window::{Command, WindowId};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value,
};

/// A managed window identity captured before a caller runs further script.
/// Keeping the owner rooted does not prevent explicit window invalidation.
pub struct Target {
    shared: Shared,
    id: WindowId,
    owner: ObjId,
}
impl Trace for Target {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
pub fn target(cx: &mut NativeCx<'_>, window: Value) -> NativeResult<Target> {
    let Value::Obj(reference) = window else {
        return Err(NativeError::Type("a Window object"));
    };
    let owner = reference
        .object
        .ok_or(NativeError::Type("a non-null Window object"))?;
    cx.heap_mut()
        .with_native_state::<State, _>(owner, |state| {
            let lease = state.lease()?;
            Ok(Target {
                shared: lease.shared.clone(),
                id: lease.id,
                owner,
            })
        })?
}
impl Target {
    /// Query after all script computations so a getter moving the window is
    /// reflected in the same call. A missing global origin is an unavailable
    /// capability (e.g. Wayland), never a fabricated zero or outer-frame origin.
    pub fn coordinate(
        self,
        cx: &mut NativeCx<'_>,
        position: i32,
        vertical: bool,
    ) -> NativeResult<NativeStep> {
        cx.heap().ensure_valid(self.owner)?;
        self.shared.borrow_mut().record_mut(self.id)?.snapshot = None;
        let shared = self.shared.clone();
        tasks::request_then(
            &shared,
            self.id,
            Command::QueryState,
            Change::None,
            None,
            Box::new(Readback {
                target: self,
                position,
                vertical,
            }),
        )
    }
}
struct Readback {
    target: Target,
    position: i32,
    vertical: bool,
}
impl Trace for Readback {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.target.trace(visit);
    }
}
impl NativeContinuation for Readback {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let world = self.target.shared.borrow();
        let client = world
            .record(self.target.id)?
            .snapshot
            .ok_or(NativeError::Message("host did not return window state"))?
            .client
            .ok_or(NativeError::Message(
                "client screen coordinates are unavailable on this backend",
            ))?;
        let origin = if self.vertical { client.y } else { client.x };
        Ok(NativeStep::Return(Value::Int(i64::from(
            origin.wrapping_add(self.position),
        ))))
    }
}

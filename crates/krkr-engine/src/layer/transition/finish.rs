use super::*;
use input::changes::Operation;
use tjs_core::NativeTryContinuation;

pub(super) fn stop(shared: Shared, id: Id) -> NativeResult<NativeStep> {
    let mut world = shared.borrow_mut();
    let Some(active) = world.remove_transition(id) else {
        return Ok(NativeStep::Return(Value::Void));
    };
    let a = world.record(active.destination)?;
    let b = world.record(active.source)?;
    let task = Stop {
        shared: shared.clone(),
        a: active.destination,
        b: active.source,
        positions: [
            (b.geometry.left, b.geometry.top, b.visible),
            (a.geometry.left, a.geometry.top, a.visible),
        ],
        phase: 0,
    };
    drop(world);
    Ok(change(
        shared,
        task.a,
        Operation::Exchange(task.b, !active.with_children),
        Box::new(task),
    ))
}
struct Stop {
    shared: Shared,
    a: LayerId,
    b: LayerId,
    positions: [(i32, i32, bool); 2],
    phase: u8,
}
impl Trace for Stop {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for id in [self.a, self.b] {
            if let Some(r) = self.shared.borrow().records.get(id) {
                r.owner.trace(visit);
            }
        }
    }
}
impl NativeContinuation for Stop {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        if self.phase == 0 {
            let mut world = shared.borrow_mut();
            for (id, (left, top, _)) in [self.a, self.b].into_iter().zip(self.positions) {
                if world.is_primary(id) && (left != 0 || top != 0) {
                    return Err(NativeError::Message("primary layer cannot be moved"));
                }
                let r = world.record_mut(id)?;
                r.geometry.left = left;
                r.geometry.top = top;
                let window = r.window;
                world.changed(window);
            }
        }
        if self.phase < 2 {
            let id = [self.a, self.b][self.phase as usize];
            let visible = self.positions[self.phase as usize].2;
            self.phase += 1;
            return Ok(change(shared, id, Operation::Visible(visible), self));
        }
        let world = shared.borrow();
        let a = world.record(self.a)?;
        let b = world.record(self.b)?;
        if a.shutdown || b.shutdown {
            return Ok(NativeStep::Return(Value::Void));
        }
        Ok(NativeStep::CallMember {
            object: object(a.owner),
            key: world.names["onTransitionCompleted"],
            arguments: vec![object(a.owner), object(b.owner)],
            continuation: Box::new(input::Returned),
        })
    }
}
struct Change {
    shared: Shared,
    id: LayerId,
    operation: Operation,
}
impl Trace for Change {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        if let Some(r) = world.records.get(self.id) {
            r.owner.trace(visit);
        }
        if let Operation::Exchange(id, _) = self.operation
            && let Some(r) = world.records.get(id)
        {
            r.owner.trace(visit);
        }
    }
}
impl NativeContinuation for Change {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        input::changes::start(self.shared, self.id, self.operation)
    }
}
struct Then(Box<dyn NativeContinuation>);
impl Trace for Then {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.0.trace(visit);
    }
}
impl NativeTryContinuation for Then {
    fn resume(
        self: Box<Self>,
        _: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        Ok(match result {
            Ok(_) => NativeStep::Continue(self.0),
            Err(e) => NativeStep::Throw(e),
        })
    }
}
fn change(
    shared: Shared,
    id: LayerId,
    operation: Operation,
    next: Box<dyn NativeContinuation>,
) -> NativeStep {
    NativeStep::Try {
        task: Box::new(Change {
            shared,
            id,
            operation,
        }),
        continuation: Box::new(Then(next)),
    }
}
pub(in crate::layer) fn cleanup(
    shared: Shared,
    id: LayerId,
    next: Box<dyn NativeContinuation>,
) -> NativeStep {
    NativeStep::Continue(Box::new(Cleanup { shared, id, next }))
}
struct Cleanup {
    shared: Shared,
    id: LayerId,
    next: Box<dyn NativeContinuation>,
}
impl Trace for Cleanup {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl NativeContinuation for Cleanup {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let id =
            self.shared.borrow().transitions.iter().find_map(|(id, a)| {
                (a.destination == self.id || a.source == self.id).then_some(id)
            });
        if let Some(id) = id {
            Ok(NativeStep::Try {
                task: Box::new(StopOne {
                    shared: self.shared.clone(),
                    id,
                }),
                continuation: Box::new(Then(self)),
            })
        } else {
            self.next.resume(cx, Value::Void)
        }
    }
}
struct StopOne {
    shared: Shared,
    id: Id,
}
impl Trace for StopOne {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.shared.borrow().trace_transitions(visit);
    }
}
impl NativeContinuation for StopOne {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        stop(self.shared, self.id)
    }
}

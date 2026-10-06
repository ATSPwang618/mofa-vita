//! Outermost enabled-state notifications and intrinsic cleanup completion.
use super::*;
use tjs_core::NativeTryContinuation;

pub(super) struct Shutdown {
    pub(super) shared: Shared,
    pub(super) id: LayerId,
    pub(super) committed: bool,
}
impl Drop for Shutdown {
    fn drop(&mut self) {
        if !self.committed
            && let Some(r) = self.shared.borrow_mut().records.get_mut(self.id)
        {
            r.shutdown = false;
        }
    }
}
pub(super) struct Finish {
    pub(super) shared: Shared,
    pub(super) id: LayerId,
    pub(super) window: WindowId,
    pub(super) batch: Option<Rc<()>>,
    pub(super) shutdown: Option<Shutdown>,
}
impl Trace for Finish {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        trace_layer(&self.shared.borrow(), Some(self.id), visit);
    }
}
impl NativeTryContinuation for Finish {
    fn resume(
        self: Box<Self>,
        _: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        let notify = self
            .batch
            .is_some_and(|batch| Rc::strong_count(&batch) == 1);
        let stack = if notify {
            self.shared
                .borrow()
                .primary
                .get(&self.window)
                .copied()
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        Ok(NativeStep::Continue(Box::new(Notify {
            shared: self.shared,
            id: self.id,
            window: self.window,
            stack,
            children_of: None,
            result,
            shutdown: self.shutdown,
            font_pending: false,
        })))
    }
}
struct Notify {
    shared: Shared,
    id: LayerId,
    window: WindowId,
    stack: Vec<LayerId>,
    children_of: Option<LayerId>,
    result: Result<Value, Value>,
    shutdown: Option<Shutdown>,
    font_pending: bool,
}
impl Trace for Notify {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        (*self.result.as_ref().unwrap_or_else(|v| v)).trace(visit);
        trace_layer(&self.shared.borrow(), Some(self.id), visit);
        // Tree membership is weak. Only the currently dispatched node is retained.
        trace_layer(&self.shared.borrow(), self.children_of, visit);
    }
}
impl NativeContinuation for Notify {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        if !shared.borrow().windows.borrow().contains(self.window) {
            self.stack.clear();
            self.children_of = None;
        }
        // Unchanged nodes have no script work to resume. Scan a bounded batch
        // in place instead of passing every sibling through the VM scheduler.
        // Descend only after a callback returns: it may change the child list,
        // close the window or start another enabled-state notification walk.
        for _ in 0..64 {
            let world = shared.borrow();
            if let Some(id) = self.children_of.take()
                && let Some(r) = world.records.get(id)
            {
                self.stack.extend(r.children.iter().rev().copied());
            }
            let Some(id) = self.stack.pop() else {
                break;
            };
            let enabled = world.node_enabled(id);
            let changed = world
                .records
                .get(id)
                .is_some_and(|r| r.enabled_work != enabled);
            self.children_of = Some(id);
            drop(world);
            if changed {
                return Ok(send(
                    &shared,
                    Some(id),
                    if enabled {
                        "onNodeEnabled"
                    } else {
                        "onNodeDisabled"
                    },
                    vec![],
                    self,
                ));
            }
        }
        if self.children_of.is_some() || !self.stack.is_empty() {
            return Ok(NativeStep::Continue(self));
        }
        if self.result.is_ok() && self.shutdown.is_some() && !self.font_pending {
            self.font_pending = true;
            let font = shared
                .borrow()
                .records
                .get(self.id)
                .and_then(|r| r.font_object);
            if let Some(font) = font {
                return Ok(NativeStep::Invalidate {
                    object: object(font),
                    continuation: self,
                });
            }
        }
        if self.result.is_ok()
            && let Some(shutdown) = self.shutdown.as_mut()
        {
            shutdown.committed = true;
        }
        Ok(match self.result {
            Ok(value) => NativeStep::Return(value),
            Err(value) => NativeStep::Throw(value),
        })
    }
}

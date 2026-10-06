//! Explicit Window.update completes pending script paint in the calling VM.
use super::*;
use tjs_core::{NativeContinuation, NativeCx, NativeStep};

pub(crate) fn window(shared: Shared, window: WindowId, owner: ObjId) -> NativeStep {
    let mut world = shared.borrow_mut();
    // The reference suppresses recursive delivery from onPaint. Re-entering
    // here would also expose an incomplete parent/child paint pass.
    if !world.paint_active.insert(window) {
        return NativeStep::Return(Value::Void);
    }
    // Most animation ticks only change geometry or pixels. With no script
    // paint pending, avoid collecting, rooting and marking the entire tree.
    // Queued passes retain their original ordering and cleanup path.
    if !world.records.values().any(|r| {
        r.window == window && (r.paint_queued || (r.ready && !r.shutdown && r.call_on_paint))
    }) {
        world.paint_active.remove(&window);
        let live = world.windows.borrow().is_live(window);
        if live {
            world.paint_finished.insert(window);
            world.changed(window);
        }
        world.publish_window(window);
        return NativeStep::Return(Value::Void);
    }
    let roots = world.paint_roots(window);
    let mut seen = HashSet::new();
    let ids: Vec<_> = roots
        .into_iter()
        .flat_map(|root| world.nodes(Some(root)))
        .filter(|id| seen.insert(*id))
        .filter(|id| {
            world
                .records
                .get(*id)
                .is_some_and(|r| r.ready && !r.shutdown)
        })
        .collect();
    for &id in &ids {
        world.records[id].paint_queued = true;
    }
    drop(world);
    NativeStep::Continue(Box::new(Update {
        shared,
        window,
        owner,
        ids,
        next: 0,
    }))
}

struct Update {
    shared: Shared,
    window: WindowId,
    owner: ObjId,
    ids: Vec<LayerId>,
    next: usize,
}
impl Trace for Update {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        let world = self.shared.borrow();
        for id in &self.ids {
            if let Some(r) = world.records.get(*id) {
                r.owner.trace(visit);
            }
        }
        world.trace_transitions(visit);
    }
}
impl Drop for Update {
    fn drop(&mut self) {
        let mut world = self.shared.borrow_mut();
        world.paint_active.remove(&self.window);
        for id in &self.ids {
            if let Some(r) = world.records.get_mut(*id) {
                r.paint_queued = false;
            }
        }
        let live = world.windows.borrow().is_live(self.window);
        if live {
            world.paint_finished.insert(self.window);
            world.changed(self.window);
        }
    }
}
impl NativeContinuation for Update {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        while let Some(&id) = self.ids.get(self.next) {
            self.next += 1;
            let call = {
                let mut world = self.shared.borrow_mut();
                let key = world.names["onPaint"];
                world
                    .records
                    .get_mut(id)
                    .filter(|r| r.ready && !r.shutdown && r.call_on_paint)
                    .map(|r| {
                        r.call_on_paint = false;
                        (object(r.owner), key)
                    })
            };
            if let Some((object, key)) = call {
                return Ok(NativeStep::CallMember {
                    object,
                    key,
                    arguments: vec![],
                    continuation: self,
                });
            }
        }
        let shared = self.shared.clone();
        let window = self.window;
        drop(self);
        // Only submit this window: another script's partially prepared scene
        // must keep its usual scheduler boundary. No framebuffer readback.
        shared.borrow_mut().publish_window(window);
        Ok(NativeStep::Return(Value::Void))
    }
}

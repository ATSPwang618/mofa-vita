//! VM-owned lifecycle and clock phases; textures and kernels stay in the host.
use super::*;
use krkr_protocol::transition::{Effect, Frame};
use std::time::Duration;
use tjs_core::{NativeContinuation, NativeCx, NativeStep};
pub(super) mod begin;
mod finish;
mod paint;
pub(crate) mod provider;
pub(in crate::layer) use finish::cleanup;
pub(crate) use paint::callback as paint_callback;
slotmap::new_key_type! { pub(crate) struct Id; }

pub(super) struct Active {
    destination: LayerId,
    source: LayerId,
    callback: Value,
    with_children: bool,
    frame: Frame,
    duration: u64,
    start: Option<u64>,
    next: Duration,
    self_update: bool,
    refresh: bool,
    queued: bool,
    complete: bool,
    rule: Option<ImageRef>,
    custom: Option<krkr_protocol::transition::custom::Frame>,
}
impl Active {
    fn advance(&mut self, tick: u64) -> bool {
        let start = *self.start.get_or_insert(tick);
        let elapsed = tick.wrapping_sub(start).min(self.duration);
        let custom_changed = self.custom.as_mut().is_some_and(|custom| {
            let changed = custom.elapsed != elapsed;
            custom.elapsed = elapsed;
            changed
        });
        let frame = self.frame.effect.frame(
            self.frame.face,
            self.frame.size,
            tick.wrapping_sub(start),
            self.duration,
        );
        let changed = custom_changed || self.frame.phase != frame.phase;
        self.frame = frame;
        self.complete = frame.phase == frame.effect.phases(frame.size);
        changed
    }
}
impl Layers {
    pub(super) fn has_transition(&self, window: WindowId) -> bool {
        self.transitions.values().any(|a| {
            self.records
                .get(a.destination)
                .is_some_and(|r| r.window == window)
        })
    }

    pub(super) fn paint_roots(&self, window: WindowId) -> Vec<LayerId> {
        let mut roots: Vec<_> = self.primary.get(&window).copied().into_iter().collect();
        roots.extend(self.transitions.values().filter_map(|a| {
            self.records
                .get(a.destination)
                .filter(|r| r.window == window)
                .map(|_| a.source)
        }));
        roots
    }
    pub(super) fn transition_source(&self, id: LayerId) -> Option<LayerId> {
        self.records
            .get(id)?
            .transition
            .and_then(|id| self.transitions.get(id))
            .map(|a| a.source)
    }
    pub(crate) fn cancel_transition_tick(&mut self, id: Id) {
        if let Some(a) = self.transitions.get_mut(id) {
            a.queued = false;
            a.refresh = true;
        }
    }
    pub(crate) fn cancel_paint(&mut self, id: LayerId) {
        if let Some(r) = self.records.get_mut(id) {
            r.paint_queued = false;
            let window = r.window;
            self.changed(window);
        }
    }
    pub(crate) fn forget_window(&mut self, window: WindowId) {
        self.forget_input(window);
        let ids: Vec<_> = self
            .transitions
            .iter()
            .filter_map(|(id, a)| (self.records[a.destination].window == window).then_some(id))
            .collect();
        for id in ids {
            self.remove_transition(id);
        }
        for r in self.records.values_mut().filter(|r| r.window == window) {
            r.paint_queued = false;
            r.call_on_paint = false;
        }
        self.dirty.remove(&window);
        self.paint_finished.remove(&window);
    }
    pub(super) fn scene_transitions(
        &self,
        indices: &HashMap<LayerId, usize>,
    ) -> Vec<krkr_protocol::transition::SceneTransition> {
        let mut result = Vec::with_capacity(
            self.transitions
                .values()
                .filter(|a| indices.contains_key(&a.destination) && indices.contains_key(&a.source))
                .count(),
        );
        result.extend(self.transitions.values().filter_map(|a| {
            Some(krkr_protocol::transition::SceneTransition {
                destination: *indices.get(&a.destination)?,
                source: *indices.get(&a.source)?,
                with_children: a.with_children,
                frame: a.frame,
                rule: a.rule.clone(),
                custom: a.custom.clone(),
            })
        }));
        result
    }
    pub(super) fn trace_transitions(&self, visit: &mut dyn FnMut(Value)) {
        for active in self.transitions.values() {
            for id in [active.destination, active.source] {
                if let Some(r) = self.records.get(id) {
                    r.owner.trace(visit);
                }
            }
            active.callback.trace(visit);
        }
    }
    pub(super) fn changed(&mut self, window: WindowId) {
        self.dirty.insert(window);
        for active in self.transitions.values_mut().filter(|a| a.self_update) {
            if self
                .records
                .get(active.destination)
                .is_some_and(|r| r.window == window)
            {
                active.refresh = true;
            }
        }
    }
    fn transition_visible(&self, id: LayerId) -> bool {
        let mut at = Some(id);
        while let Some(id) = at {
            let Some(r) = self.records.get(id) else {
                return false;
            };
            if !r.visible || r.opacity == 0 || r.shutdown {
                return false;
            }
            at = r.parent;
        }
        true
    }
    pub(crate) fn advance_transitions(&mut self) {
        if self.dirty.is_empty() && self.transitions.is_empty() {
            return;
        }
        let now = self.windows.borrow().now();
        let mut drawing = self.dirty.clone();
        for a in self.transitions.values() {
            if !a.queued
                && !a.complete
                && if a.self_update {
                    a.refresh
                } else {
                    a.next <= now
                }
            {
                drawing.insert(self.records[a.destination].window);
            }
        }
        let painting = self.advance_paint(&drawing);
        // No work scales with pixel count here. Ordinary clock phases can
        // progress while script events are disabled; completion remains queued.
        let ids: Vec<_> = self.transitions.keys().collect();
        for id in ids {
            let a = &self.transitions[id];
            let visible = self.transition_visible(a.destination);
            let window = self.records[a.destination].window;
            let paint_pending = painting.contains(&window);
            if a.queued
                || paint_pending
                || (a.self_update && (!a.refresh || !visible) && !a.complete)
                || (!a.self_update && a.next > now && !a.complete)
            {
                continue;
            }
            let a = &mut self.transitions[id];
            a.next = now.saturating_add(Duration::from_millis(16));
            a.refresh = false;
            if !a.complete && matches!(a.callback, Value::Void) {
                if a.advance(now.as_millis() as u64) {
                    self.dirty.insert(window);
                }
                if !visible && !a.self_update {
                    a.complete = true;
                }
            }
            if a.complete || !matches!(a.callback, Value::Void) {
                a.queued = self
                    .windows
                    .borrow_mut()
                    .post_transition(window, id)
                    .is_ok();
                if !a.queued {
                    a.refresh = true;
                }
            }
        }
    }
    pub(crate) fn transition_sleep(&self, events: bool) -> Option<Duration> {
        let now = self.windows.borrow().now();
        self.transitions
            .values()
            .filter(|a| {
                !a.queued
                    && !a.self_update
                    && !a.complete
                    && (events || matches!(a.callback, Value::Void))
                    && !self
                        .records
                        .values()
                        .any(|r| r.paint_queued && r.window == self.records[a.destination].window)
            })
            .map(|a| a.next.saturating_sub(now))
            .min()
    }
    fn remove_transition(&mut self, id: Id) -> Option<Active> {
        let active = self.transitions.remove(id)?;
        if let Some(r) = self.records.get_mut(active.destination) {
            if r.transition == Some(id) {
                r.transition = None;
            }
            self.dirty.insert(r.window);
        }
        if let Some(image) = &active.rule {
            self.images.remove(image.id);
        }
        Some(active)
    }
}
pub(crate) fn callback(shared: Shared, id: Id) -> Box<dyn NativeContinuation> {
    Box::new(Tick {
        shared,
        id,
        called: false,
    })
}
struct Tick {
    shared: Shared,
    id: Id,
    called: bool,
}
impl Trace for Tick {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.shared.borrow().trace_transitions(visit);
    }
}
impl Drop for Tick {
    fn drop(&mut self) {
        if let Some(a) = self.shared.borrow_mut().transitions.get_mut(self.id) {
            a.queued = false;
        }
    }
}
impl NativeContinuation for Tick {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        let mut world = shared.borrow_mut();
        let Some(a) = world.transitions.get(self.id) else {
            return Ok(NativeStep::Return(Value::Void));
        };
        let destination = a.destination;
        if !a.complete && !matches!(a.callback, Value::Void) && !self.called {
            let function = a.callback;
            drop(world);
            self.called = true;
            return Ok(NativeStep::Call {
                function,
                arguments: vec![],
                continuation: self,
            });
        }
        if self.called {
            let tick = tjs_core::value::to_integer(cx.heap(), value)? as u64;
            let visible = world.transition_visible(destination);
            let window = world.record(destination)?.window;
            let a = &mut world.transitions[self.id];
            let changed = a.advance(tick);
            if !visible && !a.self_update {
                a.complete = true;
            }
            if changed {
                world.dirty.insert(window);
            }
        }
        let complete = world.transitions[self.id].complete;
        drop(world);
        if complete {
            stop(shared, self.id)
        } else {
            Ok(NativeStep::Return(Value::Void))
        }
    }
}
pub(super) fn stop(shared: Shared, id: Id) -> NativeResult<NativeStep> {
    finish::stop(shared, id)
}
pub(super) fn stop_layer(shared: Shared, id: LayerId) -> NativeResult<NativeStep> {
    let active = shared.borrow().record(id)?.transition;
    match active {
        Some(active) => stop(shared, active),
        None => Ok(NativeStep::Return(Value::Void)),
    }
}

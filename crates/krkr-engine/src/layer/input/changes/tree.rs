//! Blur and modal removal use the same focus state machine as explicit focus.
use super::*;

pub(super) fn blur(
    shared: Shared,
    window: WindowId,
    root: LayerId,
    completion: Box<dyn NativeContinuation>,
) -> NativeStep {
    let task = Box::new(Blur {
        shared: shared.clone(),
        window,
        root,
        phase: 0,
        completion,
    });
    remove_modes(shared, window, root, true, task)
}
struct Blur {
    shared: Shared,
    window: WindowId,
    root: LayerId,
    phase: u8,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Blur {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.completion.trace(visit);
        trace_layer(&self.shared.borrow(), Some(self.root), visit);
    }
}
impl NativeContinuation for Blur {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        match self.phase {
            0 => {
                self.phase = 1;
                let mut world = shared.borrow_mut();
                let old = world
                    .input
                    .get(&self.window)
                    .and_then(|s| s.hover)
                    .filter(|&id| belongs(&world.records, self.root, id));
                if old.is_some() {
                    world.input.get_mut(&self.window).unwrap().hover = None;
                }
                drop(world);
                Ok(send(&shared, old, "onMouseLeave", vec![], self))
            }
            1 => {
                let world = shared.borrow();
                let affected = world
                    .focused(self.window)
                    .is_some_and(|id| belongs(&world.records, self.root, id));
                drop(world);
                if !affected {
                    return self.completion.resume(cx, Value::Void);
                }
                self.phase = 2;
                Ok(focus::search(shared, self.root, true, self))
            }
            2 => {
                let candidate = focus::optional_layer(cx, value)?;
                let candidate =
                    candidate.filter(|&id| shared.borrow().focused(self.window) != Some(id));
                self.phase = 3;
                Ok(focus::set(shared, self.window, candidate, true, self))
            }
            _ => self.completion.resume(cx, Value::Void),
        }
    }
}

pub(super) fn remove_modes(
    shared: Shared,
    window: WindowId,
    root: LayerId,
    tree: bool,
    completion: Box<dyn NativeContinuation>,
) -> NativeStep {
    NativeStep::Continue(Box::new(RemoveModes {
        shared,
        window,
        root,
        tree,
        removing: None,
        phase: 0,
        completion,
    }))
}
struct RemoveModes {
    shared: Shared,
    window: WindowId,
    root: LayerId,
    tree: bool,
    removing: Option<LayerId>,
    phase: u8,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for RemoveModes {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.completion.trace(visit);
        let world = self.shared.borrow();
        trace_layer(&world, Some(self.root), visit);
        trace_layer(&world, self.removing, visit);
    }
}
impl NativeContinuation for RemoveModes {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        match self.phase {
            0 => {
                let world = shared.borrow();
                self.removing = world.input.get(&self.window).and_then(|s| {
                    s.modal.iter().copied().find(|&id| {
                        if self.tree {
                            belongs(&world.records, self.root, id)
                        } else {
                            id == self.root
                        }
                    })
                });
                drop(world);
                if self.removing.is_none() {
                    return self.completion.resume(cx, Value::Void);
                }
                self.phase = 1;
                Ok(focus::search(shared, self.root, true, self))
            }
            1 => {
                self.phase = 2;
                let target = focus::optional_layer(cx, value)?;
                Ok(focus::set(shared, self.window, target, true, self))
            }
            _ => {
                if let Some(s) = shared.borrow_mut().input.get_mut(&self.window)
                    && let Some(index) = s.modal.iter().position(|&id| Some(id) == self.removing)
                {
                    s.modal.remove(index);
                }
                self.phase = 0;
                Ok(NativeStep::Continue(self))
            }
        }
    }
}
impl Layers {
    pub(super) fn release_capture_tree(&mut self, root: LayerId) {
        for state in self.input.values_mut() {
            if state
                .capture
                .is_some_and(|id| belongs(&self.records, root, id))
            {
                state.capture = None;
                state.release = state.release.wrapping_add(1);
            }
        }
    }
}

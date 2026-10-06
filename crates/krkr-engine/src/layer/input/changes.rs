//! Tree/input changes preserve the reference callback order. Enabled-state
//! notifications run at the outermost completed change, including exceptions.
use super::*;
mod exchange;
mod notify;
mod tree;
use notify::{Finish, Shutdown};
use tree::{blur, remove_modes};

#[derive(Clone, Copy)]
pub(in crate::layer) enum Operation {
    Visible(bool),
    Enabled(bool),
    Focusable(bool),
    Parent(Option<LayerId>),
    Exchange(LayerId, bool),
    SetMode,
    RemoveMode,
    Invalidate,
}
pub(in crate::layer) fn start(
    shared: Shared,
    id: LayerId,
    operation: Operation,
) -> NativeResult<NativeStep> {
    let mut world = shared.borrow_mut();
    let r = world.record(id)?;
    let window = r.window;
    let unchanged = match operation {
        Operation::Visible(value) => r.visible == value,
        Operation::Enabled(value) => r.enabled == value,
        Operation::Focusable(value) => r.focusable == value,
        Operation::Parent(parent) => r.parent == parent,
        Operation::Exchange(other, _) => id == other,
        Operation::RemoveMode => !world
            .input
            .get(&window)
            .is_some_and(|s| s.modal.contains(&id)),
        _ => false,
    };
    if unchanged {
        return Ok(NativeStep::Return(Value::Void));
    }
    // Common construction/property updates need no owned callback task.
    if let Operation::Focusable(value) = operation
        && (value || world.focused(window) != Some(id) || !world.node_focusable(id))
    {
        world.record_mut(id)?.focusable = value;
        return Ok(NativeStep::Return(Value::Void));
    }
    if matches!(operation, Operation::Visible(true)) {
        world.record_mut(id)?.visible = true;
        world.changed(window);
        return Ok(NativeStep::Return(Value::Void));
    }
    if matches!(operation, Operation::Visible(false)) && world.is_primary(id) {
        return Err(NativeError::Message("primary layer cannot be hidden"));
    }
    if let Operation::Parent(parent) = operation {
        world.validate_parent(id, parent)?;
    }
    if let Operation::Exchange(other, _) = operation
        && world.record(other)?.window != window
    {
        return Err(NativeError::Message(
            "layers belong to different tree owners",
        ));
    }
    let needs_batch = matches!(
        operation,
        Operation::Enabled(_)
            | Operation::SetMode
            | Operation::RemoveMode
            | Operation::Exchange(..)
    ) || world.input.get(&window).is_some_and(|s| {
        s.modal
            .iter()
            .any(|&modal| belongs(&world.records, id, modal))
    });
    let batch = if needs_batch {
        Some(
            if let Some(batch) = world
                .input
                .get(&window)
                .and_then(|s| s.enabled_batch.upgrade())
            {
                batch
            } else {
                // Keep the reference's per-node work bit. Nested notifications
                // may start another snapshot; the outer walk sees that update.
                for id in world.nodes(world.primary.get(&window).copied()) {
                    let enabled = world.node_enabled(id);
                    world.records[id].enabled_work = enabled;
                }
                let batch = Rc::new(());
                world.input.entry(window).or_default().enabled_batch = Rc::downgrade(&batch);
                batch
            },
        )
    } else {
        None
    };
    let shutdown = if matches!(operation, Operation::Invalidate) {
        world.record_mut(id)?.shutdown = true;
        Some(Shutdown {
            shared: shared.clone(),
            id,
            committed: false,
        })
    } else {
        None
    };
    drop(world);
    Ok(NativeStep::Try {
        task: Box::new(Work {
            shared: shared.clone(),
            id,
            window,
            operation,
            phase: 0,
        }),
        continuation: Box::new(Finish {
            shared,
            id,
            window,
            batch,
            shutdown,
        }),
    })
}
struct Work {
    shared: Shared,
    id: LayerId,
    window: WindowId,
    operation: Operation,
    phase: u8,
}
impl Trace for Work {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        trace_layer(&world, Some(self.id), visit);
        if let Operation::Parent(id) = self.operation {
            trace_layer(&world, id, visit);
        }
        if let Operation::Exchange(id, _) = self.operation {
            trace_layer(&world, Some(id), visit);
        }
    }
}
impl NativeContinuation for Work {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        if !shared.borrow().records.contains_key(self.id)
            || !shared.borrow().windows.borrow().contains(self.window)
        {
            return Ok(NativeStep::Return(Value::Void));
        }
        match self.phase {
            0 => {
                self.phase = 1;
                match self.operation {
                    Operation::Exchange(other, keep_children) => {
                        return exchange::start(shared, self.id, other, keep_children);
                    }
                    Operation::Visible(visible) => {
                        let mut world = shared.borrow_mut();
                        world.record_mut(self.id)?.visible = visible;
                        world.changed(self.window);
                        if visible {
                            return Ok(NativeStep::Return(Value::Void));
                        }
                    }
                    Operation::Enabled(enabled) => {
                        shared.borrow_mut().record_mut(self.id)?.enabled = enabled;
                        if enabled {
                            return Ok(NativeStep::Return(Value::Void));
                        }
                    }
                    Operation::Focusable(focusable) => {
                        shared.borrow_mut().record_mut(self.id)?.focusable = focusable;
                        let world = shared.borrow();
                        let change = world.focused(self.window) == Some(self.id)
                            && !world.node_focusable(self.id);
                        drop(world);
                        if !change {
                            return Ok(NativeStep::Return(Value::Void));
                        }
                        self.phase = 2;
                        return Ok(focus::search(shared, self.id, true, self));
                    }
                    Operation::Parent(_) => {
                        if shared.borrow().record(self.id)?.parent.is_none() {
                            return self.resume(cx, value);
                        }
                    }
                    Operation::Invalidate => {
                        self.phase = 5;
                        return Ok(crate::layer::transition::cleanup(shared, self.id, self));
                    }
                    Operation::SetMode => {
                        let mut world = shared.borrow_mut();
                        let current = world
                            .input
                            .get(&self.window)
                            .and_then(|s| s.modal.last())
                            .copied();
                        if current.is_some_and(|root| belongs(&world.records, root, self.id)) {
                            return Err(NativeError::Message(
                                "cannot set mode to a disabled or current modal layer",
                            ));
                        }
                        world.record_mut(self.id)?.visible = true;
                        world.changed(self.window);
                        let mut parent = world.record(self.id)?.parent;
                        while let Some(id) = parent {
                            let r = world.record(id)?;
                            if !r.visible {
                                return Err(NativeError::Message(
                                    "modal layer has a hidden ancestor",
                                ));
                            }
                            parent = r.parent;
                        }
                        if !world.record(self.id)?.enabled {
                            return Err(NativeError::Message("modal layer is disabled"));
                        }
                        let target = world.first_focusable(Some(self.id), true);
                        drop(world);
                        self.phase = 4;
                        return Ok(focus::set(shared, self.window, target, true, self));
                    }
                    Operation::RemoveMode => {
                        return Ok(remove_modes(shared, self.window, self.id, false, self));
                    }
                }
                Ok(blur(shared, self.window, self.id, self))
            }
            1 => {
                match self.operation {
                    Operation::Parent(parent) => {
                        let mut world = shared.borrow_mut();
                        world.validate_parent(self.id, parent)?;
                        world.release_capture_tree(self.id);
                        world.attach(self.id, parent)?;
                    }
                    Operation::Invalidate => {
                        let mut world = shared.borrow_mut();
                        world.release_capture_tree(self.id);
                        world.attach(self.id, None)?;
                    }
                    _ => {}
                }
                Ok(NativeStep::Return(Value::Void))
            }
            2 => {
                self.phase = 3;
                let target = focus::optional_layer(cx, value)?;
                Ok(focus::set(shared, self.window, target, true, self))
            }
            5 => {
                self.phase = 1;
                Ok(blur(shared, self.window, self.id, self))
            }
            4 => {
                shared
                    .borrow_mut()
                    .input
                    .entry(self.window)
                    .or_default()
                    .modal
                    .push(self.id);
                Ok(NativeStep::Return(Value::Void))
            }
            _ => Ok(NativeStep::Return(Value::Void)),
        }
    }
}

impl bindings::State {
    pub(in crate::layer) fn change_input(&self, operation: Operation) -> NativeResult<NativeStep> {
        let Some(lease) = &self.lease else {
            return if matches!(operation, Operation::Invalidate) {
                Ok(NativeStep::Return(Value::Void))
            } else {
                Err(NativeError::This)
            };
        };
        start(lease.shared.clone(), lease.id, operation)
    }
}

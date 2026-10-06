//! Focus selection and callbacks are separate: before-focus may redirect or
//! reenter; blur/focus hold an owned lock which also releases on cancellation.
use super::*;

impl Layers {
    pub(in crate::layer) fn focused(&self, window: WindowId) -> Option<LayerId> {
        self.input.get(&window).and_then(|s| s.focus).filter(|&id| {
            self.input_primary(window)
                .is_some_and(|root| belongs(&self.records, root, id))
        })
    }
    pub(in crate::layer) fn disabled_by_mode(&self, id: LayerId) -> bool {
        self.records
            .get(id)
            .and_then(|r| self.input.get(&r.window))
            .and_then(|s| s.modal.last())
            .is_some_and(|&root| !belongs(&self.records, root, id))
    }
    pub(in crate::layer) fn node_focusable(&self, id: LayerId) -> bool {
        if !self
            .records
            .get(id)
            .is_some_and(|r| r.focusable && !r.shutdown)
            || self.disabled_by_mode(id)
        {
            return false;
        }
        let mut node = Some(id);
        while let Some(id) = node {
            let Some(r) = self.records.get(id) else {
                return false;
            };
            if !r.visible || !r.enabled {
                return false;
            }
            node = r.parent;
        }
        true
    }
    pub(in crate::layer) fn nodes(&self, root: Option<LayerId>) -> Vec<LayerId> {
        let mut result = Vec::new();
        let mut stack: Vec<_> = root.into_iter().collect();
        while let Some(id) = stack.pop() {
            if let Some(r) = self.records.get(id) {
                result.push(id);
                stack.extend(r.children.iter().rev().copied());
            }
        }
        result
    }
    pub(in crate::layer) fn first_focusable(
        &self,
        root: Option<LayerId>,
        ignore_chain: bool,
    ) -> Option<LayerId> {
        let mut stack: smallvec::SmallVec<[_; 16]> = root.into_iter().collect();
        while let Some(id) = stack.pop() {
            if let Some(record) = self.records.get(id) {
                if (ignore_chain || record.join_focus_chain) && self.node_focusable(id) {
                    return Some(id);
                }
                stack.extend(record.children.iter().rev().copied());
            }
        }
        None
    }
    fn neighbor(&self, id: LayerId, forward: bool) -> Option<LayerId> {
        let window = self.records.get(id)?.window;
        let nodes = self.nodes(self.input_primary(window));
        let index = nodes.iter().position(|&other| other == id)?;
        let len = nodes.len();
        if len < 2 {
            return None;
        }
        (1..=len)
            .map(|step| {
                nodes[if forward {
                    (index + step) % len
                } else {
                    (index + len - step) % len
                }]
            })
            .find(|&id| self.node_focusable(id) && self.records[id].join_focus_chain)
    }
}

pub(in crate::layer) fn search(
    shared: Shared,
    id: LayerId,
    forward: bool,
    completion: Box<dyn NativeContinuation>,
) -> NativeStep {
    let candidate = {
        let mut world = shared.borrow_mut();
        let candidate = world.neighbor(id, forward);
        if let Some(r) = world.records.get_mut(id) {
            r.focus_work = candidate;
        }
        candidate
    };
    let args = vec![owner(&shared.borrow(), candidate)];
    let name = if forward {
        "onSearchNextFocusable"
    } else {
        "onSearchPrevFocusable"
    };
    let task = Box::new(Search {
        shared: shared.clone(),
        id,
        candidate,
        completion,
    });
    send(&shared, Some(id), name, args, task)
}
struct Search {
    shared: Shared,
    id: LayerId,
    candidate: Option<LayerId>,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Search {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.completion.trace(visit);
        let world = self.shared.borrow();
        trace_layer(&world, Some(self.id), visit);
        trace_layer(&world, self.candidate, visit);
        trace_layer(
            &world,
            world.records.get(self.id).and_then(|r| r.focus_work),
            visit,
        );
    }
}
impl NativeContinuation for Search {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let result = {
            let world = self.shared.borrow();
            owner(
                &world,
                world.records.get(self.id).and_then(|r| r.focus_work),
            )
        };
        self.completion.resume(cx, result)
    }
}

pub(in crate::layer) fn set(
    shared: Shared,
    window: WindowId,
    target: Option<LayerId>,
    forward: bool,
    completion: Box<dyn NativeContinuation>,
) -> NativeStep {
    NativeStep::Continue(Box::new(Change {
        shared,
        window,
        target,
        previous: None,
        forward,
        phase: 0,
        lock: None,
        completion,
    }))
}
struct Change {
    shared: Shared,
    window: WindowId,
    target: Option<LayerId>,
    previous: Option<LayerId>,
    forward: bool,
    phase: u8,
    lock: Option<Rc<()>>,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Change {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.completion.trace(visit);
        let world = self.shared.borrow();
        for id in [self.target, self.previous, world.focused(self.window)] {
            trace_layer(&world, id, visit);
        }
        trace_layer(
            &world,
            self.target
                .and_then(|id| world.records.get(id))
                .and_then(|r| r.focus_work),
            visit,
        );
    }
}
impl NativeContinuation for Change {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let shared = self.shared.clone();
        if !shared.borrow().windows.borrow().contains(self.window) {
            return self.completion.resume(cx, Value::Int(0));
        }
        match self.phase {
            0 => {
                if self
                    .target
                    .is_some_and(|id| !shared.borrow().node_focusable(id))
                {
                    return self.completion.resume(cx, Value::Int(0));
                }
                self.phase = 1;
                let args = {
                    let mut world = shared.borrow_mut();
                    self.previous = world.focused(self.window);
                    if let Some(r) = self.target.and_then(|id| world.records.get_mut(id)) {
                        r.focus_work = self.target;
                    }
                    vec![
                        owner(&world, self.target),
                        owner(&world, self.previous),
                        Value::Int(self.forward.into()),
                    ]
                };
                Ok(send(&shared, self.target, "onBeforeFocus", args, self))
            }
            1 => {
                let mut world = shared.borrow_mut();
                self.target = self
                    .target
                    .and_then(|id| world.records.get(id))
                    .and_then(|r| r.focus_work);
                if self.target.is_some_and(|id| !world.node_focusable(id))
                    || world.focused(self.window) == self.target
                {
                    drop(world);
                    return self.completion.resume(cx, Value::Int(0));
                }
                let state = world.input.entry(self.window).or_default();
                if state.focus_lock.upgrade().is_some() {
                    return Err(NativeError::Message(
                        "cannot change focus while processing onBlur or onFocus",
                    ));
                }
                let lock = Rc::new(());
                state.focus_lock = Rc::downgrade(&lock);
                self.lock = Some(lock);
                self.previous = state.focus;
                state.focus = self.target;
                let args = vec![owner(&world, self.target)];
                drop(world);
                self.phase = 2;
                Ok(send(&shared, self.previous, "onBlur", args, self))
            }
            2 => {
                self.phase = 3;
                let world = shared.borrow();
                let current = world.focused(self.window);
                let args = vec![
                    owner(&world, self.previous),
                    Value::Int(self.forward.into()),
                ];
                drop(world);
                Ok(send(&shared, current, "onFocus", args, self))
            }
            _ => {
                self.lock = None;
                presentation::sync(&shared, self.window, cx, Value::Int(1), self.completion)
            }
        }
    }
}

pub(in crate::layer) fn navigate(
    shared: Shared,
    window: WindowId,
    forward: bool,
    completion: Box<dyn NativeContinuation>,
) -> NativeStep {
    let current = shared.borrow().focused(window);
    let task = Box::new(Navigate {
        shared: shared.clone(),
        window,
        forward,
        completion,
        candidate: Value::Void,
        selected: false,
    });
    if let Some(id) = current {
        search(shared, id, forward, task)
    } else {
        let world = shared.borrow();
        let id = world.first_focusable(world.input_primary(window), false);
        NativeStep::Continue(Box::new(First {
            value: owner(&world, id),
            task,
        }))
    }
}
struct First {
    value: Value,
    task: Box<dyn NativeContinuation>,
}
impl Trace for First {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.value.trace(visit);
        self.task.trace(visit);
    }
}
impl NativeContinuation for First {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.task.resume(cx, self.value)
    }
}
struct Navigate {
    shared: Shared,
    window: WindowId,
    forward: bool,
    completion: Box<dyn NativeContinuation>,
    candidate: Value,
    selected: bool,
}
impl Trace for Navigate {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.candidate.trace(visit);
        self.completion.trace(visit);
    }
}
impl NativeContinuation for Navigate {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if self.selected {
            return self.completion.resume(cx, self.candidate);
        }
        self.candidate = value;
        self.selected = true;
        let target = optional_layer(cx, value)?;
        if target.is_none() {
            return self.completion.resume(cx, value);
        }
        Ok(set(
            self.shared.clone(),
            self.window,
            target,
            self.forward,
            self,
        ))
    }
}
pub(in crate::layer) fn optional_layer(
    cx: &mut NativeCx<'_>,
    value: Value,
) -> NativeResult<Option<LayerId>> {
    if matches!(value, Value::Void | Value::Obj(ObjRef { object: None, .. })) {
        Ok(None)
    } else {
        bindings::layer_id(cx.heap_mut(), value).map(Some)
    }
}

pub(in crate::layer) struct Selection {
    pub shared: Shared,
    pub id: LayerId,
    pub value: Value,
}
impl Trace for Selection {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.value.trace(visit);
        trace_layer(&self.shared.borrow(), Some(self.id), visit);
    }
}
impl NativeContinuation for Selection {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        let selected = optional_layer(cx, self.value)?;
        if let Some(r) = self.shared.borrow_mut().records.get_mut(self.id) {
            r.focus_work = selected;
        }
        Ok(NativeStep::Return(value))
    }
}

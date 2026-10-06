//! Layer input stays in the window event's VM, including script callbacks.
use super::*;
pub(super) mod changes;
pub(super) mod focus;
pub(super) mod keyboard;
mod mouse;
pub(super) mod presentation;
use krkr_protocol::window::Input;
use tjs_core::{NativeContinuation, NativeCx, NativeStep};

pub(super) const NAMES: &[&str] = &[
    "onTransitionCompleted",
    "onPaint",
    "selfupdate",
    "callback",
    "time",
    "vague",
    "rule",
    "from",
    "stay",
    "onHitTest",
    "onMouseEnter",
    "onMouseLeave",
    "onMouseMove",
    "onMouseDown",
    "onMouseUp",
    "onClick",
    "onDoubleClick",
    "onBeforeFocus",
    "onBlur",
    "onFocus",
    "onSearchPrevFocusable",
    "onSearchNextFocusable",
    "onNodeEnabled",
    "onNodeDisabled",
    "onKeyDown",
    "onKeyUp",
    "onKeyPress",
    "onMouseWheel",
    "action",
];
#[derive(Default)]
pub(super) struct State {
    capture: Option<LayerId>,
    hover: Option<LayerId>,
    position: Option<(i64, i64)>,
    outside: bool,
    release: u64,
    focus: Option<LayerId>,
    modal: Vec<LayerId>,
    focus_lock: std::rc::Weak<()>,
    enabled_batch: std::rc::Weak<()>,
    presentation_lock: std::rc::Weak<()>,
}
impl Layers {
    pub(crate) fn release_window_captures(&mut self) {
        for state in self.input.values_mut() {
            state.capture = None;
            state.release = state.release.wrapping_add(1);
        }
    }
    pub(crate) fn trace_input(&self, window: WindowId, visit: &mut dyn FnMut(Value)) {
        if let Some(input) = self.input.get(&window) {
            for id in [input.capture, input.hover, input.focus]
                .into_iter()
                .flatten()
                .chain(input.modal.iter().copied())
            {
                if let Some(r) = self.records.get(id) {
                    r.owner.trace(visit);
                }
            }
        }
    }
    pub(crate) fn forget_input(&mut self, window: WindowId) {
        self.input.remove(&window);
    }
    pub(super) fn release_capture(&mut self, window: WindowId) {
        if let Some(state) = self.input.get_mut(&window) {
            state.capture = None;
            state.release = state.release.wrapping_add(1);
        }
    }
    pub(super) fn remove_input_layer(&mut self, id: LayerId) {
        for state in self.input.values_mut() {
            if state
                .focus
                .is_some_and(|child| belongs(&self.records, id, child))
            {
                state.focus = None;
            }
            state
                .modal
                .retain(|&child| !belongs(&self.records, id, child));
            if state
                .capture
                .is_some_and(|child| belongs(&self.records, id, child))
            {
                state.capture = None;
            }
            if state
                .hover
                .is_some_and(|child| belongs(&self.records, id, child))
            {
                state.hover = None;
            }
        }
    }
}
fn belongs(records: &SlotMap<LayerId, Record>, root: LayerId, mut id: LayerId) -> bool {
    loop {
        if id == root {
            return true;
        }
        let Some(parent) = records.get(id).and_then(|r| r.parent) else {
            return false;
        };
        id = parent;
    }
}
impl bindings::State {
    pub(super) fn input_update(&self, change: impl FnOnce(&mut Record)) -> NativeResult<()> {
        let lease = self.lease()?;
        change(lease.shared.borrow_mut().record_mut(lease.id)?);
        Ok(())
    }
    pub(super) fn event(
        &self,
        cx: &mut NativeCx<'_>,
        name: &'static str,
        fields: &[usize],
        args: &[Value],
    ) -> NativeResult<NativeStep> {
        self.event_then(cx, name, fields, args, Box::new(Returned))
    }
    pub(super) fn event_then(
        &self,
        cx: &mut NativeCx<'_>,
        name: &'static str,
        fields: &[usize],
        args: &[Value],
        completion: Box<dyn NativeContinuation>,
    ) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let (owner, event, key, keys) = {
            let world = lease.shared.borrow();
            (
                world.record(lease.id)?.action_owner,
                world.names[name],
                world.names["action"],
                world.windows.borrow().fields,
            )
        };
        crate::window::callbacks::action_then(
            cx, owner, event, key, &keys, fields, args, completion,
        )
    }
}
pub(crate) fn event(
    shared: Shared,
    window: WindowId,
    owner: Value,
    key: Value,
    args: Vec<Value>,
    input: Input,
) -> Box<dyn NativeContinuation> {
    Box::new(WindowEvent {
        shared,
        window,
        owner,
        key,
        args,
        input,
        phase: 0,
        result: Value::Void,
    })
}
struct WindowEvent {
    shared: Shared,
    window: WindowId,
    owner: Value,
    key: Value,
    args: Vec<Value>,
    input: Input,
    phase: u8,
    result: Value,
}
impl Trace for WindowEvent {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.key.trace(visit);
        self.args.trace(visit);
        self.result.trace(visit);
    }
}
impl NativeContinuation for WindowEvent {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        match self.phase {
            0 => {
                self.phase = 1;
                Ok(NativeStep::CallMember {
                    object: self.owner,
                    key: self.key,
                    arguments: std::mem::take(&mut self.args),
                    continuation: self,
                })
            }
            1 => {
                self.result = value;
                self.phase = 2;
                if !self.shared.borrow().windows.borrow().is_live(self.window) {
                    return Ok(NativeStep::Return(value));
                }
                self.shared
                    .borrow_mut()
                    .input
                    .entry(self.window)
                    .or_default();
                if let Input::Wheel { x, y, .. } = &mut self.input {
                    (*x, *y) = self.shared.borrow().input_point(self.window, (*x, *y))?;
                }
                if matches!(
                    self.input,
                    Input::KeyDown { .. }
                        | Input::KeyUp { .. }
                        | Input::KeyPress(_)
                        | Input::Wheel { .. }
                ) {
                    return Ok(keyboard::start(
                        self.shared.clone(),
                        self.window,
                        self.input,
                        self,
                    ));
                }
                let (x, y, shift) = match self.input {
                    Input::MouseMove { x, y, shift }
                    | Input::MouseDown { x, y, shift, .. }
                    | Input::MouseUp { x, y, shift, .. } => (x.into(), y.into(), shift),
                    Input::Click { x, y } | Input::DoubleClick { x, y } => (x.into(), y.into(), 0),
                    Input::MouseLeave => (-1, -1, 0),
                    _ => return Ok(NativeStep::Return(value)),
                };
                let (x, y) = if matches!(self.input, Input::MouseLeave) {
                    (x, y)
                } else {
                    let point = self
                        .shared
                        .borrow()
                        .input_point(self.window, (x as i32, y as i32))?;
                    (i64::from(point.0), i64::from(point.1))
                };
                Ok(mouse::start(
                    self.shared.clone(),
                    self.window,
                    self.input,
                    (x, y, shift),
                    self,
                ))
            }
            _ => Ok(NativeStep::Return(self.result)),
        }
    }
}

fn owner(world: &Layers, id: Option<LayerId>) -> Value {
    id.and_then(|id| world.records.get(id))
        .map_or(null(), |r| object(r.owner))
}
fn trace_layer(world: &Layers, id: Option<LayerId>, visit: &mut dyn FnMut(Value)) {
    if let Some(r) = id.and_then(|id| world.records.get(id)) {
        r.owner.trace(visit);
    }
}
fn send(
    shared: &Shared,
    id: Option<LayerId>,
    name: &'static str,
    arguments: Vec<Value>,
    continuation: Box<dyn NativeContinuation>,
) -> NativeStep {
    let world = shared.borrow();
    if let Some(r) = id
        .and_then(|id| world.records.get(id))
        .filter(|r| !r.shutdown)
    {
        NativeStep::CallMember {
            object: object(r.owner),
            key: world.names[name],
            arguments,
            continuation,
        }
    } else {
        NativeStep::Continue(continuation)
    }
}
pub(super) struct Returned;
impl Trace for Returned {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Returned {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(value))
    }
}

// Rechecks use the latest delivered position, and do not synthesize a Window callback.
pub(crate) fn recheck(
    shared: Shared,
    window: WindowId,
    owner: ObjId,
) -> Box<dyn NativeContinuation> {
    Box::new(Recheck {
        shared,
        window,
        owner,
    })
}
struct Recheck {
    shared: Shared,
    window: WindowId,
    owner: ObjId,
}
impl Trace for Recheck {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Recheck {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let world = self.shared.borrow();
        let position = world.input.get(&self.window).and_then(|s| s.position);
        if !world.windows.borrow().is_live(self.window) {
            return Ok(NativeStep::Return(Value::Void));
        }
        if position.is_none() || world.input.get(&self.window).is_some_and(|s| s.outside) {
            drop(world);
            return presentation::sync(
                &self.shared,
                self.window,
                cx,
                Value::Void,
                Box::new(Returned),
            );
        }
        let point = world.windows.borrow().cursor_position(self.window)?;
        let (x, y) = world.input_point(self.window, point)?;
        drop(world);
        Ok(mouse::start(
            self.shared,
            self.window,
            Input::MouseMove { x, y, shift: 0 },
            (x.into(), y.into(), 0),
            Box::new(Returned),
        ))
    }
}

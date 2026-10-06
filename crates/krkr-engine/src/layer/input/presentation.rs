//! Resolve inherited pointer/IME settings using VM-owned metadata. Only plain
//! presentation data crosses to the host, and unchanged settings send no command.
use super::*;
use krkr_protocol::input_style::Style;

impl Layers {
    pub(in crate::layer) fn origin(&self, mut id: LayerId) -> NativeResult<(i64, i64)> {
        let (mut x, mut y) = (0, 0);
        loop {
            let r = self.record(id)?;
            let Some(parent) = r.parent else {
                return Ok((x, y));
            };
            x += i64::from(r.geometry.left);
            y += i64::from(r.geometry.top);
            id = parent;
        }
    }
    fn active_cursor(&self, mut id: Option<LayerId>) -> i32 {
        while let Some(r) = id.and_then(|id| self.records.get(id)) {
            if r.cursor != 0 {
                return r.cursor;
            }
            id = r.parent;
        }
        0
    }
    fn active_hint(&self, pointer: Option<LayerId>) -> Option<(Option<ObjId>, Arc<[u16]>)> {
        let Some(id) = pointer else {
            return Some((None, Arc::from([])));
        };
        let mut r = self.records.get(id)?;
        if r.ignore_hint_sensing {
            return None;
        }
        let owner = r.owner;
        while r.show_parent_hint {
            let Some(parent) = r.parent.and_then(|id| self.records.get(id)) else {
                break;
            };
            r = parent;
        }
        Some((Some(owner), r.hint.clone()))
    }
    fn presentation(&self, window: WindowId, pointer: Option<LayerId>, default_ime: i32) -> Style {
        let mut style = Style {
            cursor: self.active_cursor(pointer),
            ime: default_ime,
            attention: None,
        };
        let focused = self
            .focused(window)
            .and_then(|id| self.records.get(id))
            .filter(|r| !r.shutdown);
        if let Some(focused) = focused {
            style.ime = focused.ime;
            let mut id = self.focused(window);
            while let Some((key, r)) = id.and_then(|id| self.records.get(id).map(|r| (id, r))) {
                if r.use_attention {
                    if let Ok((x, y)) = self.origin(key) {
                        style.attention = Some(Rect {
                            left: (x + i64::from(r.attention.0)) as i32,
                            top: (y + i64::from(r.attention.1)) as i32,
                            width: 1,
                            height: focused.font.height.unsigned_abs().max(1),
                        });
                    }
                    break;
                }
                id = r.parent;
            }
        }
        if let Ok((viewport, size)) = self.viewport(window) {
            style.attention = style.attention.map(|rect| viewport.attention(size, rect));
        }
        style
    }
}
pub(super) fn hint(
    shared: &Shared,
    window: WindowId,
    pointer: Option<LayerId>,
) -> NativeResult<()> {
    let world = shared.borrow();
    if let Some((sender, text)) = world.active_hint(pointer) {
        world.windows.borrow_mut().set_hint(window, sender, text)?;
    }
    Ok(())
}
pub(crate) fn sync(
    shared: &Shared,
    window: WindowId,
    cx: &mut NativeCx<'_>,
    result: Value,
    completion: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let pointer = shared
        .borrow()
        .input
        .get(&window)
        .and_then(|s| s.capture.or(s.hover));
    apply(shared, window, pointer, cx, result, completion)
}
fn apply(
    shared: &Shared,
    window: WindowId,
    pointer: Option<LayerId>,
    cx: &mut NativeCx<'_>,
    result: Value,
    completion: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let world = shared.borrow();
    let windows = world.windows.clone();
    if !windows.borrow().contains(window) {
        drop(world);
        return completion.resume(cx, result);
    }
    let (previous, default_ime) = windows.borrow().input_style(window)?;
    let style = world.presentation(window, pointer, default_ime);
    drop(world);
    if Some(style) == previous {
        return completion.resume(cx, result);
    }
    crate::window::input::apply_style(
        &windows,
        window,
        style,
        Box::new(Done { result, completion }),
    )
}
struct Done {
    result: Value,
    completion: Box<dyn NativeContinuation>,
}
impl Trace for Done {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.result.trace(visit);
        self.completion.trace(visit);
    }
}
impl NativeContinuation for Done {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.completion.resume(cx, self.result)
    }
}
impl bindings::State {
    pub(in crate::layer) fn sync_ime(&self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let window = lease.shared.borrow().record(lease.id)?.window;
        if lease.shared.borrow().focused(window) != Some(lease.id) {
            return Ok(NativeStep::Return(Value::Void));
        }
        sync(&lease.shared, window, cx, Value::Void, Box::new(Returned))
    }
    pub(in crate::layer) fn get_cursor_pos(&self, x: bool) -> NativeResult<i64> {
        let lease = self.lease()?;
        let world = lease.shared.borrow();
        let window = world.record(lease.id)?.window;
        let position = world.windows.borrow().cursor_position(window)?;
        let position = world.input_point(window, position)?;
        let origin = world.origin(lease.id)?;
        Ok((if x {
            i64::from(position.0) - origin.0
        } else {
            i64::from(position.1) - origin.1
        }) as i32 as i64)
    }
    pub(in crate::layer) fn move_cursor(&self, x: i32, y: i32) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let world = lease.shared.borrow();
        let window = world.record(lease.id)?.window;
        let (ox, oy) = world.origin(lease.id)?;
        let (viewport, size) = world.viewport(window)?;
        let (x, y) = viewport.to_window(
            size,
            ((ox + i64::from(x)) as i32, (oy + i64::from(y)) as i32),
        );
        crate::window::input::move_cursor(&world.windows, window, x, y)
    }
    pub(in crate::layer) fn notify_cursor(&self) -> NativeResult<NativeStep> {
        self.notify_presentation(false)
    }
    pub(in crate::layer) fn notify_hint(&self) -> NativeResult<NativeStep> {
        self.notify_presentation(true)
    }
    fn notify_presentation(&self, hint: bool) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        let mut world = lease.shared.borrow_mut();
        let window = world.record(lease.id)?.window;
        let state = world.input.entry(window).or_default();
        if state.presentation_lock.upgrade().is_some() {
            return Ok(NativeStep::Return(Value::Void));
        }
        let lock = Rc::new(());
        state.presentation_lock = Rc::downgrade(&lock);
        let capture = state.capture;
        let (x, y) = state.position.unwrap_or((-1, -1));
        let task = Box::new(Refresh {
            shared: lease.shared.clone(),
            id: lease.id,
            window,
            capture,
            hint,
            _lock: lock,
        });
        drop(world);
        Ok(if capture.is_some() {
            NativeStep::Continue(task)
        } else {
            hit::start(&lease.shared, window, x, y, None, false, Some(task))
        })
    }
}
struct Refresh {
    shared: Shared,
    id: LayerId,
    window: WindowId,
    capture: Option<LayerId>,
    hint: bool,
    _lock: Rc<()>,
}
impl Trace for Refresh {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        let world = self.shared.borrow();
        trace_layer(&world, Some(self.id), visit);
        trace_layer(&world, self.capture, visit);
    }
}
impl NativeContinuation for Refresh {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        let target = if self.capture.is_some() {
            self.capture
        } else {
            focus::optional_layer(cx, value)?
        };
        if target != Some(self.id) {
            return Ok(NativeStep::Return(Value::Void));
        }
        if self.hint {
            hint(&self.shared, self.window, target)?;
            Ok(NativeStep::Return(Value::Void))
        } else {
            apply(
                &self.shared,
                self.window,
                target,
                cx,
                Value::Void,
                Box::new(Returned),
            )
        }
    }
}

use super::*;
use crate::operations::{Operations, Request};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, ObjRef, WaitMode};

pub(super) enum Change {
    None,
    Caption(Vec<u16>),
    Visible(bool),
    StayOnTop(bool),
    FullScreen(bool),
    BorderStyle(krkr_protocol::window::BorderStyle),
    DisableResize(bool),
    DisableMove(bool),
    MinSize(u32, u32),
    MaxSize(u32, u32),
    InputStyle(krkr_protocol::input_style::Style, u64),
    CursorState(i32),
    CursorPosition(i32, i32),
}
struct Updated {
    shared: Shared,
    id: WindowId,
    delivery: Delivery,
    change: Change,
    created: Option<Lease>,
    completion: Box<dyn NativeContinuation>,
    snapshot_required: bool,
    fullscreen_basis: krkr_protocol::graphics::Size,
    sets_client_basis: bool,
}

// TVPWindow::CheckMinMaxSize applies maximum first, then minimum (minimum
// wins for a temporarily inconsistent pair). Zero means unconstrained.
fn constrained(size: (u32, u32), minimum: (u32, u32), maximum: (u32, u32)) -> (u32, u32) {
    fn axis(value: u32, minimum: u32, maximum: u32) -> u32 {
        let value = if maximum == 0 {
            value
        } else {
            value.min(maximum)
        };
        value.max(minimum)
    }
    (
        axis(size.0, minimum.0, maximum.0),
        axis(size.1, minimum.1, maximum.1),
    )
}
impl Trace for Updated {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.completion.trace(visit);
        if let Ok(record) = self.shared.borrow().record(self.id) {
            record.owner.trace(visit);
        }
    }
}
impl NativeContinuation for Updated {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let response = self
            .delivery
            .borrow_mut()
            .take()
            .expect("window completion");
        let geometry = match response {
            krkr_protocol::window::Response::Geometry(geometry) if !self.snapshot_required => {
                geometry
            }
            krkr_protocol::window::Response::Snapshot(snapshot) => {
                let mut world = self.shared.borrow_mut();
                let record = world.record_mut(self.id)?;
                record.snapshot = Some(snapshot);
                record.visible = snapshot.visible;
                snapshot.geometry
            }
            _ => return Err(NativeError::Message("unexpected window response")),
        };
        let check_size = matches!(self.change, Change::MinSize(..) | Change::MaxSize(..));
        let hidden = matches!(self.change, Change::Visible(false));
        let resize = {
            let mut world = self.shared.borrow_mut();
            let old_viewport = world.viewport(self.id)?;
            let record = world.record_mut(self.id)?;
            let client_changed = record.geometry.inner_width != geometry.inner_width
                || record.geometry.inner_height != geometry.inner_height;
            record.geometry = geometry;
            if self.sets_client_basis && record.full_screen.is_none() {
                record.client_basis = Some(krkr_protocol::graphics::Size {
                    width: geometry.inner_width,
                    height: geometry.inner_height,
                });
            }
            match std::mem::replace(&mut self.change, Change::None) {
                Change::None => {}
                Change::Caption(v) => record.caption = v,
                Change::Visible(v) => record.visible = v,
                Change::StayOnTop(v) => record.stay_on_top = v,
                Change::FullScreen(v) => record.full_screen = v.then_some(self.fullscreen_basis),
                Change::BorderStyle(v) => record.border_style = v,
                Change::DisableResize(v) => record.disable_resize = v,
                Change::DisableMove(v) => record.disable_move = v,
                Change::MinSize(w, h) => record.min_size = (w, h),
                Change::MaxSize(w, h) => record.max_size = (w, h),
                Change::InputStyle(style, revision) => {
                    if record.style_revision == revision {
                        record.input_style = Some(style);
                    }
                }
                Change::CursorState(state) => record.cursor_state = state,
                Change::CursorPosition(x, y) => {
                    record.cursor = (x, y);
                    if record.cursor_state == 1 {
                        record.cursor_state = 0;
                    }
                }
            }
            let current = (geometry.width, geometry.height);
            let wanted = constrained(current, record.min_size, record.max_size);
            let resize = (check_size && wanted != current).then_some(wanted);
            if client_changed || world.viewport(self.id)? != old_viewport {
                world.invalidate_viewport(self.id);
            }
            if hidden {
                world.finish_modal(self.id);
            }
            resize
        };
        if let Some((width, height)) = resize {
            // Setting a native min/max hint need not resize a window. Keep the
            // API synchronous until the actual adjusted geometry is confirmed.
            return request_then(
                &self.shared,
                self.id,
                Command::Size {
                    width,
                    height,
                    inner: false,
                },
                Change::None,
                self.created.take(),
                self.completion,
            );
        }
        if let Some(lease) = self.created.take() {
            // Preserve associations registered by the derived constructor before
            // it called super.Window; host creation only completes the lease.
            let owner = cx.this();
            cx.heap_mut()
                .with_native_state::<super::bindings::State, _>(owner, |s| s.lease = Some(lease))?;
            super::draw_device::create_default(cx)?;
            return Ok(NativeStep::Return(object(owner)));
        }
        self.completion.resume(cx, Value::Void)
    }
}
pub(super) fn request(
    shared: &Shared,
    id: WindowId,
    command: Command,
    change: Change,
    created: Option<Lease>,
) -> NativeResult<NativeStep> {
    request_then(shared, id, command, change, created, Box::new(Returned))
}
pub(super) fn request_then(
    shared: &Shared,
    id: WindowId,
    command: Command,
    change: Change,
    created: Option<Lease>,
    completion: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let (host, operations, command, fullscreen_basis) = {
        let world = shared.borrow();
        let record = world.record(id)?;
        let command = if let Command::Size {
            width,
            height,
            inner,
        } = command
        {
            let geometry = record.geometry;
            let chrome = (
                geometry.width.saturating_sub(geometry.inner_width),
                geometry.height.saturating_sub(geometry.inner_height),
            );
            let outer = if inner {
                (
                    width.saturating_add(chrome.0),
                    height.saturating_add(chrome.1),
                )
            } else {
                (width, height)
            };
            let outer = constrained(outer, record.min_size, record.max_size);
            let (width, height) = if inner {
                (
                    outer.0.saturating_sub(chrome.0).max(1),
                    outer.1.saturating_sub(chrome.1).max(1),
                )
            } else {
                outer
            };
            Command::Size {
                width,
                height,
                inner,
            }
        } else {
            command
        };
        (
            world.host.clone().ok_or(NativeError::Message(
                "Window requires a platform window host",
            ))?,
            world.operations.clone(),
            command,
            record.full_screen.unwrap_or(krkr_protocol::graphics::Size {
                width: record.geometry.inner_width,
                height: record.geometry.inner_height,
            }),
        )
    };
    let snapshot_required = matches!(
        command,
        Command::QueryState
            | Command::Control(_)
            | Command::MaximizeBox(_)
            | Command::MinimizeBox(_)
            | Command::DisableResize(_)
            | Command::ZOrder { .. }
    );
    let sets_client_basis = matches!(command, Command::Create { .. } | Command::Size { .. });
    let ticket = host.request(id, command).map_err(NativeError::Detail)?;
    let delivery = Delivery::default();
    Operations::wait(
        &operations,
        Request::Window(ticket, delivery.clone()),
        WaitMode::Internal,
        Box::new(Updated {
            shared: shared.clone(),
            id,
            delivery,
            change,
            created,
            completion,
            snapshot_required,
            fullscreen_basis,
            sets_client_basis,
        }),
    )
}
pub(super) fn object(id: ObjId) -> Value {
    Value::Obj(ObjRef::bound(id))
}
pub(super) struct Returned;
impl Trace for Returned {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Returned {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Void))
    }
}
pub(super) struct Closing {
    pub shared: Shared,
    pub id: WindowId,
    pub owner: ObjId,
    pub allowed: Rc<Cell<bool>>,
}
impl Drop for Closing {
    fn drop(&mut self) {
        if let Some(record) = self.shared.borrow_mut().records.get_mut(self.id)
            && record
                .closing
                .as_ref()
                .is_some_and(|v| Rc::ptr_eq(v, &self.allowed))
        {
            record.closing = None;
        }
    }
}
impl Trace for Closing {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Closing {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if self.allowed.get() {
            if self.shared.borrow_mut().finish_modal(self.id) {
                return Ok(NativeStep::Return(Value::Void));
            }
            Ok(NativeStep::Invalidate {
                object: object(self.owner),
                continuation: Box::new(Returned),
            })
        } else {
            Ok(NativeStep::Return(Value::Void))
        }
    }
}

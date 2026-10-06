//! Portable windowEx access to the same window record and request continuations.
use super::{
    Shared,
    bindings::State,
    tasks::{self, Change},
};
use krkr_protocol::window::{Command, Control, WindowId, ZOrder};
use tjs_core::{NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace, Value};

#[derive(Clone, Copy)]
pub enum Property {
    Maximized,
    Minimized,
    MaximizeBox,
    MinimizeBox,
    DisableResize,
    DisableMove,
}
#[derive(Clone, Copy)]
pub enum Rectangle {
    Window,
    Client,
    Normal { nofix: bool },
}

fn window(cx: &mut NativeCx<'_>) -> NativeResult<(Shared, WindowId)> {
    let owner = cx.this();
    cx.heap_mut()
        .with_native_state::<State, _>(owner, |state| {
            let lease = state.lease()?;
            lease
                .shared
                .borrow_mut()
                .record_mut(lease.id)?
                .extended_events = true;
            Ok((lease.shared.clone(), lease.id))
        })?
}

pub fn control(cx: &mut NativeCx<'_>, control: Control) -> NativeResult<NativeStep> {
    let (shared, id) = window(cx)?;
    if matches!(control, Control::Maximize | Control::Maximized(true)) {
        return super::extended::query_maximize(cx, shared, id, control);
    }
    tasks::request(&shared, id, Command::Control(control), Change::None, None)
}

pub fn set(cx: &mut NativeCx<'_>, property: Property, enabled: bool) -> NativeResult<NativeStep> {
    let (shared, id) = window(cx)?;
    if matches!(property, Property::Maximized) && enabled {
        return super::extended::query_maximize(cx, shared, id, Control::Maximized(true));
    }
    let (command, change) = match property {
        Property::Maximized => (Command::Control(Control::Maximized(enabled)), Change::None),
        Property::Minimized => (Command::Control(Control::Minimized(enabled)), Change::None),
        Property::MaximizeBox => (Command::MaximizeBox(enabled), Change::None),
        Property::MinimizeBox => (Command::MinimizeBox(enabled), Change::None),
        Property::DisableResize => (
            Command::DisableResize(enabled),
            Change::DisableResize(enabled),
        ),
        Property::DisableMove => (Command::DisableMove(enabled), Change::DisableMove(enabled)),
    };
    tasks::request(&shared, id, command, change, None)
}

enum Read {
    Property(Property),
    Rectangle(Rectangle),
}
struct Readback {
    shared: Shared,
    id: WindowId,
    read: Read,
}
impl Trace for Readback {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(owner) = self.shared.borrow().owner(self.id) {
            owner.trace(visit);
        }
    }
}
impl NativeContinuation for Readback {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let snapshot = self
            .shared
            .borrow()
            .record(self.id)?
            .snapshot
            .ok_or(NativeError::Message("host did not return window state"))?;
        let rectangle = match self.read {
            Read::Property(property) => {
                let value = match property {
                    Property::Maximized => Some(snapshot.maximized),
                    Property::Minimized => snapshot.minimized,
                    Property::MaximizeBox => snapshot.maximize_box,
                    Property::MinimizeBox => snapshot.minimize_box,
                    Property::DisableResize | Property::DisableMove => unreachable!(),
                }
                .ok_or(NativeError::Message(
                    "window property is unavailable on this backend",
                ))?;
                return Ok(NativeStep::Return(Value::Int(value.into())));
            }
            Read::Rectangle(Rectangle::Window) => snapshot.outer,
            Read::Rectangle(Rectangle::Client) => snapshot.client,
            Read::Rectangle(Rectangle::Normal { nofix: false }) => snapshot.normal,
            Read::Rectangle(Rectangle::Normal { nofix: true }) => snapshot.normal_workspace,
        };
        // Original windowEx returns void when the platform rectangle query fails.
        let Some(rectangle) = rectangle else {
            return Ok(NativeStep::Return(Value::Void));
        };
        let heap = cx.heap_mut();
        let object = heap.alloc_dictionary();
        for (name, value) in [
            ("x", i64::from(rectangle.x)),
            ("y", i64::from(rectangle.y)),
            ("w", i64::from(rectangle.width)),
            ("h", i64::from(rectangle.height)),
        ] {
            let key = heap.intern_str(name);
            heap.set_member(object, key, Value::Int(value))?;
        }
        Ok(NativeStep::Return(Value::Obj(tjs_core::ObjRef::bound(
            object,
        ))))
    }
}
fn read(cx: &mut NativeCx<'_>, read: Read) -> NativeResult<NativeStep> {
    let (shared, id) = window(cx)?;
    // Never reuse a prior response for a new query (including after a host error).
    shared.borrow_mut().record_mut(id)?.snapshot = None;
    tasks::request_then(
        &shared,
        id,
        Command::QueryState,
        Change::None,
        None,
        Box::new(Readback {
            shared: shared.clone(),
            id,
            read,
        }),
    )
}
pub fn get(cx: &mut NativeCx<'_>, property: Property) -> NativeResult<NativeStep> {
    if matches!(property, Property::DisableResize | Property::DisableMove) {
        let (shared, id) = window(cx)?;
        let world = shared.borrow();
        let record = world.record(id)?;
        let value = if matches!(property, Property::DisableMove) {
            record.disable_move
        } else {
            record.disable_resize
        };
        return Ok(NativeStep::Return(Value::Int(value.into())));
    }
    read(cx, Read::Property(property))
}

/// Upstream reads all four fields, but SWP_NOMOVE means x/y do not move the window.
pub fn set_client_rect(cx: &mut NativeCx<'_>, rect: Value) -> NativeResult<NativeStep> {
    if !matches!(rect, Value::Obj(_)) {
        return Err(NativeError::Type("a rectangle object"));
    }
    let (shared, id) = window(cx)?;
    tasks::request_then(
        &shared,
        id,
        Command::QueryState,
        Change::None,
        None,
        Box::new(ClientRect {
            shared: shared.clone(),
            id,
            rect,
            index: 0,
            dimensions: [0; 2],
            reading: false,
        }),
    )
}
struct ClientRect {
    shared: Shared,
    id: WindowId,
    rect: Value,
    index: usize,
    dimensions: [u32; 2],
    reading: bool,
}
impl Trace for ClientRect {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.rect.trace(visit);
        if let Some(owner) = self.shared.borrow().owner(self.id) {
            owner.trace(visit);
        }
    }
}
impl NativeContinuation for ClientRect {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if !self.reading {
            let snapshot = self.shared.borrow().record(self.id)?.snapshot;
            let Some(snapshot) = snapshot.filter(|s| s.client.is_some() && s.outer.is_some())
            else {
                return Ok(NativeStep::Return(Value::Void));
            };
            self.dimensions = [
                snapshot.geometry.inner_width,
                snapshot.geometry.inner_height,
            ];
            self.reading = true;
        } else {
            {
                let number = tjs_core::value::to_integer(cx.heap(), value)? as i32;
                if self.index >= 2 {
                    if number < 0 {
                        return Ok(NativeStep::Return(Value::Int(0)));
                    }
                    self.dimensions[self.index - 2] = number as u32;
                }
            }
            self.index += 1;
        }
        if self.index < 4 {
            let key = Value::Str(
                cx.heap_mut().alloc_string(
                    ["x", "y", "w", "h"][self.index]
                        .encode_utf16()
                        .collect::<Vec<_>>(),
                ),
            );
            let fallback = if self.index >= 2 {
                self.dimensions[self.index - 2] as i64
            } else {
                0
            };
            // ncbPropAccessor uses MEMBERMUSTEXIST so a missing Dictionary
            // field gets the current dimension; an explicitly stored void
            // still converts to zero. Ordinary Dictionary lookup returns void
            // for absence and would lose that distinction here.
            return Ok(NativeStep::GetRequiredOr {
                object: self.rect,
                key,
                fallback: Value::Int(fallback),
                continuation: self,
            });
        }
        tasks::request_then(
            &self.shared,
            self.id,
            Command::Size {
                width: self.dimensions[0],
                height: self.dimensions[1],
                inner: true,
            },
            Change::None,
            None,
            Box::new(ClientRectDone),
        )
    }
}
struct ClientRectDone;
impl Trace for ClientRectDone {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for ClientRectDone {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Int(1)))
    }
}
pub fn rectangle(cx: &mut NativeCx<'_>, rectangle: Rectangle) -> NativeResult<NativeStep> {
    read(cx, Read::Rectangle(rectangle))
}
pub fn z_order(
    cx: &mut NativeCx<'_>,
    target: Option<Value>,
    activate: bool,
) -> NativeResult<NativeStep> {
    let (shared, id) = window(cx)?;
    let order = match target {
        Some(Value::Int(1)) => ZOrder::Bottom,
        Some(Value::Int(-1)) => ZOrder::Topmost,
        Some(Value::Int(-2)) => ZOrder::NotTopmost,
        Some(Value::Int(0)) | None => ZOrder::Top,
        Some(Value::Int(_)) => {
            return Err(NativeError::Message(
                "native window handles are not supported",
            ));
        }
        Some(Value::Str(string)) => {
            let text = String::from_utf16_lossy(cx.heap().string(string)?).to_ascii_lowercase();
            match text.as_str() {
                "bottom" => ZOrder::Bottom,
                "topmost" => ZOrder::Topmost,
                "notopmost" => ZOrder::NotTopmost,
                _ => ZOrder::Top,
            }
        }
        Some(Value::Obj(object)) if object.object.is_some() => {
            ZOrder::Behind(super::bindings::id(cx.heap_mut(), object.object.unwrap())?)
        }
        _ => ZOrder::Top,
    };
    let change = match order {
        ZOrder::Topmost => Change::StayOnTop(true),
        ZOrder::NotTopmost | ZOrder::Bottom => Change::StayOnTop(false),
        _ => Change::None,
    };
    tasks::request(
        &shared,
        id,
        Command::ZOrder { order, activate },
        change,
        None,
    )
}

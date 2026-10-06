//! Script-posted keyboard events use the same bounded FIFO as desktop input.
use super::*;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, ObjRef, value};

impl Windows {
    pub(crate) fn input_style(
        &self,
        id: WindowId,
    ) -> NativeResult<(Option<krkr_protocol::input_style::Style>, i32)> {
        let r = self.record(id)?;
        Ok((r.input_style, r.default_ime))
    }
    pub(crate) fn cursor_position(&self, id: WindowId) -> NativeResult<(i32, i32)> {
        Ok(self.record(id)?.cursor)
    }
}
pub(crate) fn apply_style(
    shared: &Shared,
    window: WindowId,
    style: krkr_protocol::input_style::Style,
    completion: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let revision = {
        let mut world = shared.borrow_mut();
        let r = world.record_mut(window)?;
        r.style_revision = r.style_revision.wrapping_add(1);
        // A cancelled wait may already have changed the host. Reconcile on the
        // next presentation update instead of trusting the previous cache.
        r.input_style = None;
        r.style_revision
    };
    tasks::request_then(
        shared,
        window,
        Command::InputStyle(style),
        tasks::Change::InputStyle(style, revision),
        None,
        completion,
    )
}
pub(crate) fn move_cursor(
    shared: &Shared,
    window: WindowId,
    x: i32,
    y: i32,
) -> NativeResult<NativeStep> {
    tasks::request(
        shared,
        window,
        Command::CursorPosition(x, y),
        tasks::Change::CursorPosition(x, y),
        None,
    )
}

pub(super) fn post(
    lease: &Lease,
    cx: &mut NativeCx<'_>,
    name: Value,
    params: Option<Value>,
) -> NativeResult<NativeStep> {
    let Value::Str(name) = value::to_string(cx.heap_mut(), name)? else {
        unreachable!()
    };
    let text = tjs_core::string::c_string(cx.heap().string(name)?);
    let kind = match text {
        x if x.iter().copied().eq("onKeyDown".encode_utf16()) => 0,
        x if x.iter().copied().eq("onKeyUp".encode_utf16()) => 1,
        x if x.iter().copied().eq("onKeyPress".encode_utf16()) => 2,
        _ => return Err(NativeError::Message("unknown input event name")),
    };
    let params = params.ok_or(NativeError::Missing(2))?;
    if !matches!(
        params,
        Value::Obj(ObjRef {
            object: Some(_),
            ..
        })
    ) {
        return Err(NativeError::Type("an event parameter object"));
    }
    Ok(NativeStep::Get {
        object: params,
        key: lease.shared.borrow().post_keys[0],
        continuation: Box::new(Post {
            shared: lease.shared.clone(),
            window: lease.id,
            params,
            kind,
            key: None,
        }),
    })
}
struct Post {
    shared: Shared,
    window: WindowId,
    params: Value,
    kind: u8,
    key: Option<u32>,
}
impl Trace for Post {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.params.trace(visit);
        if let Some(r) = self.shared.borrow().records.get(self.window) {
            r.owner.trace(visit);
        }
    }
}
impl NativeContinuation for Post {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if !self.shared.borrow().contains(self.window) {
            return Ok(NativeStep::Return(Value::Void));
        }
        let number = value::to_integer(cx.heap(), result)? as u32;
        let input = if let Some(key) = self.key {
            if self.kind == 0 {
                Input::KeyDown { key, shift: number }
            } else {
                Input::KeyUp {
                    key,
                    shift: number & 7,
                }
            }
        } else if self.kind == 2 {
            // Both reference hosts pass through a signed char at this entry.
            // OS IME input remains full UTF-16; this is the legacy posting API.
            Input::KeyPress(number as u8 as i8 as u16)
        } else {
            self.key = Some(number as u16 as u32);
            let key = self.shared.borrow().post_keys[1];
            return Ok(NativeStep::Get {
                object: self.params,
                key,
                continuation: self,
            });
        };
        let mut world = self.shared.borrow_mut();
        if !world.accepts_input(self.window) {
            return Ok(NativeStep::Return(Value::Void));
        }
        let source = world.record(self.window)?.source;
        world.events.borrow_mut().post(source, 1, 0)?;
        world
            .record_mut(self.window)?
            .pending
            .push_back(Pending::Input(input));
        Ok(NativeStep::Return(Value::Void))
    }
}

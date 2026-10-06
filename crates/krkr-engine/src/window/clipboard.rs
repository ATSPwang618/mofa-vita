//! Clipboard notifications enter the existing ordered Window event queue.
use super::*;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, ObjRef};
fn window(cx: &mut NativeCx<'_>) -> NativeResult<(Shared, WindowId)> {
    let owner = cx.this();
    cx.heap_mut()
        .with_native_state::<bindings::State, _>(owner, |s| {
            let lease = s.lease()?;
            Ok((lease.shared.clone(), lease.id))
        })?
}
pub(crate) fn get(cx: &mut NativeCx<'_>) -> NativeResult<Value> {
    let (shared, id) = window(cx)?;
    Ok(Value::Int(
        shared.borrow().record(id)?.clipboard_watch.is_some().into(),
    ))
}
pub(crate) fn set(cx: &mut NativeCx<'_>, enabled: bool) -> NativeResult<()> {
    let (shared, id) = window(cx)?;
    if !shared.borrow().is_live(id) {
        return Err(NativeError::Message("window is closing"));
    }
    if shared.borrow().record(id)?.clipboard_watch.is_some() == enabled {
        return Ok(());
    }
    let watch = if enabled {
        let wake = shared.borrow().operations.borrow().waker();
        Some(crate::clipboard::subscribe(cx.heap_mut(), wake)?)
    } else {
        None
    };
    shared.borrow_mut().record_mut(id)?.clipboard_watch = watch;
    Ok(())
}
pub(crate) fn release(heap: &mut Heap) -> NativeResult<()> {
    let class = heap.registered_class("Window").ok_or(NativeError::This)?;
    heap.with_native_state::<bindings::State, _>(class, |s| {
        if let Some(shared) = &s.service {
            for record in shared.borrow_mut().records.values_mut() {
                record.clipboard_watch = None;
            }
        }
    })
}
impl Windows {
    pub(super) fn pump_clipboard(&mut self) {
        for record in self
            .records
            .values_mut()
            .filter(|r| r.registered && r.invalidating.strong_count() == 0)
        {
            let Some(watch) = &mut record.clipboard_watch else {
                continue;
            };
            if let Some(error) = watch.error() {
                if self.events.borrow_mut().post(record.source, 1, 0).is_err() {
                    break;
                }
                record.clipboard_watch = None;
                record.pending.push_back(Pending::ClipboardError(error));
                continue;
            }
            let current = watch.current();
            if current == watch.revision {
                continue;
            }
            if self.events.borrow_mut().post(record.source, 1, 0).is_err() {
                break;
            }
            watch.revision = current;
            record
                .pending
                .push_back(Pending::Clipboard(Rc::downgrade(&watch.token)));
        }
    }
}
struct Changed {
    owner: ObjId,
    token: std::rc::Weak<()>,
    phase: u8,
}
impl Trace for Changed {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Changed {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if self.token.strong_count() == 0 || !cx.heap().is_valid(self.owner)? {
            return Ok(NativeStep::Return(Value::Void));
        }
        if self.phase == 0 || (self.phase == 1 && matches!(result, Value::Void)) {
            let name = if self.phase == 0 {
                "onDrawClipboard"
            } else {
                "onClipboardChanged"
            };
            self.phase += 1;
            let key = Value::Str(
                cx.heap_mut()
                    .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
            );
            return Ok(NativeStep::GetOptional {
                object: Value::Obj(ObjRef::bound(self.owner)),
                key,
                continuation: self,
            });
        }
        if self.phase <= 2 && !matches!(result, Value::Void) {
            self.phase = 3;
            let function = match result {
                Value::Obj(mut r) => {
                    r.this = Some(self.owner);
                    Value::Obj(r)
                }
                _ => result,
            };
            return Ok(NativeStep::CallDiscard {
                function,
                arguments: vec![],
                continuation: self,
            });
        }
        Ok(NativeStep::Return(Value::Void))
    }
}
pub(super) fn callback(
    owner: ObjId,
    token: std::rc::Weak<()>,
) -> Option<Box<dyn NativeContinuation>> {
    (token.strong_count() != 0).then(|| {
        Box::new(Changed {
            owner,
            token,
            phase: 0,
        }) as Box<dyn NativeContinuation>
    })
}

struct Failed(String);
impl Trace for Failed {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Failed {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Err(NativeError::Detail(self.0))
    }
}
pub(super) fn failure(error: String) -> Box<dyn NativeContinuation> {
    Box::new(Failed(error))
}

pub(crate) fn staging_budget(
    heap: &mut Heap,
) -> NativeResult<Option<krkr_protocol::budget::Budget>> {
    let class = heap.registered_class("Window").ok_or(NativeError::This)?;
    heap.with_native_state::<bindings::State, _>(class, |s| {
        s.service
            .as_ref()
            .and_then(|shared| shared.borrow().host.as_ref().map(|h| h.staging_budget()))
    })
}

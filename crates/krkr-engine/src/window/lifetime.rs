//! Window-owned objects are invalidated in registration order. The work owns
//! the lock token, so cancellation unlocks the live window without Rust Drop
//! calling script or trying to borrow a heap.
use super::*;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, NativeTryContinuation};

pub(super) fn associate(state: &mut bindings::State, value: Value, add: bool) -> NativeResult<()> {
    // The registered class carries service state, not a native Window instance.
    if state.service.is_some() {
        return Err(NativeError::This);
    }
    let Value::Obj(value) = value else {
        return Err(NativeError::Type("an object"));
    };
    if state.invalidating.strong_count() != 0 {
        return Ok(());
    }
    let index = state
        .associated
        .iter()
        .position(|&v| matches!(v, Value::Obj(v) if v == value));
    if add && index.is_none() {
        state.associated.push(Value::Obj(value));
    } else if !add && let Some(index) = index {
        state.associated.remove(index);
    }
    Ok(())
}
struct Invalidating {
    owner: ObjId,
    index: usize,
    menu: Option<ObjId>,
    _lock: Rc<()>,
}
impl Trace for Invalidating {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.menu.trace(visit);
    }
}
impl Invalidating {
    fn next(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if let Some(menu) = self.menu.take() {
            return Ok(NativeStep::TryInvalidate {
                object: tasks::object(menu),
                continuation: self,
            });
        }
        let next = cx
            .heap_mut()
            .with_native_state::<bindings::State, _>(self.owner, |state| {
                state.associated.get(self.index).copied()
            })?;
        if let Some(object) = next {
            self.index += 1;
            Ok(NativeStep::TryInvalidate {
                object,
                continuation: self,
            })
        } else {
            cx.heap_mut()
                .with_native_state::<bindings::State, _>(self.owner, |state| {
                    state.associated.clear();
                })?;
            Ok(NativeStep::Return(Value::Void))
        }
    }
}
impl NativeTryContinuation for Invalidating {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        if let Err(error) = result {
            return crate::debug::native_error(cx, error, self);
        }
        self.next(cx)
    }
}
impl NativeContinuation for Invalidating {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.next(cx)
    }
}
pub(super) fn invalidate(cx: &mut NativeCx<'_>, owner: ObjId) -> NativeResult<NativeStep> {
    let lock = Rc::new(());
    let window = cx
        .heap_mut()
        .with_native_state::<bindings::State, _>(owner, |state| {
            state.invalidating = Rc::downgrade(&lock);
            state
                .lease
                .as_ref()
                .map(|lease| (lease.shared.clone(), lease.id))
        })?;
    let mut menu = None;
    if let Some((shared, id)) = window {
        let mut world = shared.borrow_mut();
        world.unregister(id);
        let record = world.record_mut(id)?;
        record.invalidating = Rc::downgrade(&lock);
        menu = record.menu;
        let pending = std::mem::take(&mut record.pending);
        let source = record.source;
        world.events.borrow_mut().cancel(source);
        if let Some(layers) = world.layers.upgrade() {
            let mut layers = layers.borrow_mut();
            for item in pending {
                item.cancel(&mut layers);
            }
        }
    }
    Box::new(Invalidating {
        owner,
        index: 0,
        menu,
        _lock: lock,
    })
    .next(cx)
}

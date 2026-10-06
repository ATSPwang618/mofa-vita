//! Scoped integer tokens replace foreign process pointers in getVisBuffer calls.
use super::*;
use std::sync::atomic::{AtomicI64, Ordering};
static NEXT_BUFFER: AtomicI64 = AtomicI64::new(1);
pub(super) struct Storage {
    pub samples: Vec<i16>,
    _permit: krkr_protocol::budget::Permit,
}
#[derive(Clone)]
pub struct SampleBuffer {
    token: i64,
    storage: Rc<RefCell<Storage>>,
}
impl SampleBuffer {
    pub fn token(&self) -> Value {
        Value::Int(self.token)
    }
    pub fn clear(&self) {
        self.storage.borrow_mut().samples.fill(0);
    }
    pub fn read<T>(&self, f: impl FnOnce(&[i16]) -> T) -> T {
        f(&self.storage.borrow().samples)
    }
}
pub fn validate(cx: &mut NativeCx<'_>) -> NativeResult<()> {
    let owner = cx.this();
    cx.heap_mut()
        .with_native_state::<bindings::implementation::State, _>(owner, |s| s.lease().map(|_| ()))?
}
pub fn sample_buffer(cx: &mut NativeCx<'_>, count: usize) -> NativeResult<SampleBuffer> {
    if count > 1_048_576 {
        return Err(NativeError::Message("sample buffer exceeds limit"));
    }
    let service = bindings::service(cx)?;
    let mut world = service.borrow_mut();
    let permit = world
        .backend
        .budget()
        .reserve(count * 2)
        .map_err(|e| NativeError::Detail(e.to_string()))?;
    let storage = Rc::new(RefCell::new(Storage {
        samples: vec![0; count],
        _permit: permit,
    }));
    world.sample_buffers.retain(|_, s| s.strong_count() != 0);
    let token = NEXT_BUFFER.fetch_add(1, Ordering::Relaxed);
    world.sample_buffers.insert(token, Rc::downgrade(&storage));
    Ok(SampleBuffer { token, storage })
}

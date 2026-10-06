//! System.touchImages: bounded, optional loads through the normal image cache.
//! Kirikiri2/krkrz SystemIntf.cpp and GraphicsLoaderIntf.cpp::TVPTouchImages.
use super::*;
use krkr_protocol::image_cache::{Cache, Key, MAX_ENTRIES};
use std::{collections::VecDeque, time::Duration};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, NativeTryContinuation, value};

pub(crate) fn start(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = *args.first().ok_or(NativeError::Missing(0))?;
    object_id(input)?;
    let limit = args
        .get(1)
        .map_or(Ok(0), |v| value::to_integer(cx.heap(), *v))?;
    let timeout = args
        .get(2)
        .map_or(Ok(0), |v| value::to_integer(cx.heap(), *v))? as u64;
    let class = cx
        .heap()
        .registered_class("Layer")
        .expect("installed Layer");
    let layers = cx
        .heap_mut()
        .with_native_state::<bindings::State, _>(class, |s| s.service.clone())?
        .expect("Layer service");
    let (system, cache, destination) = {
        let world = layers.borrow();
        let windows = world.windows.borrow();
        (
            windows.system.upgrade().expect("System service"),
            windows.operations.borrow().images.clone(),
            windows.graphics_window(),
        )
    };
    let maximum = cache.limit() as u64;
    let limit = if limit < 0 {
        maximum.saturating_sub(limit.unsigned_abs())
    } else if limit == 0 {
        maximum
    } else {
        maximum.min(limit as u64)
    };
    // Renderer-owned caches require a live graphics context. Preloading before
    // any window exists remains an optional hint; ordinary loads still work.
    let Some((window, owner)) = destination.filter(|_| limit != 0) else {
        return Ok(NativeStep::Return(Value::Void));
    };
    let names = cx.heap_mut().alloc_array();
    Collect {
        input,
        batch: Batch {
            layers,
            system,
            cache,
            window,
            owner,
            names,
            index: 0,
            bytes: 0,
            limit,
            timeout: Duration::from_millis(timeout),
            started: Duration::ZERO,
            touched: Rc::new(RefCell::new(VecDeque::new())),
        },
    }
    .next()
}

struct Collect {
    input: Value,
    batch: Batch,
}
impl Trace for Collect {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.input.trace(visit);
        self.batch.trace(visit);
    }
}
impl Collect {
    fn next(self) -> NativeResult<NativeStep> {
        Ok(NativeStep::GetOr {
            object: self.input,
            key: Value::Int(self.batch.index as i64),
            raw: false,
            fallback: Value::Void,
            continuation: Box::new(self),
        })
    }
}
impl NativeContinuation for Collect {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, item: Value) -> NativeResult<NativeStep> {
        if matches!(item, Value::Void) {
            self.batch.index = 0;
            self.batch.started = self.batch.system.borrow().clock.now();
            return self.batch.next(cx);
        }
        let name = value::to_string(cx.heap_mut(), item)?;
        cx.heap_mut().array_push(self.batch.names, name)?;
        self.batch.index += 1;
        self.next()
    }
}

type Touched = Rc<RefCell<VecDeque<Key>>>;
struct Batch {
    layers: Shared,
    system: crate::system::Shared,
    cache: Cache,
    window: WindowId,
    owner: ObjId,
    names: ObjId,
    index: usize,
    bytes: u64,
    limit: u64,
    timeout: Duration,
    started: Duration,
    touched: Touched,
}
impl Trace for Batch {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.names.trace(visit);
    }
}
impl Batch {
    fn next(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index >= cx.heap().array(self.names)?.len()
            || self.bytes >= self.limit
            || self.cache.limit() == 0
            || (!self.timeout.is_zero()
                && self
                    .system
                    .borrow()
                    .clock
                    .now()
                    .saturating_sub(self.started)
                    >= self.timeout)
        {
            // No reload: merely restore the original earlier-first LRU order.
            for key in self.touched.borrow().iter().rev() {
                self.cache.get(key);
            }
            return Ok(NativeStep::Return(Value::Void));
        }
        let name = cx.heap().array(self.names)?[self.index];
        self.index += 1;
        let task = One {
            layers: self.layers.clone(),
            window: self.window,
            owner: self.owner,
            name,
            touched: self.touched.clone(),
        };
        Ok(NativeStep::Try {
            task: Box::new(task),
            continuation: Box::new(self),
        })
    }
}
impl NativeTryContinuation for Batch {
    fn resume(
        mut self: Box<Self>,
        _: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        // The reference skips per-image decode, storage and allocation errors.
        // Cancellation drops the task/leases instead of entering this callback.
        if let Ok(Value::Int(bytes)) = result {
            self.bytes = self.bytes.saturating_add(bytes as u64);
        }
        Ok(NativeStep::Continue(self))
    }
}
impl NativeContinuation for Batch {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.next(cx)
    }
}
struct One {
    layers: Shared,
    window: WindowId,
    owner: ObjId,
    name: Value,
    touched: Touched,
}
impl Trace for One {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.name.trace(visit);
    }
}
impl NativeContinuation for One {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Value::Str(id) = self.name else {
            unreachable!("collected storage name")
        };
        let options = crate::storages::image::Options {
            name: tjs_core::string::c_string(cx.heap().string(id)?).to_vec(),
            key: 0x1fffffff,
            size: None,
            grayscale: false,
            budget: self.layers.borrow().host()?.staging_budget(),
        };
        crate::storages::image::request(cx, options, *self, |state, _, request| {
            let key = loading::cache_key(&request);
            let mut touched = state.touched.borrow_mut();
            // Earlier evicted entries cannot be restored by a touch. Bound
            // retained names to the cache's own maximum number of entries.
            if touched.len() == MAX_ENTRIES {
                touched.pop_front();
            }
            touched.push_back(key);
            drop(touched);
            loading::preload(&state.layers, state.window, request)
        })
    }
}

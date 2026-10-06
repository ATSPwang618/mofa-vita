use super::*;
use crate::{
    io,
    operations::{Operations, Request},
};
use krkr_protocol::{
    budget::Budget,
    input_style::CursorImage,
    window::{Command, Response},
};
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode, value};

#[derive(Default)]
pub(super) struct Cache {
    names: HashMap<Vec<u16>, i32>,
    images: HashMap<i32, Arc<CursorImage>>,
    budget: Option<Budget>,
    next: i32,
}
impl bindings::State {
    pub(super) fn load_cursor(
        &self,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        let lease = self.lease()?;
        if let Value::Str(name) = value {
            let name = tjs_core::string::c_string(cx.heap().string(name)?).to_vec();
            return crate::storages::managed::plans(
                cx,
                vec![(name, true)],
                CursorLoad {
                    shared: lease.shared.clone(),
                    id: lease.id,
                },
                |load, cx, mut plans| {
                    load.open(cx, plans.pop().flatten().expect("required cursor plan"))
                },
            );
        }
        let cursor = value::to_integer(cx.heap(), value)? as i32;
        let world = lease.shared.borrow();
        if !(-22..=1).contains(&cursor) && !world.cursors.images.contains_key(&cursor) {
            return Err(NativeError::Message("unknown cursor handle"));
        }
        drop(world);
        self.input_update(|r| r.cursor = cursor)?;
        self.notify_cursor()
    }
}
struct CursorLoad {
    shared: Shared,
    id: LayerId,
}
impl Trace for CursorLoad {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Ok(record) = self.shared.borrow().record(self.id) {
            record.owner.trace(visit);
        }
    }
}
impl CursorLoad {
    fn open(
        self,
        _cx: &mut NativeCx<'_>,
        plan: crate::storages::ReadPlan,
    ) -> NativeResult<NativeStep> {
        let name = plan.name.clone();
        let mut world = self.shared.borrow_mut();
        if let Some(&cursor) = world.cursors.names.get(&name) {
            world.record_mut(self.id)?.cursor = cursor;
            drop(world);
            let owner = self.shared.borrow().record(self.id)?.owner;
            // A cached managed plan may complete inside load_cursor while its
            // receiver state is still borrowed. Resume after the setter returns.
            return Ok(NativeStep::Continue(tjs_bind::flow::callback(
                owner,
                |owner, cx, _| {
                    cx.heap_mut()
                        .with_native_state::<bindings::State, _>(owner, |state| {
                            state.notify_cursor()
                        })?
                },
            )));
        }
        let limits = world.host()?.limits();
        if world.cursors.images.len() >= limits.cursors {
            return Err(NativeError::Message("cursor capacity reached"));
        }
        let budget = world
            .cursors
            .budget
            .get_or_insert_with(|| Budget::new(limits.cursor_bytes))
            .clone();
        let cursor = world.cursors.next.max(2);
        world.cursors.next = cursor
            .checked_add(1)
            .ok_or(NativeError::Message("cursor handle space exhausted"))?;
        let operations = world.windows.borrow().operations.clone();
        drop(world);
        let delivery = io::Delivery::default();
        Operations::wait(
            &operations,
            Request::Read(Box::new(io::Work::Cursor(plan, budget)), delivery.clone()),
            WaitMode::Internal,
            Box::new(Loaded {
                shared: self.shared.clone(),
                id: self.id,
                cursor,
                name,
                delivery,
            }),
        )
    }
}
struct Loaded {
    shared: Shared,
    id: LayerId,
    cursor: i32,
    name: Vec<u16>,
    delivery: io::Delivery,
}
impl Trace for Loaded {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(r) = self.shared.borrow().records.get(self.id) {
            r.owner.trace(visit);
        }
    }
}
impl NativeContinuation for Loaded {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Some(io::Data::Cursor(image)) = self.delivery.borrow_mut().take() else {
            return Err(NativeError::Message("unexpected cursor response"));
        };
        let image = Arc::new(image);
        let world = self.shared.borrow();
        let window = world.record(self.id)?.window;
        let host = world.host()?;
        if world.cursors.images.len() >= host.limits().cursors {
            return Err(NativeError::Message("cursor capacity reached"));
        }
        let operations = world.windows.borrow().operations.clone();
        drop(world);
        let ticket = host
            .request(
                window,
                Command::RegisterCursor {
                    id: self.cursor,
                    image: image.clone(),
                },
            )
            .map_err(NativeError::Detail)?;
        let delivery = crate::window::Delivery::default();
        Operations::wait(
            &operations,
            Request::Window(ticket, delivery.clone()),
            WaitMode::Internal,
            Box::new(Registered {
                shared: self.shared,
                id: self.id,
                cursor: self.cursor,
                name: self.name,
                image,
                delivery,
            }),
        )
    }
}
struct Registered {
    shared: Shared,
    id: LayerId,
    cursor: i32,
    name: Vec<u16>,
    image: Arc<CursorImage>,
    delivery: crate::window::Delivery,
}
impl Trace for Registered {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(r) = self.shared.borrow().records.get(self.id) {
            r.owner.trace(visit);
        }
    }
}
impl NativeContinuation for Registered {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if !matches!(
            self.delivery.borrow_mut().take(),
            Some(Response::Geometry(_) | Response::Done)
        ) {
            return Err(NativeError::Message(
                "unexpected cursor registration response",
            ));
        }
        let owner = {
            let mut world = self.shared.borrow_mut();
            let cursor = if let Some(&cursor) = world.cursors.names.get(&self.name) {
                cursor
            } else {
                if world.cursors.images.len() >= world.host()?.limits().cursors {
                    return Err(NativeError::Message("cursor capacity reached"));
                }
                world.cursors.names.insert(self.name, self.cursor);
                world.cursors.images.insert(self.cursor, self.image);
                self.cursor
            };
            let r = world.record_mut(self.id)?;
            r.cursor = cursor;
            r.owner
        };
        cx.heap_mut()
            .with_native_state::<bindings::State, _>(owner, |state| state.notify_cursor())?
    }
}

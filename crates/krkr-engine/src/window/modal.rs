//! Window modality owns an EventWait, not a nested native message loop.
use super::*;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, WaitMode};

pub(super) struct Entry {
    id: WindowId,
    active: Arc<AtomicBool>,
}

impl Windows {
    pub(crate) fn accepts_input(&self, id: WindowId) -> bool {
        self.modals
            .last()
            .is_none_or(|modal| modal.id == id && modal.active.load(Ordering::Acquire))
    }

    pub(super) fn finish_modal(&mut self, id: WindowId) -> bool {
        let Some(modal) = self.modals.iter().find(|modal| modal.id == id) else {
            return false;
        };
        modal.active.store(false, Ordering::Release);
        if let Some(record) = self.records.get_mut(id) {
            record.visible = false;
        }
        if let Some(host) = &self.host {
            host.wake_host();
        }
        true
    }

    fn clear_modal_input(&mut self) {
        if let Some(host) = &self.host {
            host.clear_user_input();
        }
        for record in self.records.values_mut() {
            for pending in &mut record.pending {
                if matches!(pending, Pending::Input(input) if input.is_user_input()) {
                    *pending = Pending::Suppressed;
                }
            }
            record.hint.deadline = None;
        }
        if let Some(layers) = self.layers.upgrade() {
            layers.borrow_mut().release_window_captures();
        }
    }
}

pub(super) fn show(lease: &Lease) -> NativeResult<NativeStep> {
    let (host, operations, owner, active) = {
        let mut world = lease.shared.borrow_mut();
        let record = world.record(lease.id)?;
        if world
            .system
            .upgrade()
            .is_some_and(|system| system.borrow().event_disabled)
        {
            return Err(NativeError::Message(
                "cannot show a modal window while script events are disabled",
            ));
        }
        if !world.is_live(lease.id) {
            return Err(NativeError::Message("window is closing"));
        }
        if record.visible || world.modals.iter().any(|modal| modal.id == lease.id) {
            return Err(NativeError::Message(
                "cannot show an already visible or modal window",
            ));
        }
        if record.full_screen.is_some() {
            return Err(NativeError::Message(
                "cannot show a fullscreen window as modal",
            ));
        }
        if world.count() < 2 {
            return Err(NativeError::Message(
                "a modal window requires another window",
            ));
        }
        let host = world.host.clone().ok_or(NativeError::Message(
            "Window requires a platform window host",
        ))?;
        let owner = record.owner;
        let active = Arc::new(AtomicBool::new(true));
        world.clear_modal_input();
        world.modals.push(Entry {
            id: lease.id,
            active: active.clone(),
        });
        world.record_mut(lease.id)?.visible = true;
        (host, world.operations.clone(), owner, active)
    };
    let delivery = Delivery::default();
    let scope = Box::new(Scope {
        shared: lease.shared.clone(),
        id: lease.id,
        owner,
        active,
        delivery: delivery.clone(),
    });
    let ticket = host
        .request(
            lease.id,
            Command::ShowModal {
                active: Arc::downgrade(&scope.active),
            },
        )
        .map_err(NativeError::Detail)?;
    // The host holds this one bounded request until dismissal. Nested callbacks
    // use the existing runtime/context stack, without polling timers or threads.
    operations::Operations::wait(
        &operations,
        operations::Request::Modal(ticket, delivery),
        WaitMode::Event,
        scope,
    )
}

struct Scope {
    shared: Shared,
    id: WindowId,
    owner: ObjId,
    active: Arc<AtomicBool>,
    delivery: Delivery,
}
impl Trace for Scope {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
impl NativeContinuation for Scope {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let response = self.delivery.borrow_mut().take().expect("modal completion");
        let krkr_protocol::window::Response::Geometry(geometry) = response else {
            return Err(NativeError::Message("unexpected modal window response"));
        };
        if let Some(record) = self.shared.borrow_mut().records.get_mut(self.id) {
            record.geometry = geometry;
        }
        Ok(NativeStep::Return(Value::Void))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        let mut world = self.shared.borrow_mut();
        world
            .modals
            .retain(|modal| !Arc::ptr_eq(&modal.active, &self.active));
        if let Some(record) = world.records.get_mut(self.id) {
            record.visible = false;
        }
        world.clear_modal_input();
        if let Some(host) = &world.host {
            host.wake_host();
        }
    }
}

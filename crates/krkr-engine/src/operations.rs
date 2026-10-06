//! Engine-owned operation identities. The continuation owns a lease, so normal
//! return, exceptions, VM cancellation and engine teardown all release the job.
use crate::io::{Delivery, Work, Worker};
use slotmap::{Key, KeyData, SlotMap, new_key_type};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, WaitMode,
    WaitRequest,
};
use tjs_runtime::WaitId;

new_key_type! { pub(crate) struct OperationId; }
pub(crate) type Shared = Rc<RefCell<Operations>>;
pub(crate) enum Request {
    Delay(Duration),
    Read(Box<Work>, Delivery),
    Exit(i32),
    Compact(i32),
    Window(krkr_protocol::window::Ticket, crate::window::Delivery),
    Modal(krkr_protocol::window::Ticket, crate::window::Delivery),
}
struct Entry {
    request: Option<Request>,
    wait: Option<WaitId>,
    cancelled: Arc<AtomicBool>,
    delivery: Option<Delivery>,
    window: Option<(krkr_protocol::window::Ticket, crate::window::Delivery)>,
}
pub(crate) struct Operations {
    pub images: krkr_protocol::image_cache::Cache,
    entries: SlotMap<OperationId, Entry>,
    limit: usize,
    worker: Option<Worker>,
    waker: Arc<dyn Fn() + Send + Sync>,
}
impl Operations {
    pub fn new(limit: usize) -> Shared {
        let thread = std::thread::current();
        Rc::new(RefCell::new(Self {
            images: krkr_protocol::image_cache::Cache::new(32 * 1024 * 1024),
            entries: SlotMap::with_key(),
            limit,
            worker: None,
            waker: Arc::new(move || thread.unpark()),
        }))
    }
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) -> NativeResult<()> {
        if self.worker.is_some() {
            return Err(NativeError::Message(
                "set the engine waker before starting IO",
            ));
        }
        self.waker = waker;
        Ok(())
    }
    pub fn waker(&self) -> Arc<dyn Fn() + Send + Sync> {
        self.waker.clone()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn pending_host(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| {
                entry.delivery.is_some()
                    || entry.window.is_some()
                    || matches!(
                        entry.request,
                        Some(Request::Read(..) | Request::Window(..) | Request::Modal(..))
                    )
            })
            .count()
    }
    pub fn wait(
        shared: &Shared,
        request: Request,
        mode: WaitMode,
        inner: Box<dyn NativeContinuation>,
    ) -> NativeResult<NativeStep> {
        // A quick host command may have finished before we install its wait.
        // Continue under the VM work budget without an artificial scheduler
        // boundary. Event waits must still yield even when already complete.
        if mode == WaitMode::Internal
            && let Request::Window(ticket, delivery) = &request
            && let Some(result) = ticket.take()
        {
            *delivery.borrow_mut() = Some(result.map_err(NativeError::Detail)?);
            return Ok(NativeStep::Continue(inner));
        }
        let mut operations = shared.borrow_mut();
        if operations.entries.len() >= operations.limit {
            return Err(NativeError::Message("engine operation capacity reached"));
        }
        let id = operations.entries.insert(Entry {
            request: Some(request),
            wait: None,
            cancelled: Arc::new(AtomicBool::new(false)),
            delivery: None,
            window: None,
        });
        Ok(NativeStep::Wait {
            request: WaitRequest {
                token: id.data().as_ffi(),
                mode,
            },
            continuation: Box::new(Lease {
                shared: shared.clone(),
                id,
                inner: Some(inner),
            }),
        })
    }
    pub fn bind(&mut self, token: u64, wait: WaitId) -> Option<Request> {
        let entry = self
            .entries
            .get_mut(OperationId::from(KeyData::from_ffi(token)))?;
        // A repeated Waiting notification must neither resubmit nor rearm work.
        if entry.wait.is_some() {
            return None;
        }
        entry.wait = Some(wait);
        entry.request.take()
    }
    pub fn contains(&self, token: u64) -> bool {
        self.entries
            .contains_key(OperationId::from(KeyData::from_ffi(token)))
    }
    pub fn read(&mut self, token: u64, read: Box<Work>, delivery: Delivery) -> NativeResult<()> {
        let id = OperationId::from(KeyData::from_ffi(token));
        self.entries[id].delivery = Some(delivery);
        let cancelled = self.entries[id].cancelled.clone();
        if self.worker.is_none() {
            self.worker = Some(Worker::new(self.limit, self.waker.clone())?);
        }
        self.worker
            .as_ref()
            .expect("started IO worker")
            .submit(id, cancelled, read)
    }
    pub fn completion(&mut self) -> Option<(Option<WaitId>, NativeResult<Value>)> {
        for entry in self.entries.values_mut() {
            if let Some((ticket, delivery)) = &entry.window
                && let Some(result) = ticket.take()
            {
                let result = result
                    .map(|geometry| {
                        *delivery.borrow_mut() = Some(geometry);
                        Value::Void
                    })
                    .map_err(NativeError::Detail);
                entry.window = None;
                return Some((entry.wait, result));
            }
        }
        let (id, result) = match self.worker.as_ref()?.completion() {
            Ok(completion) => completion?,
            Err(error) => {
                // A disconnected worker cannot ever complete its pending reads.
                // Fail each one once so its continuation can unwind normally.
                let entry = self
                    .entries
                    .values_mut()
                    .find(|entry| entry.delivery.is_some())?;
                entry.delivery = None;
                return Some((entry.wait, Err(error)));
            }
        };
        let Some(entry) = self.entries.get(id) else {
            return Some((None, Ok(Value::Void)));
        };
        let result = result.map(|data| {
            *entry.delivery.as_ref().expect("IO delivery").borrow_mut() = Some(data);
            Value::Void
        });
        Some((entry.wait, result))
    }
    pub fn window(
        &mut self,
        token: u64,
        ticket: krkr_protocol::window::Ticket,
        delivery: crate::window::Delivery,
    ) {
        self.entries[OperationId::from(KeyData::from_ffi(token))].window = Some((ticket, delivery));
    }
}
struct Lease {
    shared: Shared,
    id: OperationId,
    inner: Option<Box<dyn NativeContinuation>>,
}
impl Trace for Lease {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        if let Some(inner) = &self.inner {
            inner.trace(visit);
        }
    }
}
impl NativeContinuation for Lease {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        // Release the completed slot before resuming: a continuation may issue
        // its next IO immediately even when the host permits just one context.
        if let Some(entry) = self.shared.borrow_mut().entries.remove(self.id) {
            entry.cancelled.store(true, Ordering::Relaxed);
        }
        self.inner
            .take()
            .expect("owned continuation")
            .resume(cx, value)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(entry) = self.shared.borrow_mut().entries.remove(self.id) {
            entry.cancelled.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Crash;
    impl crate::extensions::worker::Job for Crash {
        fn run(self: Box<Self>, _: &AtomicBool) -> NativeResult<Box<dyn std::any::Any + Send>> {
            panic!("injected IO worker failure");
        }
    }

    #[test]
    fn worker_disconnect_fails_pending_reads_instead_of_waiting_forever() {
        let shared = Operations::new(4);
        let mut operations = shared.borrow_mut();
        let mut waits = SlotMap::<WaitId, ()>::with_key();
        let wait = waits.insert(());
        let id = operations.entries.insert(Entry {
            request: None,
            wait: Some(wait),
            cancelled: Arc::new(AtomicBool::new(false)),
            delivery: None,
            window: None,
        });
        operations
            .read(
                id.data().as_ffi(),
                Box::new(Work::Extension(Box::new(Crash))),
                Delivery::default(),
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let completion = loop {
            if let Some(completion) = operations.completion() {
                break Some(completion);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        let (owner, result) = completion.expect("disconnected IO left a native wait pending");
        assert_eq!(owner, Some(wait));
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("engine IO worker stopped")
        );
        assert!(
            operations.completion().is_none(),
            "failure delivered more than once"
        );
    }
}

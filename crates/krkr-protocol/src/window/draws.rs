//! Lock order is requests -> draws -> scenes. Any observable operation closes
//! the grant before entering the ordered queue; scene fences include its writes.
use super::*;
use crate::graphics::{Command as GraphicsCommand, PreparedDraws};

pub(super) struct Grant {
    window: WindowId,
    pub(super) batch: PreparedDraws,
    sequence: u64,
    reply: Arc<Reply>,
}
impl Shared {
    pub(super) fn flush_draws(&self, requests: &mut VecDeque<Request>) {
        let Some(grant) = self.draws.lock().unwrap().take() else {
            return;
        };
        if !grant.batch.draws().is_empty() {
            requests.push_back(Request {
                window: grant.window,
                command: Command::Graphics(GraphicsCommand::PreparedDraw(grant.batch)),
                sequence: grant.sequence,
                reply: grant.reply,
            });
        }
    }
}
impl Request {
    /// The host has executed this request, detached the writable main plane,
    /// and reserved every batch slot. A later request invalidates this offer.
    pub fn offer_draws(&self, batch: PreparedDraws) {
        let shared = &self.reply.shared;
        let mut requests = shared.requests.lock().unwrap();
        let mut grant = shared.draws.lock().unwrap();
        if self.cancelled()
            || !shared.connected.load(Ordering::Acquire)
            || batch.payload_bytes() > shared.limits.graphics_bytes
            || shared.sequence.load(Ordering::Relaxed) != self.sequence
            || !requests.is_empty()
            || grant.is_some()
            || !shared.scenes.lock().unwrap().is_empty()
        {
            return;
        }
        if requests.try_reserve(1).is_err() {
            return;
        }
        if shared
            .active
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                (count < shared.limits.operations).then(|| count + 1)
            })
            .is_err()
        {
            return;
        }
        *grant = Some(Grant {
            window: self.window,
            batch,
            sequence: self.sequence,
            reply: Arc::new(Reply {
                shared: shared.clone(),
                active: AtomicBool::new(true),
                result: Mutex::new(None),
                admitted: true,
            }),
        });
    }
}
impl Client {
    pub fn try_draw(&self, window: WindowId, command: &GraphicsCommand) -> Result<bool, String> {
        if !matches!(
            command,
            GraphicsCommand::Fill { .. }
                | GraphicsCommand::Color { .. }
                | GraphicsCommand::Copy { .. }
                | GraphicsCommand::Operate { .. }
        ) {
            return Ok(false);
        }
        let mut requests = self.0.requests.lock().unwrap();
        if !self.0.connected.load(Ordering::Acquire) {
            return Err("window host disconnected".into());
        }
        let mut grant = self.0.draws.lock().unwrap();
        let Some(batch) = grant.as_mut().filter(|g| g.window == window) else {
            return Ok(false);
        };
        if !batch.batch.push(command) {
            return Ok(false);
        }
        command.invalidate_snapshots();
        batch.sequence = self.0.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let full = batch.batch.full();
        drop(grant);
        if full {
            self.0.flush_draws(&mut requests);
        }
        drop(requests);
        if full {
            self.wake_host();
        }
        Ok(true)
    }
}

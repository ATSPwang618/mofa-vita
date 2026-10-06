//! Hint timing and stationary-pointer rechecks share the engine's clock and FIFO.
use super::*;
use std::time::Duration;
use tjs_core::{NativeContinuation, NativeCx, NativeStep, ObjRef};

pub(super) enum Pending {
    // Keep the dispatch token when modal entry discards an old input payload.
    Suppressed,
    Clipboard(std::rc::Weak<()>),
    ClipboardError(String),
    Transition(crate::layer::transition::Id),
    Paint(Vec<krkr_protocol::graphics::LayerId>),
    Input(Input),
    Hint {
        text: Arc<[u16]>,
        position: (i32, i32),
    },
    Recheck,
}
impl Pending {
    pub(super) fn cancel(&self, layers: &mut crate::layer::Layers) {
        match self {
            Self::Transition(id) => layers.cancel_transition_tick(*id),
            Self::Paint(ids) => {
                for &id in ids {
                    layers.cancel_paint(id);
                }
            }
            _ => {}
        }
    }
}
pub(super) struct Hints {
    pub delay: i32,
    text: Arc<[u16]>,
    // Identity only, like the reference's LastHintSender; never dereferenced.
    sender: Option<ObjId>,
    pub(super) deadline: Option<Duration>,
}
impl Default for Hints {
    fn default() -> Self {
        Self {
            delay: 500,
            text: Arc::from([]),
            sender: None,
            deadline: None,
        }
    }
}
impl Windows {
    pub(crate) fn now(&self) -> Duration {
        self.clock.now()
    }
    pub(crate) fn post_transition(
        &mut self,
        window: WindowId,
        transition: crate::layer::transition::Id,
    ) -> NativeResult<()> {
        self.post_layer(window, Pending::Transition(transition))
    }
    pub(crate) fn post_paint(
        &mut self,
        window: WindowId,
        layers: Vec<krkr_protocol::graphics::LayerId>,
    ) -> NativeResult<()> {
        self.post_layer(window, Pending::Paint(layers))
    }
    fn post_layer(&mut self, window: WindowId, pending: Pending) -> NativeResult<()> {
        let record = self
            .records
            .get_mut(window)
            .ok_or(NativeError::Message("window is invalid"))?;
        if record.invalidating.strong_count() != 0 {
            return Err(NativeError::Message("window is shutting down"));
        }
        self.events.borrow_mut().post(record.source, 1, 0)?;
        record.pending.push_back(pending);
        Ok(())
    }
    pub(crate) fn set_hint(
        &mut self,
        id: WindowId,
        sender: Option<ObjId>,
        text: Arc<[u16]>,
    ) -> NativeResult<()> {
        let now = self.clock.now();
        let Some(record) = self.records.get_mut(id) else {
            return Ok(());
        };
        let hint = &mut record.hint;
        let changed = hint.text != text;
        let restart = changed || hint.sender != sender;
        let hide = text.is_empty() || changed;
        let show = !text.is_empty() && restart && hint.delay == 0;
        // Reserve both notifications before changing state, so a full queue
        // cannot leave only half of an immediate hide/show pair committed.
        let count = usize::from(hide) + usize::from(show);
        if count != 0 {
            self.events.borrow_mut().post(record.source, count, 0)?;
        }
        if hide {
            record.pending.push_back(Pending::Hint {
                text: Arc::from([]),
                position: record.cursor,
            });
        }
        if show {
            record.pending.push_back(Pending::Hint {
                text: text.clone(),
                position: record.cursor,
            });
        }
        if text.is_empty() || restart {
            hint.deadline = (!text.is_empty() && hint.delay > 0)
                .then(|| now.saturating_add(Duration::from_millis(hint.delay as u64)));
        }
        hint.text = text;
        hint.sender = sender;
        Ok(())
    }
    pub(super) fn advance_input(&mut self) {
        let now = self.clock.now();
        let mut events = self.events.borrow_mut();
        for record in self
            .records
            .values_mut()
            .filter(|r| r.invalidating.strong_count() == 0)
        {
            if record.hint.deadline.is_some_and(|deadline| deadline <= now) {
                if events.post(record.source, 1, 0).is_err() {
                    break;
                }
                record.pending.push_back(Pending::Hint {
                    text: record.hint.text.clone(),
                    position: record.cursor,
                });
                record.hint.deadline = None;
            }
            if record.recheck.is_some_and(|deadline| deadline <= now) && !record.recheck_queued {
                if events.post(record.source, 1, 0).is_err() {
                    break;
                }
                record.pending.push_back(Pending::Recheck);
                record.recheck_queued = true;
                record.recheck = Some(now.saturating_add(Duration::from_secs(1)));
            }
        }
    }
    pub(crate) fn sleep_duration(&self) -> Option<Duration> {
        let now = self.clock.now();
        self.records
            .values()
            .filter(|r| r.invalidating.strong_count() == 0)
            .flat_map(|r| [r.hint.deadline, r.recheck.filter(|_| !r.recheck_queued)])
            .flatten()
            .min()
            .map(|deadline| deadline.saturating_sub(now))
    }
}
pub(super) fn hint_callback(
    owner: ObjId,
    key: Value,
    text: Arc<[u16]>,
    position: (i32, i32),
    heap: &mut Heap,
) -> Box<dyn NativeContinuation> {
    let show = !text.is_empty();
    Box::new(HintCallback {
        owner,
        key,
        arguments: vec![
            Value::Str(heap.alloc_string(text.to_vec())),
            Value::Int(position.0.into()),
            Value::Int(position.1.into()),
            Value::Int(show.into()),
        ],
        fetched: false,
    })
}
struct HintCallback {
    owner: ObjId,
    key: Value,
    arguments: Vec<Value>,
    fetched: bool,
}
impl Trace for HintCallback {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.key.trace(visit);
        self.arguments.trace(visit);
    }
}
impl NativeContinuation for HintCallback {
    fn resume(
        mut self: Box<Self>,
        _: &mut NativeCx<'_>,
        mut value: Value,
    ) -> NativeResult<NativeStep> {
        if !self.fetched {
            self.fetched = true;
            return Ok(NativeStep::GetOptional {
                object: Value::Obj(ObjRef::bound(self.owner)),
                key: self.key,
                continuation: self,
            });
        }
        let Value::Obj(ref mut reference) = value else {
            return Ok(NativeStep::Return(Value::Void));
        };
        if reference.object.is_none() {
            return Ok(NativeStep::Return(Value::Void));
        }
        if reference.this.is_none() {
            reference.this = Some(self.owner);
        }
        Ok(NativeStep::Call {
            function: value,
            arguments: self.arguments,
            continuation: Box::new(tasks::Returned),
        })
    }
}

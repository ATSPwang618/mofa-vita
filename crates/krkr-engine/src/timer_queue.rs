use crate::events::{self, Kind, SourceId};
use slotmap::{SlotMap, new_key_type};
use std::{collections::BTreeSet, rc::Rc, time::Duration};
use tjs_core::{NativeError, NativeResult, ObjId};
use tjs_runtime::clock::Clock;

new_key_type! { pub(crate) struct TimerId; }

#[derive(Clone, Copy)]
pub struct TimerLimits {
    pub max_timers: usize,
    /// Shared capacity for Timer and AsyncTrigger queues, including reserved
    /// timer backlog and uncached trigger events.
    pub max_pending_events: usize,
}
impl Default for TimerLimits {
    fn default() -> Self {
        Self {
            max_timers: 1024,
            max_pending_events: 65536,
        }
    }
}

pub(crate) struct Record {
    source: SourceId,
    pub interval: u64,
    pub enabled: bool,
    pub capacity: i32,
    pub mode: i32,
    deadline: Option<u64>,
}
pub(crate) struct Timers {
    clock: Rc<dyn Clock>,
    records: SlotMap<TimerId, Record>,
    deadlines: BTreeSet<(u64, TimerId)>,
    events: events::Shared,
    limits: TimerLimits,
}
fn capacity(value: i32) -> usize {
    if value == 0 {
        65535
    } else {
        value.max(0) as usize
    }
}
fn ticks(time: Duration) -> u64 {
    (time.as_nanos() * 65536 / 1_000_000).min(u128::from(u64::MAX)) as u64
}
fn duration(ticks: u64) -> Duration {
    Duration::from_secs(ticks / 65_536_000)
        + Duration::from_nanos(((ticks % 65_536_000) * 1_000_000).div_ceil(65536))
}
impl Timers {
    pub fn new(clock: Rc<dyn Clock>, limits: TimerLimits, events: events::Shared) -> Self {
        Self {
            clock,
            limits,
            records: SlotMap::with_key(),
            deadlines: BTreeSet::new(),
            events,
        }
    }
    pub fn insert(&mut self, owner: ObjId) -> NativeResult<TimerId> {
        if self.records.len() >= self.limits.max_timers {
            return Err(NativeError::Message(
                "Timer capacity exceeds host event budget",
            ));
        }
        let source = self.events.borrow_mut().insert(owner, Kind::Timer, 6)?;
        Ok(self.records.insert(Record {
            source,
            interval: 1000,
            enabled: false,
            capacity: 6,
            mode: 0,
            deadline: None,
        }))
    }
    pub fn get(&self, id: TimerId) -> &Record {
        &self.records[id]
    }
    pub fn remove(&mut self, id: TimerId) {
        if let Some(record) = self.records.remove(id) {
            if let Some(deadline) = record.deadline {
                self.deadlines.remove(&(deadline, id));
            }
            self.events.borrow_mut().remove(record.source);
        }
    }
    fn cancel_pending(&mut self, id: TimerId) {
        self.events.borrow_mut().cancel(self.records[id].source);
    }
    fn schedule(&mut self, id: TimerId) {
        let now = ticks(self.clock.now());
        let record = &mut self.records[id];
        if let Some(deadline) = record.deadline.take() {
            self.deadlines.remove(&(deadline, id));
        }
        if record.enabled
            && record.interval != 0
            && let Some(deadline) = now.checked_add(record.interval)
        {
            record.deadline = Some(deadline);
            self.deadlines.insert((deadline, id));
        }
    }
    pub fn enable(&mut self, id: TimerId, enabled: bool) {
        self.records[id].enabled = enabled;
        if !enabled {
            self.cancel_pending(id);
        }
        self.schedule(id);
    }
    pub fn interval(&mut self, id: TimerId, interval: u64) {
        self.records[id].interval = interval;
        if self.records[id].enabled {
            self.cancel_pending(id);
        }
        self.schedule(id);
    }
    pub fn set_capacity(&mut self, id: TimerId, value: i32) -> NativeResult<()> {
        let record = &mut self.records[id];
        self.events
            .borrow_mut()
            .capacity(record.source, capacity(value))?;
        record.capacity = value;
        Ok(())
    }
    pub fn mode(&mut self, id: TimerId, mode: i32) {
        self.records[id].mode = mode;
    }
    pub fn advance(&mut self, limit: usize) {
        let now = ticks(self.clock.now());
        for _ in 0..limit {
            let Some(&(deadline, id)) = self.deadlines.first() else {
                break;
            };
            if deadline >= now {
                break;
            }
            self.deadlines.remove(&(deadline, id));
            let record = &mut self.records[id];
            let elapsed = (now - deadline) / record.interval + 1;
            // Reference timers collapse an excessively late batch to one event.
            let count = if elapsed > 40 { 1 } else { elapsed };
            let next = if elapsed > 40 {
                now.checked_add(record.interval)
            } else {
                record
                    .interval
                    .checked_mul(count)
                    .and_then(|span| deadline.checked_add(span))
            };
            record.deadline = next;
            if let Some(next) = next {
                self.deadlines.insert((next, id));
            }
            let mut events = self.events.borrow_mut();
            let count = (count as usize)
                .min(capacity(record.capacity).saturating_sub(events.pending(record.source)));
            events
                .post(record.source, count, record.mode)
                .expect("reserved timer event capacity");
        }
    }
    pub fn sleep_duration(&self) -> Option<Duration> {
        self.deadlines.first().map(|&(deadline, _)| {
            // The original trigger condition is strictly later than NextTick.
            duration(deadline.saturating_add(1)).saturating_sub(self.clock.now())
        })
    }
    pub fn clear(&mut self) {
        self.deadlines.clear();
        for record in self.records.values_mut() {
            record.enabled = false;
            record.deadline = None;
            self.events.borrow_mut().cancel(record.source);
        }
    }
}

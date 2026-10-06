//! Deadline driving for owned native waits. This layer never sleeps or runs
//! callbacks outside Scheduler's event-entry rules.
use crate::{CancelledContext, ContextId, Runtime, Scheduler, SchedulerEvent, WaitId};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value, Vm};

pub trait Clock {
    fn now(&self) -> Duration;
}

impl<T: Clock + ?Sized> Clock for std::rc::Rc<T> {
    fn now(&self) -> Duration {
        (**self).now()
    }
}

pub struct MonotonicClock(Instant);
impl Default for MonotonicClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}
impl Clock for MonotonicClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

pub struct TimedScheduler<C> {
    scheduler: Scheduler,
    clock: C,
    deadlines: BTreeSet<(Duration, WaitId)>,
    by_wait: BTreeMap<WaitId, Duration>,
}
impl<C: Clock> TimedScheduler<C> {
    pub fn new(scheduler: Scheduler, clock: C) -> Self {
        Self {
            scheduler,
            clock,
            deadlines: BTreeSet::new(),
            by_wait: BTreeMap::new(),
        }
    }
    pub fn runtime(&self) -> &Runtime {
        &self.scheduler.runtime
    }
    pub fn runtime_mut(&mut self) -> &mut Runtime {
        &mut self.scheduler.runtime
    }
    pub fn can_enter_event(&self) -> bool {
        // Deliver due completions before admitting another event. The resumed
        // script may cancel it, so it must remain in its owner's queue for now.
        self.scheduler.can_enter_event()
            && self
                .deadlines
                .first()
                .is_none_or(|&(deadline, _)| deadline > self.clock.now())
    }
    pub fn work_executed(&self) -> u64 {
        self.scheduler.work_executed()
    }
    pub fn instructions_executed(&self) -> u64 {
        self.scheduler.instructions_executed()
    }
    pub fn active_diagnostic(&self) -> Option<tjs_core::Diagnostic> {
        self.scheduler.active_diagnostic()
    }
    pub fn now(&self) -> Duration {
        self.clock.now()
    }
    pub fn enqueue(&mut self, vm: Vm) -> Result<ContextId, Box<Vm>> {
        self.scheduler.enqueue(vm)
    }
    pub fn enqueue_callback(
        &mut self,
        global: tjs_core::ObjId,
        callback: tjs_core::Callback,
        arguments: Vec<Value>,
    ) -> Result<ContextId, Box<Vm>> {
        self.scheduler.enqueue_callback(global, callback, arguments)
    }
    pub fn enqueue_task(
        &mut self,
        global: tjs_core::ObjId,
        task: Box<dyn tjs_core::NativeContinuation>,
    ) -> Result<ContextId, Box<Vm>> {
        self.scheduler.enqueue_task(global, task)
    }
    pub fn take_result(&mut self, id: ContextId) -> Option<crate::RuntimeExit> {
        self.scheduler.take_result(id)
    }
    pub fn resume_completed(&mut self, id: ContextId, vm: Vm) -> Result<(), Box<Vm>> {
        self.scheduler.resume_completed(id, vm)
    }
    pub fn original_result(&self, id: ContextId) -> Option<&crate::RuntimeExit> {
        self.scheduler.original_result(id)
    }
    pub fn resolve_completion(&mut self, id: ContextId, result: crate::RuntimeExit) -> bool {
        self.scheduler.resolve_completion(id, result)
    }
    pub fn collect(&mut self, roots: impl IntoIterator<Item = Value>) -> tjs_core::CollectionStats {
        self.scheduler.collect(roots)
    }

    pub fn collect_step(
        &mut self,
        roots: impl IntoIterator<Item = Value>,
        budget: usize,
    ) -> tjs_core::CollectionStep {
        self.scheduler.collect_step(roots, budget)
    }

    pub fn collect_auto(
        &mut self,
        roots: impl IntoIterator<Item = Value>,
    ) -> Option<tjs_core::CollectionStats> {
        self.scheduler.collect_auto(roots)
    }

    /// Register once, using an absolute monotonic deadline. Repeated Waiting
    /// notifications must not restart the duration. Only live waits are admitted,
    /// so timer records are bounded by Scheduler's context capacity.
    pub fn arm(&mut self, wait: WaitId, deadline: Duration) -> bool {
        if !self.scheduler.is_wait_pending(wait) || self.by_wait.contains_key(&wait) {
            return false;
        }
        self.by_wait.insert(wait, deadline);
        self.deadlines.insert((deadline, wait));
        true
    }
    pub fn complete(&mut self, wait: WaitId, result: Result<Value, tjs_core::NativeError>) -> bool {
        self.disarm(wait);
        self.scheduler.complete(wait, result)
    }
    fn disarm(&mut self, wait: WaitId) {
        if let Some(deadline) = self.by_wait.remove(&wait) {
            self.deadlines.remove(&(deadline, wait));
        }
    }
    pub fn cancel(&mut self, id: ContextId) -> Vec<CancelledContext> {
        let cancelled = self.scheduler.cancel(id);
        for entry in &cancelled {
            if let Some((wait, _)) = entry.wait {
                self.disarm(wait);
            }
        }
        cancelled
    }
    pub fn cancel_all(&mut self) -> Vec<CancelledContext> {
        self.deadlines.clear();
        self.by_wait.clear();
        self.scheduler.cancel_all()
    }
    pub fn poll(&mut self, budget: RunBudget, completions: NonZeroUsize) -> SchedulerEvent {
        let now = self.clock.now();
        for _ in 0..completions.get() {
            let Some(&(deadline, wait)) = self.deadlines.first() else {
                break;
            };
            if deadline > now {
                break;
            }
            self.disarm(wait);
            self.scheduler.complete(wait, Ok(Value::Void));
        }
        self.scheduler.poll(budget)
    }
    /// Re-read time immediately before the host waits. None means no scheduled
    /// deadline (wait for external input); zero means keep driving runnable work.
    pub fn sleep_duration(&self) -> Option<Duration> {
        if self.scheduler.has_runnable() {
            return Some(Duration::ZERO);
        }
        self.deadlines
            .first()
            .map(|&(deadline, _)| deadline.saturating_sub(self.clock.now()))
    }
}

mod constants;
mod dispatch;
mod exceptions;
use crate::{
    events::{self, Event, Kind},
    operations::{self, Operations, Request},
    system::{self, SystemConfig},
    timer,
    timer_queue::{TimerLimits, Timers},
};
use std::{cell::RefCell, collections::HashMap, num::NonZeroUsize, rc::Rc, time::Duration};
use tjs_core::{Callback, Module, NativeResult, ObjId, ObjRef, RunBudget, Value, Vm, WaitRequest};
use tjs_runtime::clock::{Clock, TimedScheduler};
use tjs_runtime::{
    CancelledContext, ContextId, Runtime, RuntimeExit, Scheduler, SchedulerEvent, SchedulerLimits,
    WaitId,
};

#[derive(Debug)]
pub enum EngineEvent {
    Video {
        context: ContextId,
        owner: ObjId,
        result: RuntimeExit,
    },
    Idle,
    Yielded,
    Waiting {
        context: ContextId,
        wait: WaitId,
        request: WaitRequest,
    },
    Completed {
        context: ContextId,
        result: RuntimeExit,
    },
    Timer {
        context: ContextId,
        owner: ObjId,
        result: RuntimeExit,
    },
    AsyncTrigger {
        context: ContextId,
        owner: ObjId,
        result: RuntimeExit,
    },
    Window {
        context: ContextId,
        owner: ObjId,
        result: RuntimeExit,
    },
    Sound {
        context: ContextId,
        owner: ObjId,
        result: RuntimeExit,
    },
    System {
        context: ContextId,
        event: SystemEvent,
        result: RuntimeExit,
    },
    Terminated(i32),
}

#[derive(Clone, Copy, Debug)]
pub enum SystemEvent {
    Continuous,
    Activate,
    Deactivate,
}

/// Single-threaded world/event owner. Platform loops supply a monotonic clock
/// and decide how to wait for input; no platform window belongs to this type.
pub struct Engine<C> {
    driver: TimedScheduler<Rc<C>>,
    timers: timer::Shared,
    events: events::Shared,
    callbacks: HashMap<ContextId, CallbackState>,
    global: ObjId,
    timer_name: Value,
    fire_name: Value,
    system: system::Shared,
    operations: operations::Shared,
    windows: crate::window::Shared,
    layers: crate::layer::Shared,
    fonts: crate::font::Shared,
    sounds: crate::sound::Shared,
    videos: crate::video::Shared,
    terminated: Option<i32>,
    startup: Startup,
    exiting_context: Option<ContextId>,
    completion_budget_exhausted: bool,
    presentation_deferred: bool,
    collection_threshold: usize,
    last_collection: Duration,
    next_media_poll: Duration,
}
#[derive(Default)]
enum Startup {
    #[default]
    NotStarted,
    Running(ContextId),
    Completed,
    Cancelled,
}
struct CallbackState {
    kind: CallbackKind,
    finished: bool,
    modal_wait: bool,
}
enum CallbackKind {
    Posted(Event),
    Continuous(Value),
    Activation(bool),
}
impl CallbackKind {
    fn root(&self) -> Option<Value> {
        match *self {
            Self::Posted(event) => Some(Value::Obj(event.owner.into())),
            Self::Continuous(function) => Some(function),
            Self::Activation(_) => None,
        }
    }
}
impl<C: Clock + 'static> Engine<C> {
    pub fn new(
        runtime: Runtime,
        clock: C,
        scheduler: SchedulerLimits,
        limits: TimerLimits,
    ) -> NativeResult<Self> {
        Self::with_system(
            runtime,
            clock,
            scheduler,
            limits,
            SystemConfig::for_process()?,
        )
    }
    pub fn with_system(
        mut runtime: Runtime,
        clock: C,
        scheduler: SchedulerLimits,
        limits: TimerLimits,
        config: SystemConfig,
    ) -> NativeResult<Self> {
        crate::configure_preprocessor(&mut runtime.preprocessor);
        let clock = Rc::new(clock);
        crate::plugins::install(&mut runtime.heap)?;
        let events = Rc::new(RefCell::new(events::Events::new(limits.max_pending_events)));
        let timers = Rc::new(RefCell::new(Timers::new(
            clock.clone(),
            limits,
            events.clone(),
        )));
        timer::install(&mut runtime.heap, timers.clone())?;
        crate::async_trigger::install(&mut runtime.heap, events.clone())?;
        let operations = Operations::new(scheduler.max_contexts);
        crate::bitmap::install(&mut runtime.heap, operations.clone())?;
        let sounds = crate::sound::install(
            &mut runtime.heap,
            clock.clone(),
            operations.clone(),
            events.clone(),
        )?;
        crate::rect::install(&mut runtime.heap)?;
        crate::menu::install(&mut runtime.heap)?;
        let fonts = crate::font::install(&mut runtime.heap, operations.clone())?;
        let windows = crate::window::install(
            &mut runtime.heap,
            operations.clone(),
            events.clone(),
            clock.clone(),
        )?;
        let layers = crate::layer::install(&mut runtime.heap, windows.clone())?;
        let videos = crate::video::install(
            &mut runtime.heap,
            operations.clone(),
            events.clone(),
            windows.clone(),
            layers.clone(),
            sounds.borrow().backend.clone(),
        )?;
        crate::scripts::attach(&mut runtime.heap, operations.clone())?;
        crate::kag::attach(&mut runtime.heap, operations.clone())?;
        let system = system::install(
            &mut runtime.heap,
            clock.clone(),
            operations.clone(),
            config,
            scheduler.max_contexts,
        )?;
        crate::clipboard::install(&mut runtime.heap)?;
        let global = runtime.heap.alloc_global();
        windows.borrow_mut().system = Rc::downgrade(&system);
        crate::plugins::attach(&mut runtime.heap, global)?;
        constants::install(&mut runtime.heap, global)?;
        let timer_name = Value::Str(
            runtime
                .heap
                .alloc_string("onTimer".encode_utf16().collect::<Vec<_>>()),
        );
        let fire_name = Value::Str(
            runtime
                .heap
                .alloc_string("onFire".encode_utf16().collect::<Vec<_>>()),
        );
        Ok(Self {
            driver: TimedScheduler::new(Scheduler::with_limits(runtime, scheduler), clock),
            timers,
            events,
            callbacks: HashMap::new(),
            global,
            timer_name,
            fire_name,
            system,
            operations,
            windows,
            layers,
            fonts,
            sounds,
            videos,
            terminated: None,
            startup: Startup::NotStarted,
            exiting_context: None,
            completion_budget_exhausted: false,
            presentation_deferred: false,
            collection_threshold: 256 * 1024,
            last_collection: Duration::ZERO,
            next_media_poll: Duration::ZERO,
        })
    }
    pub fn runtime(&self) -> &Runtime {
        self.driver.runtime()
    }

    pub fn set_audio_output(
        &mut self,
        output: impl krkr_audio::OutputHost + 'static,
    ) -> NativeResult<()> {
        self.sounds
            .borrow()
            .backend
            .set_output(output)
            .map_err(tjs_core::NativeError::Detail)
    }
    pub fn audio_host_error(&self) -> Option<String> {
        self.sounds.borrow().backend.error()
    }
    pub fn set_video_backend(&mut self, backend: impl krkr_video::Backend + 'static) {
        self.videos.borrow().backend.set_backend(backend);
    }
    pub fn set_audio_decoder_backend(
        &mut self,
        backend: impl krkr_audio::DecoderBackend + 'static,
    ) {
        self.sounds.borrow().backend.set_decoder_backend(backend);
    }
    /// Host configuration before creating any CPU Bitmap objects.
    pub fn set_bitmap_memory_limit(&mut self, bytes: usize) -> NativeResult<()> {
        crate::bitmap::set_budget(&mut self.runtime_mut().heap, bytes)
    }
    pub fn set_font_provider(&mut self, provider: impl krkr_render::font::Provider + 'static) {
        self.fonts
            .worker
            .lock()
            .unwrap()
            .set_provider(Box::new(provider));
    }
    /// Host configuration before font registration or rasterization starts.
    pub fn set_font_memory_limit(&mut self, bytes: usize) -> NativeResult<()> {
        let mut system = self.fonts.worker.lock().unwrap();
        if system.budget.used() != 0 {
            return Err(tjs_core::NativeError::Message(
                "configure font budget before loading fonts",
            ));
        }
        system.budget = krkr_protocol::budget::Budget::new(bytes);
        system.budget.set_profile_name("memory.font_bytes");
        Ok(())
    }
    pub fn runtime_mut(&mut self) -> &mut Runtime {
        self.driver.runtime_mut()
    }
    pub fn global(&self) -> ObjId {
        self.global
    }
    pub fn submit(&mut self, module: &Module) -> Result<ContextId, Box<Vm>> {
        if self.terminated.is_some() || self.system.borrow().exit.is_some() {
            return Err(Box::new(Vm::with_global(module, self.global)));
        }
        self.driver.enqueue(Vm::with_global(module, self.global))
    }
    /// Host-owned script callbacks use the same scheduler, resource IO and roots.
    pub fn submit_callback(
        &mut self,
        callback: tjs_core::Callback,
        arguments: Vec<Value>,
    ) -> Result<ContextId, Box<Vm>> {
        if self.terminated.is_some() || self.system.borrow().exit.is_some() {
            return Err(Box::new(Vm::callback(self.global, callback, arguments)));
        }
        self.driver
            .enqueue_callback(self.global, callback, arguments)
    }
    /// Execute an application's startup script once per reset. Ordinary
    /// submitted scripts do not run the no-window startup policy.
    pub fn start(&mut self, module: &Module) -> Result<ContextId, Box<Vm>> {
        if !matches!(self.startup, Startup::NotStarted) {
            return Err(Box::new(Vm::with_global(module, self.global)));
        }
        let context = self.submit(module)?;
        self.startup = Startup::Running(context);
        Ok(context)
    }
    /// An application may intentionally stay alive without windows or tasks.
    /// Finite script tools use submit() and remain free to finish when idle.
    pub fn application_running(&self) -> bool {
        self.terminated.is_none()
            && matches!(self.startup, Startup::Running(_) | Startup::Completed)
    }
    fn exit_ready(&self) -> bool {
        self.system.borrow().exit.is_some()
            && self.exiting_context.is_none()
            && !self.windows.borrow().closing(&self.driver.runtime().heap)
    }
    fn take_exit(&mut self) -> Option<i32> {
        if self.exit_ready() {
            self.system.borrow_mut().exit.take()
        } else {
            None
        }
    }
    pub fn take_result(&mut self, id: ContextId) -> Option<RuntimeExit> {
        let result = self.driver.take_result(id)?;
        self.callbacks.remove(&id);
        Some(result)
    }
    pub fn work_executed(&self) -> u64 {
        self.driver.work_executed()
    }
    pub fn instructions_executed(&self) -> u64 {
        self.driver.instructions_executed()
    }
    /// Capture the active script stack on demand, without changing execution.
    pub fn active_diagnostic(&self) -> Option<tjs_core::Diagnostic> {
        self.driver.active_diagnostic()
    }
    pub fn now(&self) -> Duration {
        self.driver.now()
    }
    pub fn system_config(&self) -> std::cell::Ref<'_, SystemConfig> {
        std::cell::Ref::map(self.system.borrow(), |system| &system.config)
    }
    /// Post a host focus transition. Repeated identical state is coalesced;
    /// distinct transitions retain order and obey the normal event-entry gate.
    pub fn set_active(&mut self, active: bool) -> bool {
        self.system.borrow_mut().set_active(active)
    }
    pub fn owns_wait(&self, request: WaitRequest) -> bool {
        self.operations.borrow().contains(request.token)
    }
    pub fn pending_operations(&self) -> usize {
        self.operations.borrow().len()
    }
    /// External work still needed before a caller can advance a script clock.
    /// Timed script waits are excluded because advancing that clock wakes them.
    pub fn pending_host_operations(&self) -> usize {
        self.operations.borrow().pending_host()
    }
    /// Attach before executing scripts. The endpoint carries only owned platform data.
    pub fn attach_windows(&mut self, host: krkr_protocol::window::Client) -> NativeResult<()> {
        let mut windows = self.windows.borrow_mut();
        if windows.has_resources() {
            return Err(tjs_core::NativeError::Message(
                "attach the window host before creating windows",
            ));
        }
        self.operations.borrow_mut().images = host.image_cache();
        host.set_waker(self.operations.borrow().waker());
        self.system.borrow_mut().input = Some(host.clone());
        windows.host = Some(host);
        Ok(())
    }
    pub fn window_count(&self) -> usize {
        self.windows.borrow().count()
    }
    pub fn window_host_error(&self) -> Option<String> {
        self.windows
            .borrow()
            .host
            .as_ref()
            .and_then(|host| host.failure())
            .or_else(|| self.layers.borrow().failure.clone())
    }
    /// Install the platform-loop wakeup before the first IO request. The default
    /// unparks the thread that constructed the engine.
    pub fn set_waker(&mut self, wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> NativeResult<()> {
        self.operations.borrow_mut().set_waker(wake.clone())?;
        if let Some(host) = &self.windows.borrow().host {
            host.set_waker(wake);
        }
        Ok(())
    }
    pub fn arm_wait(&mut self, wait: WaitId, deadline: Duration) -> bool {
        self.driver.arm(wait, deadline)
    }
    pub fn complete(&mut self, wait: WaitId, result: Result<Value, tjs_core::NativeError>) -> bool {
        self.driver.complete(wait, result)
    }
    pub fn cancel(&mut self, id: ContextId) -> Vec<CancelledContext> {
        let cancelled = self.driver.cancel(id);
        crate::plugins::cancelled(&mut self.driver.runtime_mut().heap);
        if self
            .exiting_context
            .is_some_and(|context| cancelled.iter().any(|entry| entry.context == context))
        {
            self.exiting_context = None;
        }
        if let Startup::Running(context) = self.startup
            && cancelled.iter().any(|entry| entry.context == context)
        {
            self.startup = Startup::Cancelled;
        }
        for entry in &cancelled {
            if let Some(CallbackState {
                kind: CallbackKind::Continuous(function),
                ..
            }) = self.callbacks.remove(&entry.context)
            {
                self.system.borrow_mut().finished(function, true);
            }
        }
        cancelled
    }
    pub fn reset(&mut self) -> Vec<CancelledContext> {
        self.operations.borrow().images.clear();
        self.sounds.borrow_mut().reset();
        self.videos.borrow_mut().reset();
        self.events.borrow_mut().clear();
        self.windows.borrow_mut().reset();
        self.layers.borrow_mut().reset();
        self.timers.borrow_mut().clear();
        self.system.borrow_mut().reset();
        self.terminated = None;
        self.startup = Startup::NotStarted;
        self.exiting_context = None;
        self.completion_budget_exhausted = false;
        self.presentation_deferred = false;
        self.next_media_poll = Duration::ZERO;
        self.callbacks.clear();
        let cancelled = self.driver.cancel_all();
        crate::plugins::cancelled(&mut self.driver.runtime_mut().heap);
        crate::scripts::reset(&mut self.driver.runtime_mut().heap);
        cancelled
    }
    fn can_dispatch(&self) -> bool {
        !self.system.borrow().event_disabled
            && self.driver.can_enter_event()
            && !self.callbacks.values().any(|state| {
                matches!(state.kind, CallbackKind::Posted(Event { priority: 0, .. }))
                    && !state.finished
                    && !state.modal_wait
            })
    }
    fn collection_roots(&self, roots: impl IntoIterator<Item = Value>) -> Vec<Value> {
        let events = self.events.borrow();
        // Release the queue borrow before GC: dropping a native lease removes
        // its source record. Only queued events root owners, not idle records.
        let roots: Vec<_> = [
            Value::Obj(self.global.into()),
            self.timer_name,
            self.fire_name,
        ]
        .into_iter()
        .chain(events.roots())
        .chain(self.sounds.borrow().roots())
        .chain(self.videos.borrow().roots())
        .chain(
            self.callbacks
                .values()
                .filter_map(|state| state.kind.root()),
        )
        .chain(roots)
        .collect();
        roots
    }

    pub fn collect(&mut self, roots: impl IntoIterator<Item = Value>) -> tjs_core::CollectionStats {
        let roots = self.collection_roots(roots);
        let stats = self.driver.collect(roots);
        self.collection_completed(&stats);
        stats
    }

    fn collection_completed(&mut self, stats: &tjs_core::CollectionStats) {
        self.collection_threshold = (stats.retained_bytes / 2).clamp(256 * 1024, 4 * 1024 * 1024);
        self.last_collection = self.driver.now();
    }

    /// Optional automatic collection at a host boundary, after taking results.
    /// Each call advances a slice; Some reports a completed cycle only.
    /// Explicit collect/System.doCompact retain their immediate semantics.
    pub fn collect_if_needed(&mut self) -> Option<tjs_core::CollectionStats> {
        if !self.collection_sleep().is_some_and(|delay| delay.is_zero()) {
            return None;
        }
        let roots = self.collection_roots([]);
        let completed = self.driver.collect_auto(roots);
        if let Some(stats) = &completed {
            self.collection_completed(stats);
        }
        completed
    }
    fn collection_sleep(&self) -> Option<Duration> {
        if self.runtime().heap.is_collecting() {
            return Some(Duration::ZERO);
        }
        let debt = self.runtime().allocation_debt();
        if debt >= self.collection_threshold {
            return Some(Duration::ZERO);
        }
        // Quiet games still release unreachable native owners. Do this between
        // script updates, never once per mouse/timer callback or drawing wait.
        (debt != 0 && !self.presentation_deferred && self.driver.can_enter_event()).then(|| {
            Duration::from_secs(1)
                .saturating_sub(self.driver.now().saturating_sub(self.last_collection))
        })
    }
    pub fn sleep_duration(&self) -> Option<Duration> {
        if self.terminated.is_some() {
            return None;
        }
        if self.exit_ready() {
            return Some(Duration::ZERO);
        }
        if self.completion_budget_exhausted
            || (!self.presentation_deferred && self.layers.borrow().has_dirty())
        {
            return Some(Duration::ZERO);
        }
        if self.can_dispatch() && self.events.borrow().peek().is_some() {
            return Some(Duration::ZERO);
        }
        if self.can_dispatch()
            && self
                .windows
                .borrow()
                .host
                .as_ref()
                .is_some_and(|h| h.peek_event().is_some())
        {
            return Some(Duration::ZERO);
        }
        // Internal waits and disabled events may leave timer work queued. Such
        // queues must not turn a blocking IO wait into a busy loop.
        let events = self.can_dispatch();
        let media = [
            self.sounds.borrow().sleep_duration(),
            self.videos.borrow().sleep_duration(),
        ]
        .into_iter()
        .flatten()
        .min()
        .map(|delay| {
            if self.presentation_deferred {
                delay.max(self.next_media_poll.saturating_sub(self.driver.now()))
            } else {
                delay
            }
        });
        [
            self.collection_sleep(),
            media,
            self.driver.sleep_duration(),
            (!self.presentation_deferred)
                .then(|| self.layers.borrow().transition_sleep(events))
                .flatten(),
            events
                .then(|| self.windows.borrow().sleep_duration())
                .flatten(),
            events
                .then(|| self.timers.borrow().sleep_duration())
                .flatten(),
            events
                .then(|| self.system.borrow().sleep_duration())
                .flatten(),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

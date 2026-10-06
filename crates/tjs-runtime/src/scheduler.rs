//! Owned execution contexts. Instruction yield never authorizes event reentry.
use crate::{Runtime, RuntimeExit};
use slotmap::{SlotMap, new_key_type};
use std::collections::VecDeque;
use tjs_core::{CollectionStats, NativeError, RunBudget, Value, Vm, WaitMode, WaitRequest};

/// Host policy; completed results count until the host consumes them.
#[derive(Clone, Copy, Debug)]
pub struct SchedulerLimits {
    pub max_contexts: usize,
    pub max_event_depth: std::num::NonZeroUsize,
}
impl Default for SchedulerLimits {
    fn default() -> Self {
        Self {
            max_contexts: 1024,
            max_event_depth: std::num::NonZeroUsize::new(64).expect("positive depth"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CancelledContext {
    pub context: ContextId,
    pub wait: Option<(WaitId, WaitRequest)>,
}

new_key_type! { pub struct ContextId; pub struct WaitId; }

struct Context {
    vm: Vm,
    wait: Option<WaitId>,
    result: Option<RuntimeExit>,
    finalizer: bool,
    completion: Option<Box<Completion>>,
    reusable: bool,
}
struct Completion {
    vm: Vm,
    result: RuntimeExit,
    active: bool,
}
struct Wait {
    owner: ContextId,
    request: WaitRequest,
    result: Option<Result<Value, NativeError>>,
}

#[derive(Clone, Debug)]
pub enum SchedulerEvent {
    Idle,
    Finalized,
    Yielded(ContextId),
    Waiting {
        context: ContextId,
        wait: WaitId,
        request: WaitRequest,
    },
    Completed {
        context: ContextId,
        result: RuntimeExit,
    },
}

#[derive(Default)]
pub struct Scheduler {
    pub runtime: Runtime,
    contexts: SlotMap<ContextId, Context>,
    waits: SlotMap<WaitId, Wait>,
    queued: VecDeque<ContextId>,
    stack: Vec<ContextId>,
    limits: SchedulerLimits,
    work: u64,
    instructions: u64,
    idle_task: Option<Vm>,
}

impl Scheduler {
    pub fn new(runtime: Runtime) -> Self {
        Self::with_limits(runtime, SchedulerLimits::default())
    }

    pub fn with_limits(runtime: Runtime, limits: SchedulerLimits) -> Self {
        Self {
            runtime,
            contexts: SlotMap::with_key(),
            waits: SlotMap::with_key(),
            queued: VecDeque::new(),
            stack: Vec::new(),
            limits,
            work: 0,
            instructions: 0,
            idle_task: None,
        }
    }

    /// Queue an owned VM. It starts only when idle or the top context explicitly
    /// permits events; ordinary budget exhaustion does not permit nesting.
    pub fn enqueue(&mut self, vm: Vm) -> Result<ContextId, Box<Vm>> {
        self.enqueue_inner(vm, false)
    }

    /// Queue a callback using the buffers of the last consumed callback.
    pub fn enqueue_callback(
        &mut self,
        global: tjs_core::ObjId,
        callback: tjs_core::Callback,
        arguments: Vec<Value>,
    ) -> Result<ContextId, Box<Vm>> {
        let vm = if let Some(mut vm) = self.idle_task.take() {
            vm.restart_callback(global, callback, arguments);
            vm
        } else {
            Vm::callback(global, callback, arguments)
        };
        self.enqueue_inner(vm, true)
    }

    /// Queue native event work under the same callback storage policy.
    pub fn enqueue_task(
        &mut self,
        global: tjs_core::ObjId,
        task: Box<dyn tjs_core::NativeContinuation>,
    ) -> Result<ContextId, Box<Vm>> {
        let vm = if let Some(mut vm) = self.idle_task.take() {
            vm.restart_task(global, task);
            vm
        } else {
            Vm::task(global, task)
        };
        self.enqueue_inner(vm, true)
    }

    fn enqueue_inner(&mut self, vm: Vm, reusable: bool) -> Result<ContextId, Box<Vm>> {
        if self.contexts.len() >= self.limits.max_contexts {
            return Err(Box::new(vm));
        }
        let id = self.contexts.insert(Context {
            vm,
            wait: None,
            result: None,
            finalizer: false,
            completion: None,
            reusable,
        });
        self.queued.push_back(id);
        Ok(id)
    }

    pub fn context_count(&self) -> usize {
        self.contexts.len()
    }
    pub fn wait_count(&self) -> usize {
        self.waits.len()
    }

    pub fn work_executed(&self) -> u64 {
        self.work
    }
    pub fn instructions_executed(&self) -> u64 {
        self.instructions
    }

    /// Snapshot the active script for an explicitly requested profiler sample.
    pub fn active_diagnostic(&self) -> Option<tjs_core::Diagnostic> {
        let context = self.contexts.get(*self.stack.last()?)?;
        let vm = context
            .completion
            .as_ref()
            .filter(|c| c.active)
            .map_or(&context.vm, |c| &c.vm);
        Some(vm.diagnostic("script sample"))
    }

    pub fn can_enter_event(&self) -> bool {
        self.contexts.len() < self.limits.max_contexts
            && self.stack.len() < self.limits.max_event_depth.get()
            && self.queued.is_empty()
            && self.runtime.heap.pending_finalizers() == 0
            && self.stack.last().is_none_or(|id| {
                self.contexts[*id].wait.is_some_and(|wait| {
                    self.waits[wait].request.mode == WaitMode::Event
                        && self.waits[wait].result.is_none()
                })
            })
    }

    pub fn is_wait_pending(&self, id: WaitId) -> bool {
        self.waits.get(id).is_some_and(|wait| wait.result.is_none())
    }

    /// Host sleep decisions must account for completions and eligible events,
    /// not just whether a VM is currently on the active stack.
    pub fn has_runnable(&self) -> bool {
        let may_enter = self.stack.len() < self.limits.max_event_depth.get();
        let finalizer = self.contexts.len() < self.limits.max_contexts
            && self.runtime.heap.pending_finalizers() > 0;
        match self.stack.last() {
            None => !self.queued.is_empty() || finalizer,
            Some(id) => match self.contexts[*id].wait {
                None => true,
                Some(wait) => {
                    self.waits[wait].result.is_some()
                        || (may_enter
                            && self.waits[wait].request.mode == WaitMode::Event
                            && (!self.queued.is_empty() || finalizer))
                }
            },
        }
    }

    /// Each ID accepts at most one completion. A lower context's result stays
    /// parked and rooted until all contexts above it have finished.
    pub fn complete(&mut self, id: WaitId, result: Result<Value, NativeError>) -> bool {
        let Some(wait) = self.waits.get_mut(id) else {
            return false;
        };
        if wait.result.is_some() {
            return false;
        }
        wait.result = Some(result);
        true
    }

    /// Cancel active descendants first, then their parent. Queued independent
    /// events survive. Return every operation the host must cancel.
    pub fn cancel(&mut self, id: ContextId) -> Vec<CancelledContext> {
        let mut cancelled = Vec::new();
        if let Some(index) = self.stack.iter().position(|&active| active == id) {
            while self.stack.len() > index {
                let active = self.stack.pop().expect("active context");
                cancelled.push(self.remove_context(active));
            }
        } else if self.contexts.contains_key(id) {
            self.queued.retain(|&queued| queued != id);
            cancelled.push(self.remove_context(id));
        }
        cancelled
    }

    /// Session shutdown drops active calls top-down, then queued/results.
    pub fn cancel_all(&mut self) -> Vec<CancelledContext> {
        self.idle_task = None;
        let mut cancelled = Vec::with_capacity(self.contexts.len());
        while let Some(id) = self.stack.pop() {
            cancelled.push(self.remove_context(id));
        }
        self.queued.clear();
        let remaining: Vec<_> = self.contexts.keys().collect();
        cancelled.extend(remaining.into_iter().map(|id| self.remove_context(id)));
        cancelled
    }

    fn remove_context(&mut self, id: ContextId) -> CancelledContext {
        let context = self.contexts.remove(id).expect("owned context");
        let wait = context.wait.map(|id| {
            let wait = self.waits.remove(id).expect("owned wait");
            (id, wait.request)
        });
        // Dropping the context releases native continuations before its parent.
        drop(context);
        CancelledContext { context: id, wait }
    }

    /// Completed VMs keep their result rooted until explicitly taken. After
    /// this call, the receiving host must retain/root any managed return value.
    pub fn take_result(&mut self, id: ContextId) -> Option<RuntimeExit> {
        self.contexts.get(id)?.result.as_ref()?;
        let context = self.contexts.remove(id)?;
        let result = context.result;
        // Results and error-handler VMs stay rooted until consumption. Retain
        // only cleared execution buffers, never their values or code pools.
        if context.reusable && self.idle_task.is_none() {
            self.idle_task = context.vm.into_idle_task();
        }
        result
    }

    /// Run a completion handler in the same context slot, before its waiting
    /// parent resumes. Retain the original VM/code/result through cancellation
    /// or result consumption; handlers remain usable at max_contexts capacity.
    pub fn resume_completed(&mut self, id: ContextId, vm: Vm) -> Result<(), Box<Vm>> {
        let Some(context) = self.contexts.get_mut(id) else {
            return Err(Box::new(vm));
        };
        if context.result.is_none() || context.completion.is_some() {
            return Err(Box::new(vm));
        }
        context.completion = Some(Box::new(Completion {
            vm: std::mem::replace(&mut context.vm, vm),
            result: context.result.take().expect("completed context"),
            active: true,
        }));
        self.stack.push(id);
        Ok(())
    }
    pub fn original_result(&self, id: ContextId) -> Option<&RuntimeExit> {
        self.contexts
            .get(id)?
            .completion
            .as_ref()
            .filter(|c| c.active)
            .map(|c| &c.result)
    }
    /// Resolve a completed handler once, without reinvoking it for its own error.
    pub fn resolve_completion(&mut self, id: ContextId, result: RuntimeExit) -> bool {
        let Some(context) = self.contexts.get_mut(id) else {
            return false;
        };
        let Some(completion) = context.completion.as_mut().filter(|c| c.active) else {
            return false;
        };
        if context.result.is_none() {
            return false;
        }
        completion.active = false;
        context.result = Some(result);
        true
    }

    pub fn vm(&self, id: ContextId) -> Option<&Vm> {
        self.contexts.get(id).map(|context| &context.vm)
    }

    pub fn poll(&mut self, budget: RunBudget) -> SchedulerEvent {
        // Finalizers execute as ordinary budgeted contexts, only at boundaries
        // where running another script is allowed.
        let may_enter = self.contexts.len() < self.limits.max_contexts
            && self.stack.len() < self.limits.max_event_depth.get()
            && self.stack.last().is_none_or(|id| {
                self.contexts[*id]
                    .wait
                    .is_some_and(|wait| self.waits[wait].request.mode == WaitMode::Event)
            });
        if may_enter {
            if let Some(vm) = Vm::take_finalizer(&mut self.runtime.heap) {
                let id = self.contexts.insert(Context {
                    vm,
                    wait: None,
                    result: None,
                    finalizer: true,
                    completion: None,
                    reusable: false,
                });
                self.queued.push_front(id);
            }
        }
        if let Some(&id) = self.stack.last() {
            if let Some(wait_id) = self.contexts[id].wait {
                if self.waits[wait_id].result.is_some() {
                    let wait = self.waits.remove(wait_id).expect("active wait");
                    debug_assert_eq!(wait.owner, id);
                    let context = &mut self.contexts[id];
                    context.wait = None;
                    if let Err(error) = context.vm.resume_wait(wait.result.expect("completed wait"))
                    {
                        return self.finish(id, RuntimeExit::Fault(error));
                    }
                } else if self.waits[wait_id].request.mode == WaitMode::Event
                    && self.stack.len() < self.limits.max_event_depth.get()
                {
                    if let Some(next) = self.queued.pop_front() {
                        self.stack.push(next);
                    }
                }
            }
        } else if let Some(next) = self.queued.pop_front() {
            self.stack.push(next);
        }
        let Some(&id) = self.stack.last() else {
            return SchedulerEvent::Idle;
        };
        if let Some(wait) = self.contexts[id].wait {
            return SchedulerEvent::Waiting {
                context: id,
                wait,
                request: self.waits[wait].request,
            };
        }
        let vm = &mut self.contexts[id].vm;
        let work = vm.work_executed();
        let instructions = vm.instructions_executed();
        let result = self.runtime.run_slice(vm, budget);
        self.work += vm.work_executed() - work;
        self.instructions += vm.instructions_executed() - instructions;
        match result {
            RuntimeExit::Yielded => SchedulerEvent::Yielded(id),
            RuntimeExit::Waiting(request) => {
                let wait = self.waits.insert(Wait {
                    owner: id,
                    request,
                    result: None,
                });
                self.contexts[id].wait = Some(wait);
                SchedulerEvent::Waiting {
                    context: id,
                    wait,
                    request,
                }
            }
            result => self.finish(id, result),
        }
    }

    fn finish(&mut self, id: ContextId, result: RuntimeExit) -> SchedulerEvent {
        self.stack.pop();
        if self.contexts[id].finalizer
            && self.contexts[id].completion.is_none()
            && matches!(result, RuntimeExit::Finished(_))
        {
            self.contexts.remove(id);
            return SchedulerEvent::Finalized;
        }
        self.contexts[id].result = Some(result.clone());
        SchedulerEvent::Completed {
            context: id,
            result,
        }
    }

    /// Trace all queued, active, waiting and completed contexts, including
    /// completions delivered early to a context below the top of the stack.
    pub fn collect(&mut self, host_roots: impl IntoIterator<Item = Value>) -> CollectionStats {
        self.runtime
            .collect(Self::roots(&self.contexts, &self.waits).chain(host_roots))
    }

    pub fn collect_step(
        &mut self,
        host_roots: impl IntoIterator<Item = Value>,
        budget: usize,
    ) -> tjs_core::CollectionStep {
        self.runtime.collect_step(
            Self::roots(&self.contexts, &self.waits).chain(host_roots),
            budget,
        )
    }

    pub fn collect_auto(
        &mut self,
        host_roots: impl IntoIterator<Item = Value>,
    ) -> Option<CollectionStats> {
        self.runtime
            .collect_auto(Self::roots(&self.contexts, &self.waits).chain(host_roots))
    }

    fn roots<'a>(
        contexts: &'a SlotMap<ContextId, Context>,
        waits: &'a SlotMap<WaitId, Wait>,
    ) -> impl Iterator<Item = Value> + 'a {
        contexts
            .values()
            .flat_map(|context| context.vm.roots())
            .chain(
                contexts
                    .values()
                    .filter_map(|context| context.completion.as_ref())
                    .flat_map(|c| c.vm.roots()),
            )
            .chain(waits.values().filter_map(|wait| match &wait.result {
                Some(Ok(value)) => Some(*value),
                _ => None,
            }))
    }
}

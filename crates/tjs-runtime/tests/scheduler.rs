use tjs_bind as tjs;
use tjs_core::{
    NativeContinuation, NativeCx, NativeResult, NativeStep, RunBudget, Value, Vm, WaitMode,
    WaitRequest,
};
use tjs_runtime::{Runtime, RuntimeExit, Scheduler, SchedulerEvent, WaitId};

#[tjs::class(name = "Gate")]
mod gate {
    use super::*;
    #[derive(Default, tjs::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method(resumable = true)]
        fn wait(&self, token: i64, events: i64, held: Value) -> NativeStep {
            NativeStep::Wait {
                request: WaitRequest {
                    token: token as u64,
                    mode: if events == 0 {
                        WaitMode::Internal
                    } else {
                        WaitMode::Event
                    },
                },
                continuation: Box::new(Resume { held }),
            }
        }
    }
    struct Resume {
        held: Value,
    }
    impl tjs::Trace for Resume {
        fn trace(&self, visit: &mut dyn FnMut(Value)) {
            visit(self.held);
        }
    }
    impl NativeContinuation for Resume {
        fn resume(
            self: Box<Self>,
            cx: &mut NativeCx<'_>,
            value: Value,
        ) -> NativeResult<NativeStep> {
            cx.heap().display(self.held)?;
            Ok(NativeStep::Return(value))
        }
    }
}

fn setup() -> (Scheduler, tjs::ObjId) {
    let mut runtime = Runtime::new();
    gate::install(&mut runtime.heap).unwrap();
    let global = runtime.heap.alloc_global();
    (Scheduler::new(runtime), global)
}
fn enqueue(s: &mut Scheduler, global: tjs::ObjId, text: &str) -> tjs_runtime::ContextId {
    let source = s.runtime.sources.add_utf8("scheduler", text).unwrap();
    let module = tjs_front::compile(&s.runtime.sources, source).unwrap();
    s.enqueue(Vm::with_global(&module, global))
        .unwrap_or_else(|_| panic!("context capacity"))
}
fn event(s: &mut Scheduler) -> SchedulerEvent {
    for _ in 0..10000 {
        let e = s.poll(RunBudget::new(1).unwrap());
        s.collect([]);
        if !matches!(e, SchedulerEvent::Yielded(_)) {
            return e;
        }
    }
    panic!("scheduler did not reach boundary")
}
fn waiting(e: SchedulerEvent) -> WaitId {
    let SchedulerEvent::Waiting { wait, .. } = e else {
        panic!("{e:?}")
    };
    wait
}
fn finished(s: &mut Scheduler, id: tjs_runtime::ContextId) -> Value {
    let SchedulerEvent::Completed {
        context,
        result: RuntimeExit::Finished(value),
    } = event(s)
    else {
        panic!("not finished")
    };
    assert_eq!(context, id);
    value
}

#[test]
fn reused_callbacks_keep_wait_ids_distinct_and_release_cancelled_work() {
    let (mut s, global) = setup();
    let init = enqueue(
        &mut s,
        global,
        "function tick(x) {return new Gate().wait(x,0,x);} tick;",
    );
    let callback = finished(&mut s, init);
    s.take_result(init).unwrap();
    let mut old_wait = None;
    for n in 0..5 {
        let context = s
            .enqueue_callback(
                global,
                tjs_core::Callback::Function(callback),
                vec![Value::Int(n)],
            )
            .unwrap_or_else(|_| panic!("context capacity"));
        let wait = waiting(event(&mut s));
        if let Some(old) = old_wait {
            assert!(!s.complete(old, Ok(Value::Int(999))));
        }
        if n == 2 {
            s.cancel(context);
            assert!(!s.complete(wait, Ok(Value::Int(999))));
        } else {
            assert!(s.complete(wait, Ok(Value::Int(n + 10))));
            assert_eq!(finished(&mut s, context).as_integer(), Some(n + 10));
            assert!(matches!(
                s.take_result(context),
                Some(RuntimeExit::Finished(_))
            ));
        }
        old_wait = Some(wait);
        s.collect([Value::Obj(global.into())]);
    }
    s.cancel_all();
}

#[test]
fn internal_wait_blocks_events_and_completions_are_single_use() {
    let (mut s, global) = setup();
    let a = enqueue(
        &mut s,
        global,
        "var order='a'; var g=new Gate; var n=g.wait(10,0,'held'); order+='b'; n;",
    );
    let wait = waiting(event(&mut s));
    let b = enqueue(&mut s, global, "order+='c'; order;");
    assert_eq!(waiting(event(&mut s)), wait);
    assert!(s.complete(wait, Ok(Value::Int(7))));
    assert!(!s.complete(wait, Ok(Value::Int(8))));
    assert_eq!(finished(&mut s, a).as_integer(), Some(7));
    let value = finished(&mut s, b);
    assert_eq!(s.runtime.heap.display(value).unwrap(), "abc");
    assert!(!s.complete(wait, Ok(Value::Void)));
}

#[test]
fn deadline_driver_never_restarts_waits_or_sleeps_over_runnable_work() {
    use std::{cell::Cell, num::NonZeroUsize, rc::Rc, time::Duration};
    use tjs_runtime::clock::{Clock, TimedScheduler};
    struct Manual(Rc<Cell<Duration>>);
    impl Clock for Manual {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }
    let now = Rc::new(Cell::new(Duration::ZERO));
    let (mut s, global) = setup();
    let owner = enqueue(&mut s, global, "(new Gate).wait(1,1,void); 42;");
    let wait = waiting(event(&mut s));
    let mut driver = TimedScheduler::new(s, Manual(now.clone()));
    assert!(driver.arm(wait, Duration::from_millis(100)));
    assert!(!driver.arm(wait, Duration::from_millis(500)));
    now.set(Duration::from_millis(80));
    assert_eq!(driver.sleep_duration(), Some(Duration::from_millis(20)));
    let budget = RunBudget::new(1).unwrap();
    let controls = NonZeroUsize::new(1).unwrap();
    assert_eq!(waiting(driver.poll(budget, controls)), wait);
    now.set(Duration::from_millis(100));
    loop {
        let e = driver.poll(budget, controls);
        driver.collect([]);
        if let SchedulerEvent::Completed {
            context,
            result: RuntimeExit::Finished(value),
        } = e
        {
            assert_eq!(context, owner);
            assert_eq!(value.as_integer(), Some(42));
            break;
        }
        assert!(matches!(e, SchedulerEvent::Yielded(_)));
        assert_eq!(driver.sleep_duration(), Some(Duration::ZERO));
    }
    assert!(!driver.arm(wait, Duration::from_secs(1)));
    assert_eq!(driver.sleep_duration(), None);
    let source = driver
        .runtime_mut()
        .sources
        .add_utf8("cancel deadline", "(new Gate).wait(2,0,void);")
        .unwrap();
    let module = tjs_front::compile(&driver.runtime().sources, source).unwrap();
    let pending = driver
        .enqueue(Vm::with_global(&module, global))
        .unwrap_or_else(|_| panic!("capacity"));
    let armed = loop {
        match driver.poll(budget, controls) {
            SchedulerEvent::Waiting { wait, .. } => break wait,
            SchedulerEvent::Yielded(_) => {}
            other => panic!("{other:?}"),
        }
    };
    assert!(driver.arm(armed, Duration::from_secs(3600)));
    assert_eq!(driver.cancel(pending)[0].wait.unwrap().0, armed);
    assert_eq!(driver.sleep_duration(), None);
    assert!(!driver.complete(armed, Ok(Value::Void)));
}

#[test]
fn actual_monotonic_deadline_resumes_script_and_cancel_removes_deadlines() {
    use std::{num::NonZeroUsize, time::Duration};
    use tjs_runtime::clock::{MonotonicClock, TimedScheduler};
    let (mut s, global) = setup();
    let owner = enqueue(&mut s, global, "(new Gate).wait(1,0,void); 42;");
    let wait = waiting(event(&mut s));
    let mut driver = TimedScheduler::new(s, MonotonicClock::default());
    let deadline = driver.now() + Duration::from_millis(5);
    assert!(driver.arm(wait, deadline));
    loop {
        if let Some(delay) = driver.sleep_duration() {
            std::thread::sleep(delay);
        }
        match driver.poll(RunBudget::new(100).unwrap(), NonZeroUsize::new(1).unwrap()) {
            SchedulerEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } => {
                assert_eq!(context, owner);
                assert_eq!(value.as_integer(), Some(42));
                assert!(driver.now() >= deadline);
                break;
            }
            SchedulerEvent::Yielded(_) | SchedulerEvent::Waiting { .. } => {}
            e => panic!("{e:?}"),
        }
    }
    driver.cancel_all();
    assert_eq!(driver.sleep_duration(), None);
    assert!(!driver.complete(wait, Ok(Value::Void)));
}

#[test]
fn cancelling_parent_removes_active_children_but_preserves_independent_queue() {
    let (mut s, global) = setup();
    let outer = enqueue(&mut s, global, "(new Gate).wait(1,1,void);");
    let a = waiting(event(&mut s));
    let inner = enqueue(&mut s, global, "(new Gate).wait(2,0,void);");
    let b = waiting(event(&mut s));
    let independent = enqueue(&mut s, global, "42;");
    let cancelled = s.cancel(outer);
    assert_eq!(
        cancelled.iter().map(|c| c.context).collect::<Vec<_>>(),
        [inner, outer]
    );
    assert_eq!(
        cancelled
            .iter()
            .map(|c| c.wait.unwrap().0)
            .collect::<Vec<_>>(),
        [b, a]
    );
    assert_eq!(s.wait_count(), 0);
    assert_eq!(s.context_count(), 1);
    assert!(!s.complete(a, Ok(Value::Void)));
    assert!(!s.complete(b, Ok(Value::Void)));
    assert_eq!(finished(&mut s, independent).as_integer(), Some(42));
    s.cancel_all();
    assert_eq!(s.context_count(), 0);
}

#[test]
fn context_capacity_retains_rejected_vm_and_depth_defers_events() {
    let (old, global) = setup();
    let mut s = Scheduler::with_limits(
        old.runtime,
        tjs_runtime::SchedulerLimits {
            max_contexts: 2,
            max_event_depth: std::num::NonZeroUsize::new(1).unwrap(),
        },
    );
    let first = enqueue(&mut s, global, "(new Gate).wait(1,1,void);");
    let wait = waiting(event(&mut s));
    let second = enqueue(&mut s, global, "7;");
    assert_eq!(waiting(event(&mut s)), wait);
    let source = s.runtime.sources.add_utf8("pending", "42;").unwrap();
    let module = tjs_front::compile(&s.runtime.sources, source).unwrap();
    let rejected = match s.enqueue(Vm::with_global(&module, global)) {
        Err(vm) => vm,
        Ok(_) => panic!("capacity exceeded"),
    };
    assert!(s.complete(wait, Ok(Value::Void)));
    finished(&mut s, first);
    // Completed results retain roots and occupy capacity until consumed.
    assert_eq!(s.context_count(), 2);
    s.take_result(first).unwrap();
    let third = s
        .enqueue(*rejected)
        .unwrap_or_else(|_| panic!("capacity not released"));
    assert_eq!(finished(&mut s, second).as_integer(), Some(7));
    assert_eq!(finished(&mut s, third).as_integer(), Some(42));
    s.cancel_all();
    assert_eq!(s.context_count(), 0);
}

#[test]
fn early_outer_completion_waits_for_nested_context_and_remains_rooted() {
    let (mut s, global) = setup();
    let outer = enqueue(
        &mut s,
        global,
        "var order='outer'; var g=new Gate; var result=g.wait(1,1,%[x:1]); order+=' resumed'; result;",
    );
    let a = waiting(event(&mut s));
    let inner = enqueue(
        &mut s,
        global,
        "order+=' inner'; g.wait(2,0,'held'); order+=' done'; order;",
    );
    let b = waiting(event(&mut s));
    let result = Value::Str(
        s.runtime
            .heap
            .alloc_string("retained".encode_utf16().collect::<Vec<_>>()),
    );
    assert!(s.complete(a, Ok(result)));
    s.collect([]);
    assert_eq!(waiting(event(&mut s)), b);
    assert!(s.complete(b, Ok(Value::Void)));
    let value = finished(&mut s, inner);
    assert_eq!(s.runtime.heap.display(value).unwrap(), "outer inner done");
    let value = finished(&mut s, outer);
    assert_eq!(s.runtime.heap.display(value).unwrap(), "retained");
    s.take_result(inner).unwrap();
    s.take_result(outer).unwrap();
    s.collect([]);
    let Value::Str(id) = result else {
        unreachable!()
    };
    assert!(s.runtime.heap.string(id).is_err());
}

#[test]
fn cancellation_rejects_late_completion_and_errors_enter_script_catch() {
    let (mut s, global) = setup();
    let first = enqueue(&mut s, global, "(new Gate).wait(4,0,'held');");
    let old = waiting(event(&mut s));
    assert_eq!(s.cancel(first)[0].wait.unwrap().1.token, 4);
    let second = enqueue(
        &mut s,
        global,
        "try { (new Gate).wait(4,0,'held'); } catch(e) { return e.message; }",
    );
    let new = waiting(event(&mut s));
    assert_ne!(old, new);
    assert!(!s.complete(old, Ok(Value::Void)));
    assert!(s.complete(new, Err(tjs::NativeError::Message("failed IO"))));
    let value = finished(&mut s, second);
    assert_eq!(s.runtime.heap.display(value).unwrap(), "failed IO");
}

#[test]
fn instruction_yield_does_not_allow_queued_event_to_run() {
    let (mut s, global) = setup();
    let a = enqueue(
        &mut s,
        global,
        "var order=''; for(var i=0;i<20;i++) order+='a'; order;",
    );
    let b = enqueue(&mut s, global, "order+='b'; order;");
    let value = finished(&mut s, a);
    assert_eq!(s.runtime.heap.display(value).unwrap(), "a".repeat(20));
    let value = finished(&mut s, b);
    assert_eq!(
        s.runtime.heap.display(value).unwrap(),
        format!("{}b", "a".repeat(20))
    );
}

#[test]
fn missing_wait_preserves_arguments_and_cancel_releases_reentry_guard() {
    let (mut s, global) = setup();
    let init = enqueue(
        &mut s,
        global,
        "class M { function missing(set,name,value) { (new Gate).wait(9,0,value); *value=function(a){return a;}; return true; } } var m=new M;",
    );
    finished(&mut s, init);
    let key = s
        .runtime
        .heap
        .intern(&"m".encode_utf16().collect::<Vec<_>>());
    let Value::Obj(object) = s.runtime.heap.member(global, key).unwrap().unwrap() else {
        panic!("m");
    };
    s.runtime
        .heap
        .set_call_missing(object.object.unwrap())
        .unwrap();
    let first = enqueue(&mut s, global, "m.answer('retained argument');");
    let old = waiting(event(&mut s));
    assert_eq!(s.cancel(first)[0].wait.unwrap().1.token, 9);
    let second = enqueue(&mut s, global, "m.answer('retained argument');");
    let next = waiting(event(&mut s));
    assert!(!s.complete(old, Ok(Value::Void)));
    assert!(s.complete(next, Ok(Value::Void)));
    let value = finished(&mut s, second);
    assert_eq!(s.runtime.heap.display(value).unwrap(), "retained argument");
}

#[test]
fn automatic_finalizers_run_on_scheduler_budget_without_entering_internal_wait() {
    let (mut s, global) = setup();
    let owner = enqueue(
        &mut s,
        global,
        "var count=0; class C { function finalize(){global.count++;} } var c=new C; c=void; (new Gate).wait(9,0,void); count;",
    );
    let wait = waiting(event(&mut s));
    assert!(s.runtime.heap.pending_finalizers() > 0);
    assert_eq!(waiting(event(&mut s)), wait);
    s.complete(wait, Ok(Value::Void));
    assert_eq!(finished(&mut s, owner).as_integer(), Some(0));
    assert!(matches!(event(&mut s), SchedulerEvent::Finalized));
    let check = enqueue(&mut s, global, "count;");
    assert_eq!(finished(&mut s, check).as_integer(), Some(1));
}

#[test]
fn compound_missing_operation_waits_for_read_and_write_and_cancels_cleanly() {
    let (mut s, global) = setup();
    let init = enqueue(
        &mut s,
        global,
        "var saved=0; class M { function missing(set,name,value) { (new Gate).wait(set?12:11,0,value); if(set) saved=*value; else *value=6; return true; } } var m=new M;",
    );
    finished(&mut s, init);
    let key = s
        .runtime
        .heap
        .intern(&"m".encode_utf16().collect::<Vec<_>>());
    let Value::Obj(object) = s.runtime.heap.member(global, key).unwrap().unwrap() else {
        panic!("m")
    };
    s.runtime
        .heap
        .set_call_missing(object.object.unwrap())
        .unwrap();
    let first = enqueue(&mut s, global, "m.x+=3;");
    let cancelled = waiting(event(&mut s));
    assert_eq!(s.cancel(first)[0].wait.unwrap().1.token, 11);
    let second = enqueue(&mut s, global, "var result=(m.x+=3); result*10+saved;");
    let read = waiting(event(&mut s));
    assert!(!s.complete(cancelled, Ok(Value::Void)));
    assert!(s.complete(read, Ok(Value::Void)));
    let write = waiting(event(&mut s));
    assert!(s.complete(write, Ok(Value::Void)));
    assert_eq!(finished(&mut s, second).as_integer(), Some(99));
}

#[test]
fn incremental_slices_preserve_waits_compilation_finalizer_cycles_and_resurrection() {
    let (mut scheduler, global) = setup();
    let root = scheduler.runtime.heap.root(Value::Obj(global.into()));
    let setup_id = enqueue(
        &mut scheduler,
        global,
        r#"
        var finalized=0, total=0, saved, gate=new Gate;
        class C {
            var n, peer;
            function C(v) { n=v; }
            function finalize() {
                (string(n))!;
                global.total+=n;
                global.saved=this;
                global.gate.wait(n,0,'finalizer payload');
                global.finalized++;
            }
        }
        function make() {
            var a=new C(3), b=new C(8);
            a.peer=b; b.peer=a;
        }
        make();
        42;
    "#,
    );
    let mut finished_setup = false;
    let mut waits = 0;
    for _ in 0..100_000 {
        let event = scheduler.poll(RunBudget::new(1).unwrap());
        let step = scheduler.collect_step([], 1);
        assert!(step.work <= 1);
        match event {
            SchedulerEvent::Completed { context, result } if context == setup_id => {
                assert!(matches!(result, RuntimeExit::Finished(Value::Int(42))));
                scheduler.take_result(context);
                finished_setup = true;
            }
            SchedulerEvent::Waiting { wait, .. } => {
                // The wait continuation and a newly delivered completion survive
                // many GC slices while the script finalizer is suspended.
                for _ in 0..1000 {
                    scheduler.collect_step([], 1);
                }
                let text = scheduler.runtime.heap.alloc_string(vec![42]);
                assert!(scheduler.complete(wait, Ok(Value::Str(text))));
                for _ in 0..1000 {
                    scheduler.collect_step([], 1);
                }
                waits += 1;
            }
            SchedulerEvent::Completed { result, .. } => panic!("{result:?}"),
            _ => {}
        }
        if finished_setup && waits == 2 && scheduler.context_count() == 0 {
            break;
        }
    }
    assert!(finished_setup);
    assert_eq!(waits, 2);
    let result = enqueue(
        &mut scheduler,
        global,
        "finalized*100+total*10+(isvalid saved);",
    );
    assert_eq!(finished(&mut scheduler, result).as_integer(), Some(310));
    scheduler.take_result(result);
    scheduler.runtime.heap.release_root(root);
    // No repeated finalization after resurrection of an invalidated instance.
    scheduler.collect([]);
    assert_eq!(scheduler.runtime.heap.pending_finalizers(), 0);
}

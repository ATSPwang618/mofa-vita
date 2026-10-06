use krkr_engine::{Engine, EngineEvent, TimerLimits};
use std::{cell::Cell, num::NonZeroUsize, rc::Rc, time::Duration};
use tjs_core::{
    NativeContinuation, NativeCx, NativeResult, NativeStep, RunBudget, Value, WaitMode, WaitRequest,
};
use tjs_runtime::{Runtime, RuntimeExit, clock::Clock};

struct Manual(Rc<Cell<Duration>>);
impl Clock for Manual {
    fn now(&self) -> Duration {
        self.0.get()
    }
}

#[tjs_bind::class(name = "Gate")]
mod gate {
    use super::*;
    #[derive(Default, tjs_bind::Trace)]
    pub struct State;
    impl State {
        #[tjs::constructor]
        fn new() -> Self {
            Self
        }
        #[tjs::method(resumable = true)]
        fn wait(events: bool) -> NativeStep {
            NativeStep::Wait {
                request: WaitRequest {
                    token: 1,
                    mode: if events {
                        WaitMode::Event
                    } else {
                        WaitMode::Internal
                    },
                },
                continuation: Box::new(Resume),
            }
        }
    }
    #[derive(tjs_bind::Trace)]
    struct Resume;
    impl NativeContinuation for Resume {
        fn resume(self: Box<Self>, _: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
            Ok(NativeStep::Return(value))
        }
    }
}
fn setup() -> (Engine<Manual>, Rc<Cell<Duration>>) {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut runtime = Runtime::new();
    gate::install(&mut runtime.heap).unwrap();
    let engine = Engine::new(
        runtime,
        Manual(now.clone()),
        Default::default(),
        TimerLimits::default(),
    )
    .unwrap();
    (engine, now)
}
fn step(engine: &mut Engine<Manual>) -> EngineEvent {
    let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
    engine.collect([]);
    event
}
fn submit(engine: &mut Engine<Manual>, script: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("timer test", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"))
}
fn run(engine: &mut Engine<Manual>, script: &str) -> String {
    let id = submit(engine, script);
    for _ in 0..10000 {
        match step(engine) {
            EngineEvent::Completed { context, result } if context == id => {
                let RuntimeExit::Finished(value) = result else {
                    panic!("{script}: {result:?}");
                };
                let output = engine.runtime().heap.display(value).unwrap();
                engine.take_result(id);
                return output;
            }
            EngineEvent::Yielded => {}
            _ => panic!("unexpected event"),
        }
    }
    panic!("script did not finish");
}
fn fire(engine: &mut Engine<Manual>) -> RuntimeExit {
    for _ in 0..10000 {
        match step(engine) {
            EngineEvent::Timer {
                context, result, ..
            } => {
                engine.take_result(context);
                return result;
            }
            EngineEvent::Yielded => {}
            _ => panic!("timer did not fire"),
        }
    }
    panic!("callback did not finish");
}

#[test]
fn timer_properties_action_owner_and_script_override_use_normal_dispatch() {
    let (mut engine, now) = setup();
    assert_eq!(
        run(
            &mut engine,
            "var seen=''; var owner=%[action:function(e){global.seen=e.type; return e.target;}]; var t=new Timer(owner); var defaults=t.interval*65536==1000 && !t.enabled && t.capacity==6 && t.mode==atmNormal; t.onTimer() === t && defaults && seen=='onTimer';"
        ),
        "1"
    );
    assert_eq!(
        run(
            &mut engine,
            "t.interval='10.5'; t.capacity='2'; t.mode='1'; t.interval==10.5 && t.capacity==2 && t.mode==atmExclusive;"
        ),
        "1"
    );
    run(
        &mut engine,
        "class T extends Timer { function T(){super.Timer(null);} function onTimer(){global.seen='override'; this.enabled=false;} } var derived=new T; derived.interval=10; derived.enabled=true;",
    );
    now.set(Duration::from_millis(11));
    assert!(matches!(fire(&mut engine), RuntimeExit::Finished(_)));
    assert_eq!(run(&mut engine, "seen;"), "override");
    assert_eq!(engine.sleep_duration(), None);
}

#[test]
fn backlog_capacity_priority_and_disable_clear_pending_events() {
    let (mut engine, now) = setup();
    run(
        &mut engine,
        r#"
        var trace='';
        function make(tag,mode) {
            var owner=%[tag:tag,action:function(e){global.trace+=tag; e.target.enabled=false;}];
            var t=new Timer(owner); t.interval=10; t.capacity=2; t.mode=mode; t.enabled=true; return t;
        }
        var normal=make('n',atmNormal), idle=make('i',atmAtIdle), exclusive=make('e',atmExclusive);
    "#,
    );
    now.set(Duration::from_millis(35));
    for _ in 0..3 {
        assert!(matches!(fire(&mut engine), RuntimeExit::Finished(_)));
    }
    assert_eq!(run(&mut engine, "trace;"), "eni");
    assert_eq!(engine.sleep_duration(), None);

    run(
        &mut engine,
        "var count=0; var t=new Timer(function(e){global.count++;},''); t.interval=10; t.capacity=2; t.enabled=true;",
    );
    now.set(Duration::from_millis(70));
    for _ in 0..2 {
        fire(&mut engine);
    }
    assert_eq!(run(&mut engine, "count;"), "2");
    // Altering interval clears pending deliveries and starts from the new time.
    run(&mut engine, "t.interval=100;");
    assert!(engine.sleep_duration().unwrap() >= Duration::from_millis(100));
    engine.reset();
    assert_eq!(engine.sleep_duration(), None);
    assert_eq!(run(&mut engine, "t.enabled;"), "0");
}

#[test]
fn timer_errors_invalidation_and_gc_release_native_resources() {
    let (mut engine, now) = setup();
    run(
        &mut engine,
        "var t=new Timer(function(e){e.target.enabled=false; throw 'timer error';},''); t.interval=10; t.enabled=true;",
    );
    now.set(Duration::from_millis(11));
    let RuntimeExit::Thrown(error) = fire(&mut engine) else {
        panic!("expected callback exception");
    };
    assert!(error.diagnostic.message.contains("timer error"));
    run(&mut engine, "t.enabled=true; invalidate t;");
    assert_eq!(engine.sleep_duration(), None);
    // Enabled timer records are weak; dropping script ownership releases them.
    run(
        &mut engine,
        "var dropped=new Timer(null); dropped.interval=20; dropped.enabled=true; dropped=void;",
    );
    engine.collect([]);
    assert_eq!(engine.sleep_duration(), None);
}

#[test]
fn host_budget_rejection_is_catchable_and_preserves_existing_capacity() {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut engine = Engine::new(
        Runtime::new(),
        Manual(now),
        Default::default(),
        TimerLimits {
            max_timers: 2,
            max_pending_events: 12,
        },
    )
    .unwrap();
    assert_eq!(
        run(
            &mut engine,
            "var a=new Timer(null), b=new Timer(null), count=0; try{a.capacity=0;}catch(e){count++;} try{new Timer(null);}catch(e){count++;} count==2 && a.capacity==6;"
        ),
        "1"
    );
    run(&mut engine, "invalidate a; var c=new Timer(null);");
    assert_eq!(run(&mut engine, "c.capacity;"), "6");
}

#[test]
fn completed_callback_keeps_its_owner_until_host_takes_result() {
    let (mut engine, now) = setup();
    run(
        &mut engine,
        "var t=new Timer(function(e){e.target.enabled=false; global.t=void;},''); t.interval=10; t.enabled=true;",
    );
    now.set(Duration::from_millis(11));
    for _ in 0..10000 {
        match step(&mut engine) {
            EngineEvent::Yielded => {}
            EngineEvent::Timer {
                context,
                owner,
                result: RuntimeExit::Finished(_),
            } => {
                assert!(engine.runtime().heap.is_valid(owner).unwrap());
                engine.take_result(context).unwrap();
                engine.collect([]);
                assert!(engine.runtime().heap.is_valid(owner).is_err());
                return;
            }
            other => panic!("{other:?}"),
        }
    }
    panic!("timer did not finish");
}

fn wait(engine: &mut Engine<Manual>) -> (tjs_runtime::ContextId, tjs_runtime::WaitId) {
    for _ in 0..10000 {
        match step(engine) {
            EngineEvent::Waiting { context, wait, .. } => return (context, wait),
            EngineEvent::Yielded => {}
            other => panic!("expected native wait, got {other:?}"),
        }
    }
    panic!("wait did not start");
}

#[test]
fn due_wait_and_pending_finalizer_can_cancel_timer_before_delivery() {
    for finalizer in [false, true] {
        let (mut engine, now) = setup();
        run(
            &mut engine,
            "var t=new Timer(function(e){throw 'cancelled event ran';},''); t.interval=10; t.enabled=true;",
        );
        let script = if finalizer {
            "class C { function finalize(){global.t.enabled=false;} } var c=new C; c=void; Gate.wait(false); 42;"
        } else {
            "Gate.wait(true); t.enabled=false; 42;"
        };
        let root = submit(&mut engine, script);
        let (_, waiting) = wait(&mut engine);
        assert!(engine.arm_wait(waiting, Duration::from_millis(10)));
        now.set(Duration::from_millis(11));
        let mut completed = false;
        for _ in 0..10000 {
            match step(&mut engine) {
                EngineEvent::Completed {
                    context,
                    result: RuntimeExit::Finished(value),
                } => {
                    assert_eq!(context, root);
                    assert_eq!(value.as_integer(), Some(42));
                    engine.take_result(context);
                    completed = true;
                }
                EngineEvent::Yielded => {}
                EngineEvent::Idle if completed => break,
                _ => panic!("cancelled timer must never be admitted"),
            }
        }
        assert!(completed);
        assert_eq!(engine.sleep_duration(), None);
    }
}

#[test]
fn callback_wait_modes_exclusivity_completion_roots_and_cancellation() {
    for (events, exclusive, cancel) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (true, true, true),
    ] {
        let (mut engine, now) = setup();
        run(
            &mut engine,
            &format!(
                r#"
            var trace='';
            var a=new Timer(function(e){{e.target.enabled=false; global.trace+='a'; return Gate.wait({events});}},'');
            var b=new Timer(function(e){{e.target.enabled=false; global.trace+='b';}},'');
            a.interval=10; a.mode={}; a.enabled=true; b.interval=20; b.enabled=true;
        "#,
                if exclusive { 1 } else { 0 }
            ),
        );
        now.set(Duration::from_millis(11));
        let (context, waiting) = wait(&mut engine);
        now.set(Duration::from_millis(21));
        if events && !exclusive {
            assert!(matches!(fire(&mut engine), RuntimeExit::Finished(_)));
        } else {
            assert_eq!(wait(&mut engine), (context, waiting));
        }
        if cancel {
            assert_eq!(engine.cancel(context)[0].wait.unwrap().0, waiting);
            assert!(!engine.complete(waiting, Ok(Value::Void)));
        } else {
            let result = Value::Str(
                engine
                    .runtime_mut()
                    .heap
                    .alloc_string("retained callback".encode_utf16().collect::<Vec<_>>()),
            );
            assert!(engine.complete(waiting, Ok(result)));
            engine.collect([]);
            let RuntimeExit::Finished(value) = fire(&mut engine) else {
                panic!("callback completion");
            };
            assert_eq!(
                engine.runtime().heap.display(value).unwrap(),
                "retained callback"
            );
        }
        if !events || exclusive {
            assert!(matches!(fire(&mut engine), RuntimeExit::Finished(_)));
        }
        assert_eq!(run(&mut engine, "trace;"), "ab");
        assert_eq!(engine.sleep_duration(), None);
    }
}

#[path = "../../krkr-assets/tests/support/mod.rs"]
mod support;
use krkr_engine::{
    Engine, EngineEvent, SystemEvent,
    assets::{self, Vfs, name::units},
    system::SystemConfig,
};
use std::{
    cell::Cell,
    num::NonZeroUsize,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value, WaitMode};
use tjs_runtime::{ContextId, Runtime, RuntimeExit, clock::Clock};

struct Manual(Rc<Cell<Duration>>);
impl Clock for Manual {
    fn now(&self) -> Duration {
        self.0.get()
    }
}
fn setup(vfs: Option<Vfs>) -> (Engine<Manual>, Rc<Cell<Duration>>) {
    let now = Rc::new(Cell::new(Duration::ZERO));
    let mut runtime = Runtime::new();
    krkr_engine::scripts::install(&mut runtime.heap).unwrap();
    if let Some(vfs) = vfs {
        krkr_engine::storages::install(&mut runtime.heap, vfs).unwrap();
    }
    let mut config = SystemConfig::for_process().unwrap();
    config.arguments.insert(units("-test"), units("yes"));
    let engine = Engine::with_system(
        runtime,
        Manual(now.clone()),
        Default::default(),
        Default::default(),
        config,
    )
    .unwrap();
    (engine, now)
}
fn submit(engine: &mut Engine<Manual>, script: &str) -> ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("system test", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("capacity"))
}
fn step(engine: &mut Engine<Manual>) -> EngineEvent {
    let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
    engine.collect([]);
    event
}
fn finished(engine: &mut Engine<Manual>, id: ContextId) -> String {
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        match step(engine) {
            EngineEvent::Completed { context, result } if context == id => {
                let RuntimeExit::Finished(value) = result else {
                    panic!("{result:?}")
                };
                let output = engine.runtime().heap.display(value).unwrap();
                engine.take_result(id);
                return output;
            }
            EngineEvent::Waiting { request, .. } => {
                assert!(engine.owns_wait(request));
                std::thread::park_timeout(Duration::from_millis(1));
            }
            EngineEvent::Yielded => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    panic!("script did not finish")
}
fn run(engine: &mut Engine<Manual>, script: &str) -> String {
    let id = submit(engine, script);
    finished(engine, id)
}
fn waiting(engine: &mut Engine<Manual>) -> (ContextId, tjs_runtime::WaitId, tjs_core::WaitRequest) {
    for _ in 0..10000 {
        match step(engine) {
            EngineEvent::Waiting {
                context,
                wait,
                request,
            } => return (context, wait, request),
            EngineEvent::Yielded => {}
            event => panic!("expected wait, got {event:?}"),
        }
    }
    panic!("no wait")
}
fn callback(engine: &mut Engine<Manual>) -> SystemEvent {
    for _ in 0..10000 {
        match step(engine) {
            EngineEvent::System {
                context,
                event,
                result,
            } => {
                assert!(matches!(result, RuntimeExit::Finished(_)), "{result:?}");
                engine.take_result(context);
                return event;
            }
            EngineEvent::Yielded => {}
            event => panic!("expected callback, got {event:?}"),
        }
    }
    panic!("no callback")
}

#[test]
fn system_values_arguments_uuid_and_compact_are_script_usable() {
    let (mut engine, now) = setup(None);
    now.set(Duration::from_millis(123456));
    assert_eq!(
        run(
            &mut engine,
            r#"
        var absent = System.getArgument('-absent') === void;
        System.setArgument('-number', 42);
        System.title = 'test title';
        var uuid = System.createUUID();
        var valid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(uuid);
        var live = %[answer:42]; System.doCompact(clAll);
        absent && System.getArgument('-test')=='yes' && System.getArgument('-number')==='42' &&
        System.getTickCount()==123456 && valid && uuid!=System.createUUID() &&
        System.title=='test title' && System.exeName.indexOf(System.exePath)==0 &&
        System.dataPath.length>0 && System.versionInformation.length>0 && System.processorNum>0 &&
        (System.exeBits==32 || System.exeBits==64) && live.answer==42;
    "#
        ),
        "1"
    );
    assert_eq!(engine.pending_operations(), 0);
    assert_eq!(
        String::from_utf16_lossy(&engine.system_config().title),
        "test title"
    );
    assert_eq!(
        run(
            &mut engine,
            "var caught=0; try {System.wait(-1);} catch(e){caught++;} try {System.wait(NaN);} catch(e){caught++;} caught;"
        ),
        "2"
    );
}

#[test]
fn event_wait_admits_timer_and_event_disabled_preserves_resume_order() {
    let (mut engine, now) = setup(None);
    run(
        &mut engine,
        "var seen=''; var t=new Timer(%[action:function(e){global.seen+='t'; e.target.enabled=false;}]); t.interval=10; t.enabled=true;",
    );
    let id = submit(&mut engine, "System.wait(100); seen+='r'; seen;");
    let (_, _, request) = waiting(&mut engine);
    assert_eq!(request.mode, WaitMode::Event);
    now.set(Duration::from_millis(20));
    loop {
        if let EngineEvent::Timer {
            context, result, ..
        } = step(&mut engine)
        {
            assert!(matches!(result, RuntimeExit::Finished(_)));
            engine.take_result(context);
            break;
        }
    }
    now.set(Duration::from_millis(100));
    assert_eq!(finished(&mut engine, id), "tr");
    run(
        &mut engine,
        "seen=''; t.enabled=true; System.eventDisabled=true;",
    );
    let id = submit(
        &mut engine,
        "System.wait(100); t.enabled=false; System.eventDisabled=false; seen+='r'; seen;",
    );
    waiting(&mut engine);
    now.set(Duration::from_millis(150));
    assert!(matches!(step(&mut engine), EngineEvent::Waiting { .. }));
    assert_eq!(engine.sleep_duration(), Some(Duration::from_millis(50)));
    now.set(Duration::from_millis(200));
    assert_eq!(finished(&mut engine, id), "r");
    assert_eq!(engine.pending_operations(), 0);
}

#[test]
fn continuous_handlers_keep_receivers_and_apply_edits_within_one_round() {
    let (mut engine, now) = setup(None);
    run(
        &mut engine,
        r#"
        var seen='', ticks=[];
        function b(tick){seen+='b'; ticks.add(tick); System.removeContinuousHandler(b);}
        function c(tick){seen+='c';}
        var holder=%[tag:'a', call:function(tick){global.seen+=this.tag; global.ticks.add(tick); global.System.removeContinuousHandler(this.call incontextof this); global.System.removeContinuousHandler(global.c); global.System.addContinuousHandler(global.b);}];
        System.addContinuousHandler(holder.call incontextof holder); System.addContinuousHandler(holder.call incontextof holder);
        System.addContinuousHandler(c); holder=void;
    "#,
    );
    now.set(Duration::from_millis(20));
    assert!(matches!(callback(&mut engine), SystemEvent::Continuous));
    now.set(Duration::from_millis(30));
    assert!(matches!(callback(&mut engine), SystemEvent::Continuous));
    // End the round before asking for the next host sleep.
    assert!(matches!(step(&mut engine), EngineEvent::Idle));
    assert_eq!(engine.sleep_duration(), None);
    assert_eq!(
        run(&mut engine, "seen=='ab' && ticks[0]==20 && ticks[1]==20;"),
        "1"
    );
    run(
        &mut engine,
        "System.onDeactivate=function(){global.seen+='d';}; System.onActivate=function(){global.seen+='a';}; System.eventDisabled=true;",
    );
    assert!(engine.set_active(false));
    assert!(engine.set_active(false));
    assert!(engine.set_active(true));
    assert!(matches!(step(&mut engine), EngineEvent::Idle));
    run(&mut engine, "System.eventDisabled=false;");
    assert!(matches!(callback(&mut engine), SystemEvent::Deactivate));
    assert!(matches!(callback(&mut engine), SystemEvent::Activate));
    assert_eq!(run(&mut engine, "seen;"), "abda");
}

#[test]
fn exit_never_returns_and_terminate_posts_a_host_exit() {
    let (mut engine, _) = setup(None);
    submit(
        &mut engine,
        "var after=0; try {System.exit('7'); after=1;} catch(e) {after=2;}",
    );
    for _ in 0..10000 {
        if let EngineEvent::Terminated(code) = step(&mut engine) {
            assert_eq!(code, 7);
            break;
        }
    }
    assert!(matches!(step(&mut engine), EngineEvent::Terminated(7)));
    assert_eq!(engine.pending_operations(), 0);
    engine.reset();
    assert_eq!(run(&mut engine, "after;"), "0");
    submit(&mut engine, "System.terminate(3); after=9;");
    assert!(matches!(
        engine.poll(
            RunBudget::new(1000).unwrap(),
            NonZeroUsize::new(64).unwrap()
        ),
        EngineEvent::Terminated(3)
    ));
    engine.reset();
    assert_eq!(run(&mut engine, "after;"), "9");
}

struct BlockingFilter {
    entered: mpsc::Sender<()>,
    release: Arc<Mutex<mpsc::Receiver<()>>>,
    remaining: Arc<AtomicUsize>,
}
impl assets::xp3::FilterFactory for BlockingFilter {
    fn create(
        &self,
        _: &[u16],
        _: &assets::xp3::Entry,
    ) -> assets::Result<Box<dyn assets::xp3::Filter>> {
        Ok(Box::new(Self {
            entered: self.entered.clone(),
            release: self.release.clone(),
            remaining: self.remaining.clone(),
        }))
    }
}
impl assets::xp3::Filter for BlockingFilter {
    fn apply(&mut self, _: u64, _: &mut [u8]) -> std::io::Result<()> {
        if self
            .remaining
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        Ok(())
    }
}

#[test]
fn worker_io_suspends_without_event_reentry_and_cancelled_results_stay_cancelled() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("data.xp3"),
        support::archive("answer.tjs", b"6*7;", true, true),
    )
    .unwrap();
    let (entered, entered_rx) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let blocks = Arc::new(AtomicUsize::new(2));
    let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    vfs.set_filter(Some(Arc::new(BlockingFilter {
        entered,
        release: Arc::new(Mutex::new(release_rx)),
        remaining: blocks.clone(),
    })));
    let (mut engine, now) = setup(Some(vfs));
    run(
        &mut engine,
        "var seen=''; var t=new Timer(%[action:function(e){global.seen+='t'; e.target.enabled=false;}]); t.interval=10; t.enabled=true;",
    );
    let id = submit(
        &mut engine,
        "var result=Scripts.evalStorage('data.xp3>answer.tjs'); t.enabled=false; seen+='r'; result;",
    );
    let (_, _, request) = waiting(&mut engine);
    assert_eq!(request.mode, WaitMode::Internal);
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    now.set(Duration::from_millis(20));
    for _ in 0..10 {
        assert!(matches!(step(&mut engine), EngineEvent::Waiting { .. }));
    }
    assert_eq!(engine.sleep_duration(), None);
    // Both raw and compressed segments pass through the extraction filter.
    release.send(()).unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release.send(()).unwrap();
    assert_eq!(finished(&mut engine, id), "42");
    assert_eq!(run(&mut engine, "seen;"), "r");
    assert_eq!(engine.pending_operations(), 0);

    blocks.store(1, Ordering::SeqCst);
    let id = submit(
        &mut engine,
        "Scripts.evalStorage('data.xp3>answer.tjs'); seen+='bad';",
    );
    let (_, old_wait, _) = waiting(&mut engine);
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    engine.cancel(id);
    assert_eq!(engine.pending_operations(), 0);
    let next = submit(&mut engine, "Scripts.evalStorage('data.xp3>answer.tjs');");
    let (_, next_wait, _) = waiting(&mut engine);
    assert_ne!(old_wait, next_wait);
    assert!(!engine.complete(old_wait, Ok(Value::Int(999))));
    release.send(()).unwrap();
    assert_eq!(finished(&mut engine, next), "42");
    assert_eq!(engine.pending_operations(), 0);
    assert_eq!(run(&mut engine, "seen;"), "r");

    let id = submit(&mut engine, "System.wait(1000); seen+='bad';");
    let (_, old_wait, _) = waiting(&mut engine);
    engine.cancel(id);
    assert_eq!(engine.pending_operations(), 0);
    assert!(!engine.complete(old_wait, Ok(Value::Int(42))));
    assert_eq!(run(&mut engine, "seen;"), "r");
}

#[test]
fn async_scripts_preserve_context_text_modes_bytecode_and_catchable_errors() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("context.tjs"), b"answer+1;").unwrap();
    std::fs::write(directory.path().join("bad.tjs"), b"var = ;").unwrap();
    std::fs::write(directory.path().join("throw.tjs"), b"throw 'from storage';").unwrap();
    std::fs::write(
        directory.path().join("packed.tjs"),
        assets::text::encode(&units("6*7;"), &units("z"), 4096).unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("broken.tjs"),
        [0xfe, 0xfe, 2, 0xff, 0xfe],
    )
    .unwrap();
    let vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let (mut engine, _) = setup(Some(vfs));
    assert_eq!(
        run(
            &mut engine,
            r#"
        var answer=Scripts.evalStorage('context.tjs', '', %[answer:41]);
        var packed=Scripts.evalStorage('packed.tjs');
        Scripts.compileStorage('packed.tjs', 'packed.bc', true, true, true);
        var bytecode=Scripts.evalStorage('packed.bc');
        var caught=0;
        try {Scripts.execStorage('bad.tjs');} catch(e){caught++;}
        try {Scripts.execStorage('throw.tjs');} catch(e){if(e=='from storage') caught++;}
        try {Scripts.execStorage('absent');} catch(e){caught++;}
        try {Scripts.execStorage('broken.tjs');} catch(e){caught++;}
        answer==42 && packed==42 && bytecode==42 && caught==4;
    "#
        ),
        "1"
    );
    assert_eq!(engine.pending_operations(), 0);
}

#[test]
fn automatic_gc_advances_across_idle_boundaries_and_explicit_compact_finishes() {
    let (mut engine, now) = setup(None);
    engine.collect([]);
    let baseline = engine.runtime().heap.counts();
    for _ in 0..10_000 {
        engine.runtime_mut().heap.alloc_object();
    }
    assert!(engine.collect_if_needed().is_none());
    assert!(engine.runtime().heap.is_collecting());
    assert_eq!(engine.sleep_duration(), Some(Duration::ZERO));
    // Beginning the cycle pays old allocation debt. It must keep progressing
    // even though the idle host performs no further managed allocations.
    assert_eq!(engine.runtime().allocation_debt(), 0);
    let mut completed = false;
    for _ in 0..1000 {
        if engine.collect_if_needed().is_some() {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert_eq!(engine.runtime().heap.counts(), baseline);
    assert!(!engine.runtime().heap.is_collecting());
    assert!(engine.collect_if_needed().is_none());

    // A small quiet allocation also starts a cycle on the idle deadline.
    engine.runtime_mut().heap.alloc_object();
    now.set(Duration::from_secs(2));
    engine.collect_if_needed();
    engine.collect([]);
    assert!(!engine.runtime().heap.is_collecting());
    assert_eq!(engine.runtime().heap.counts(), baseline);
}

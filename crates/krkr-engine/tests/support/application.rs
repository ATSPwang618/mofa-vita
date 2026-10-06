use super::*;
use tjs_core::Value;

fn module(engine: &mut Engine<Manual>, script: &str) -> tjs_core::Module {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("application", script)
        .unwrap();
    tjs_front::compile(&engine.runtime().sources, source)
        .unwrap_or_else(|e| panic!("{script}: {e}"))
}
fn start(engine: &mut Engine<Manual>, script: &str) -> ContextId {
    let module = module(engine, script);
    engine
        .start(&module)
        .unwrap_or_else(|_| panic!("startup rejected"))
}
fn finish(engine: &mut Engine<Manual>, host: &mut Desktop, id: ContextId, slice: u32) -> String {
    for _ in 0..10000 {
        match step(engine, host, slice) {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                let result = engine.runtime().heap.display(value).unwrap();
                engine.take_result(id);
                return result;
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("startup: {event:?}"),
        }
    }
    panic!("startup did not finish");
}
fn stopped(engine: &mut Engine<Manual>, host: &mut Desktop, slice: u32) -> i32 {
    for _ in 0..10000 {
        match step(engine, host, slice) {
            EngineEvent::Terminated(code) => {
                host.service();
                assert!(host.windows.is_empty());
                assert_eq!(engine.window_count(), 0);
                assert_eq!(engine.pending_operations(), 0);
                assert!(!engine.application_running());
                return code;
            }
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(_),
            }
            | EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            }
            | EngineEvent::Timer {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("exit: {event:?}"),
        }
    }
    panic!("application did not stop");
}
fn global(engine: &mut Engine<Manual>, name: &str) -> String {
    let global = engine.global();
    let heap = &mut engine.runtime_mut().heap;
    let key = heap.intern(&name.encode_utf16().collect::<Vec<_>>());
    heap.display(heap.member(global, key).unwrap().unwrap())
        .unwrap()
}
fn input(engine: &mut Engine<Manual>, host: &mut Desktop, id: WindowId) {
    host.host
        .post(Event {
            window: id,
            input: Input::Close,
        })
        .unwrap();
    for _ in 0..10000 {
        match step(engine, host, 1) {
            EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
                return;
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("close query: {event:?}"),
        }
    }
    panic!("close query did not finish");
}

#[test]
fn startup_policy_defaults_and_int32_setters_are_per_engine_and_survive_reset() {
    let (mut engine, mut host, _) = setup_with_exit_policy(true);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "System.exitOnWindowClose+System.exitOnNoWindowStartup;",
            1
        ),
        "2"
    );
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            r#"
        System.exitOnWindowClose=0.5; System.exitOnNoWindowStartup=4294967296;
        var n=System.exitOnWindowClose+System.exitOnNoWindowStartup;
        System.exitOnWindowClose='4294967297'; System.exitOnNoWindowStartup=-1;
        n+=10*System.exitOnWindowClose+100*System.exitOnNoWindowStartup;
        try{System.exitOnWindowClose=%[];}catch(e){n+=1000*System.exitOnWindowClose;}
        System.exitOnWindowClose=false; System.exitOnNoWindowStartup=false; n;
    "#,
            1
        ),
        "1110"
    );
    engine.reset();
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "System.exitOnWindowClose+System.exitOnNoWindowStartup;",
            1
        ),
        "0"
    );
    let (mut other, mut other_host, _) = setup_with_exit_policy(true);
    assert_eq!(
        run(
            &mut other,
            &mut other_host,
            "System.exitOnWindowClose+System.exitOnNoWindowStartup;",
            1
        ),
        "2"
    );
}

#[test]
fn no_window_startup_stops_once_but_disabled_policy_keeps_timer_application_alive() {
    for slice in [1, 10000] {
        let (mut engine, mut host, clock) = setup_with_exit_policy(true);
        let id = start(
            &mut engine,
            r#"
            var fired=0; var t=new Timer(%[action:function(e){global.fired++;global.System.terminate(7);}]);
            t.interval=50;t.enabled=true;42;
        "#,
        );
        assert!(engine.application_running());
        assert_eq!(finish(&mut engine, &mut host, id, slice), "42");
        assert_eq!(engine.sleep_duration(), Some(Duration::ZERO));
        assert_eq!(stopped(&mut engine, &mut host, slice), 0);
        assert_eq!(global(&mut engine, "fired"), "0");
        let code = module(&mut engine, "1;");
        assert!(engine.start(&code).is_err());
        assert!(engine.submit(&code).is_err());
        engine.reset();
        let id = start(
            &mut engine,
            r#"
            System.exitOnNoWindowStartup=false;
            t=new Timer(%[action:function(e){global.fired++;global.System.terminate(7);}]);
            t.interval=50;t.enabled=true;43;
        "#,
        );
        assert_eq!(finish(&mut engine, &mut host, id, slice), "43");
        assert!(engine.application_running());
        clock.0.set(Duration::from_millis(51)); // Timer fires strictly after its deadline.
        assert_eq!(stopped(&mut engine, &mut host, slice), 7);
        assert_eq!(global(&mut engine, "fired"), "1");
    }
}

#[test]
fn startup_waits_for_exception_handler_and_only_checks_window_count_at_completion() {
    let (mut engine, mut host, clock) = setup_with_exit_policy(false);
    let id = start(
        &mut engine,
        r#"
        var handled=0;
        System.exceptionHandler=function(e){System.wait(10);global.handled=e;global.w=new Window();return true;};
        throw 9;
    "#,
    );
    loop {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { .. } => break,
            event => panic!("{event:?}"),
        }
    }
    assert!(engine.application_running());
    assert_eq!(engine.window_count(), 0);
    clock.0.set(Duration::from_millis(10));
    assert_eq!(finish(&mut engine, &mut host, id, 1), "void");
    assert_eq!(global(&mut engine, "handled"), "9");
    assert_eq!(run(&mut engine, &mut host, "!w.visible;", 1), "1");
    assert_eq!(engine.window_count(), 1); // Hidden windows count too.
    run(&mut engine, &mut host, "w.close();", 1);
    assert_eq!(engine.window_count(), 0);
    assert!(engine.application_running());
    assert!(matches!(step(&mut engine, &mut host, 1), EngineEvent::Idle));
    let code = module(&mut engine, "42;");
    assert!(engine.start(&code).is_err());
}

#[test]
fn rejected_startup_error_and_cancelled_startup_are_not_reported_as_successful_exit() {
    let (mut engine, mut host, _) = setup_with_exit_policy(true);
    let id = start(&mut engine, "throw 23;");
    for _ in 0..10000 {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Thrown(_),
            } => {
                assert_eq!(context, id);
                engine.take_result(id);
                break;
            }
            EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    }
    assert!(!engine.application_running());
    engine.reset();
    let id = start(&mut engine, "System.wait(1000);42;");
    let wait = loop {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Waiting { wait, .. } => break wait,
            EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    };
    engine.cancel(id);
    assert!(!engine.application_running());
    assert_eq!(engine.pending_operations(), 0);
    assert!(!engine.complete(wait, Ok(Value::Void)));
    assert!(matches!(step(&mut engine, &mut host, 1), EngineEvent::Idle));
    engine.reset();
    let id = start(&mut engine, "42;");
    assert_eq!(finish(&mut engine, &mut host, id, 1), "42");
    assert_eq!(stopped(&mut engine, &mut host, 1), 0);
}

#[test]
fn user_close_respects_veto_and_secondary_hide_then_main_close_stops_remaining_windows() {
    let (mut engine, mut host, _) = setup_with_exit_policy(true);
    let id = start(
        &mut engine,
        r#"
        class Main extends Window {
            var allow=false;
            function Main(){super.Window();visible=true;}
            function onCloseQuery(b){super.onCloseQuery(allow);}
        }
        var w=new Main(),secondary=new Window();secondary.visible=true;42;
    "#,
    );
    assert_eq!(finish(&mut engine, &mut host, id, 1), "42");
    let mut ids: Vec<_> = host.windows.keys().copied().collect();
    ids.sort();
    input(&mut engine, &mut host, ids[1]);
    assert_eq!(
        run(
            &mut engine,
            &mut host,
            "isvalid secondary && !secondary.visible;",
            1
        ),
        "1"
    );
    input(&mut engine, &mut host, ids[0]);
    assert_eq!(engine.window_count(), 2);
    run(
        &mut engine,
        &mut host,
        "w.allow=true;System.eventDisabled=true;",
        1,
    );
    host.host
        .post(Event {
            window: ids[0],
            input: Input::Close,
        })
        .unwrap();
    assert!(matches!(step(&mut engine, &mut host, 1), EngineEvent::Idle));
    run(&mut engine, &mut host, "System.eventDisabled=false;", 1);
    assert_eq!(stopped(&mut engine, &mut host, 1), 0);
}

#[test]
fn main_unregisters_before_yielding_cleanup_and_exit_preserves_cleanup_and_later_exit_code() {
    for slice in [1, 10000] {
        let (mut engine, mut host, clock) = setup_with_exit_policy(true);
        run(
            &mut engine,
            &mut host,
            r#"
            var phase='',isMain=0;var w=new Window();
            class Item { function finalize(){
                global.phase+=(Window.mainWindow===null?'u':'bad');
                System.wait(10); global.phase+='done';
                global.created=new Window();global.isMain=Window.mainWindow===global.created;
                System.terminate(7);
            } }
            w.add(new Item());
        "#,
            slice,
        );
        submit(&mut engine, "w.close();");
        loop {
            match step(&mut engine, &mut host, slice) {
                EngineEvent::Yielded => {}
                EngineEvent::Waiting { .. } => break,
                event => panic!("{event:?}"),
            }
        }
        assert_eq!(engine.window_count(), 0);
        assert_eq!(global(&mut engine, "phase"), "u");
        let (replacement, _replacement_host) = Desktop::new();
        assert!(engine.attach_windows(replacement).is_err());
        assert!(matches!(
            step(&mut engine, &mut host, slice),
            EngineEvent::Waiting { .. }
        ));
        clock.0.set(Duration::from_millis(10));
        assert_eq!(stopped(&mut engine, &mut host, slice), 7);
        assert_eq!(global(&mut engine, "phase"), "udone");
        assert_eq!(global(&mut engine, "isMain"), "1");
        let root = engine.global();
        let heap = &mut engine.runtime_mut().heap;
        let key = heap.intern(&[119]);
        let Value::Obj(w) = heap.member(root, key).unwrap().unwrap() else {
            panic!("window");
        };
        assert!(!heap.is_valid(w.object.unwrap()).unwrap());
    }
}

#[test]
fn cancelling_main_cleanup_releases_exit_deferral_and_discards_late_wait_completion() {
    let (mut engine, mut host, _) = setup_with_exit_policy(true);
    run(
        &mut engine,
        &mut host,
        r#"
        var after=0;var w=new Window();
        class Item { function finalize(){System.wait(1000);global.after=1;} }
        w.add(new Item());
    "#,
        1,
    );
    let id = submit(&mut engine, "w.close();");
    let wait = loop {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Yielded => {}
            EngineEvent::Waiting { wait, .. } => break wait,
            event => panic!("{event:?}"),
        }
    };
    engine.cancel(id);
    assert!(!engine.complete(wait, Ok(Value::Void)));
    let code = module(&mut engine, "after=2;");
    assert!(engine.submit(&code).is_err());
    assert_eq!(stopped(&mut engine, &mut host, 1), 0);
    assert_eq!(global(&mut engine, "after"), "0");
    engine.reset();
    assert_eq!(run(&mut engine, &mut host, "after;", 1), "0");
}

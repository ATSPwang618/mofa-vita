use super::*;
use std::time::Duration;
fn setup() -> (Engine<Manual>, Desktop, Manual) {
    let clock = Manual::default();
    let mut engine = Engine::new(
        Runtime::new(),
        clock.clone(),
        SchedulerLimits {
            max_contexts: 3,
            max_event_depth: NonZeroUsize::new(2).unwrap(),
        },
        Default::default(),
    )
    .unwrap();
    let (client, mut host) = Desktop::new();
    engine.attach_windows(client).unwrap();
    run(
        &mut engine,
        &mut host,
        "System.exitOnWindowClose=false;",
        10000,
    );
    (engine, host, clock)
}

fn timed(engine: &mut Engine<Manual>, host: &mut Desktop, clock: &Manual, script: &str) -> String {
    let id = submit(engine, script);
    for _ in 0..20000 {
        let event = step(engine, host, 1);
        match event {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                let result = engine.runtime().heap.display(value).unwrap();
                engine.take_result(context);
                return result;
            }
            EngineEvent::Window {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } => {
                engine.take_result(context);
            }
            EngineEvent::Waiting { .. } | EngineEvent::Idle => {
                clock.0.set(clock.0.get() + Duration::from_millis(1))
            }
            EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    }
    panic!("transition script did not finish");
}
const BASE: &str = r#"
    var w=new Window(),r=new Layer(w,null),a=new Layer(w,r),b=new Layer(w,r);
    var x=new Layer(w,a),y=new Layer(w,b);
    a.visible=true;b.visible=false;a.left=4;b.left=9;
    var done=0,seenDest=null,seenSrc=null;
    w.action=function(e){if(e.type=='onTransitionCompleted'){
        global.done++;global.seenDest=e.dest;global.seenSrc=e.src;System.wait(0);
    }};
"#;

#[test]
fn cancelled_clock_tasks_release_the_queue_slot_and_stale_ticks_cannot_end_a_replacement() {
    let (mut engine, mut host, clock) = setup();
    run(&mut engine, &mut host, BASE, 1);
    run(
        &mut engine,
        &mut host,
        r#"
        var waitClock=true;
        a.beginTransition('crossfade',true,b,%[time:100,callback:function(){
            if(global.waitClock) System.wait(1000);return 100;
        }]);
    "#,
        1,
    );
    let mut waiting = None;
    for _ in 0..1000 {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Waiting { context, .. } => {
                waiting = Some(context);
                break;
            }
            EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    }
    engine.cancel(waiting.expect("clock callback wait"));
    clock.0.set(Duration::from_millis(16));
    assert_eq!(
        timed(
            &mut engine,
            &mut host,
            &clock,
            r#"
        waitClock=false;System.wait(30);
        if(done!=1) throw 'cancelled clock not retried';
        b.beginTransition('crossfade',true,a,%[time:100,callback:function(){throw 'stale callback';}]);
        b.stopTransition();
        a.beginTransition('crossfade',true,b,%[time:2]);System.wait(40);
        if(done!=3 || seenDest!==a || seenSrc!==b) throw 'replacement generation';
        'ok';
    "#
        ),
        "ok"
    );
    engine.reset();
}

#[test]
fn cancelled_window_cleanup_requeues_paint_and_clocks_and_closing_releases_active_transitions() {
    let (mut engine, mut host, clock) = setup();
    run(&mut engine, &mut host, BASE, 1);
    run(
        &mut engine,
        &mut host,
        r#"
        var pauseFinalize=true,paints=0,clockCalls=0;
        class Blocking {function finalize(){if(global.pauseFinalize) System.wait(1000);}}
        var blocker=new Blocking();w.add(blocker);
        a.onPaint=function(){global.paints++;};
    "#,
        1,
    );
    let closing = submit(
        &mut engine,
        r#"
        a.beginTransition('crossfade',true,b,%[time:100,selfupdate:true,callback:function(){global.clockCalls++;return 0;}]);
        a.update();w.close();
    "#,
    );
    let mut waiting = false;
    for _ in 0..1000 {
        match step(&mut engine, &mut host, 1) {
            EngineEvent::Waiting { context, .. } if context == closing => {
                waiting = true;
                break;
            }
            EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    }
    assert!(waiting);
    engine.cancel(closing);
    assert_eq!(
        timed(
            &mut engine,
            &mut host,
            &clock,
            r#"
        pauseFinalize=false;System.wait(20);
        if(paints!=1 || clockCalls!=1 || done!=0 || !isvalid w) throw 'cancelled window queue';
        w.close();a.stopTransition();
        if(done!=0 || isvalid w) throw 'orphan transition retained';
        'ok';
    "#
        ),
        "ok"
    );
    assert_eq!(engine.window_count(), 0);
    assert_eq!(engine.sleep_duration(), None);
    engine.reset();
}

#[test]
fn manual_transition_stop_swaps_pages_and_completion_can_start_the_next_transition() {
    for with_children in [true, false] {
        let (mut engine, mut host, _) = setup();
        run(&mut engine, &mut host, BASE, 1);
        let script = format!(
            r#"
            a.beginTransition('crossfade',{with_children},b,%[time:100]);
            var errors=0;
            try{{a.beginTransition('scroll',true,b,%[time:100]);}}catch(e){{errors++;}}
            try{{b.beginTransition('crossfade',true,a,%[time:100]);}}catch(e){{errors++;}}
            if(errors!=2 || done!=0) throw 'active and mutual validation';
            a.stopTransition();a.stopTransition();
            if(done!=1 || seenDest!==a || seenSrc!==b || a.visible || !b.visible || a.left!=9 || b.left!=4) throw 'completion state';
            if(r.children[0]!==b || r.children[1]!==a || x.parent!=={x_parent} || y.parent!=={y_parent}) throw 'completed tree';
            a.onTransitionCompleted=function(dest,src){{
                (global.Layer.onTransitionCompleted incontextof this)(dest,src);
                src.beginTransition('scroll',true,dest,%[time:100,from:sttTop,stay:ststStaySrc]);
            }};
            a.beginTransition('crossfade',true,b,%[time:100]);a.stopTransition();
            a.onTransitionCompleted=Layer.onTransitionCompleted incontextof a;
            b.stopTransition();if(done!=3) throw 'reentrant completion';
            var failed=0;
            try{{a.beginTransition('unknown',true,b,%[time:2]);}}catch(e){{failed++;}}
            try{{a.beginTransition('crossfade',true,b,%[]);}}catch(e){{failed++;}}
            try{{a.beginTransition('universal',true,b,%[time:2]);}}catch(e){{failed++;}}
            if(failed!=3) throw 'required options';
            'ok';
        "#,
            with_children = i32::from(with_children),
            x_parent = if with_children { "a" } else { "b" },
            y_parent = if with_children { "b" } else { "a" }
        );
        assert_eq!(run(&mut engine, &mut host, &script, 1), "ok");
        engine.reset();
    }
}

#[test]
fn automatic_completion_waits_for_events_and_selfupdate_waits_for_drawing() {
    let (mut engine, mut host, clock) = setup();
    run(&mut engine, &mut host, BASE, 1);
    assert_eq!(
        timed(
            &mut engine,
            &mut host,
            &clock,
            r#"
        System.eventDisabled=true;
        a.beginTransition('crossfade',true,b,%[time:2]);System.wait(40);
        if(done!=0 || !a.visible || b.visible) throw 'disabled completion';
        System.eventDisabled=false;System.wait(2);
        if(done!=1 || a.visible || !b.visible) throw 'deferred completion';
        var ticks=0,clockCalls=0,paints=0;
        b.onPaint=function(){global.paints++;System.wait(0);};
        b.beginTransition('scroll',false,a,%[time:100,selfupdate:true,callback:function(){global.clockCalls++;System.wait(0);return global.ticks;}]);
        System.wait(2);var initialCalls=clockCalls;
        ticks=100;System.wait(35);
        if(done!=1 || clockCalls!=initialCalls || initialCalls!=1) throw 'selfupdate idle';
        b.update();System.wait(20);
        if(done!=2 || paints!=1 || clockCalls!=2 || !a.visible || b.visible) throw 'selfupdate paint '+done+','+paints+','+clockCalls+','+a.visible+','+b.visible;
        'ok';
    "#
        ),
        "ok"
    );
    engine.reset();
}

#[test]
fn transition_options_callbacks_and_invalidation_share_the_original_vm() {
    let (mut engine, mut host, clock) = setup();
    run(&mut engine, &mut host, BASE, 1);
    assert_eq!(
        timed(
            &mut engine,
            &mut host,
            &clock,
            r#"
        var optionReads=0;
        class Options {
            property time {getter(){System.wait(0);global.optionReads++;return 100;}}
        }
        a.beginTransition('crossfade',true,b,new Options());
        invalidate b;
        if(done!=0 || a.visible || optionReads!=1) throw 'invalid source completion';
        b=new Layer(w,r);b.visible=true;
        b.beginTransition('crossfade',true,a,%[time:100,callback:function(){
            global.b.stopTransition();return 1000;
        }]);
        System.wait(3);
        if(done!=1) throw 'stop in clock callback';
        // Active destinations, sources and closures remain roots even when
        // only the native transition knows about their script objects.
        var temporary=new Layer(w,r);temporary.visible=true;
        temporary.beginTransition('crossfade',true,a,%[time:2]);temporary=null;
        System.wait(40);
        if(done!=2 || !isvalid seenDest || seenSrc!==a) throw 'transition roots';
        'ok';
    "#
        ),
        "ok"
    );
    engine.reset();
}

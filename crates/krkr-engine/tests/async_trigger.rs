#[path = "support/events.rs"]
mod support;
use krkr_engine::{EngineEvent, TimerLimits};
use std::time::Duration;
use support::*;
use tjs_core::Value;
use tjs_runtime::{Runtime, RuntimeExit};

#[test]
fn action_owner_returns_results_and_script_overrides_are_dispatched() {
    let (mut engine, _) = setup(Runtime::new(), Default::default());
    assert_eq!(
        run(
            &mut engine,
            r#"
        var receiver=%[], seen='';
        receiver.action=function(e){global.seen=e.type; return (this===global.receiver) && (e.target===global.a);};
        var a=new AsyncTrigger(receiver,void);
        var direct=a.onFire();
        var b=new AsyncTrigger(function(e){return e.target;},'');
        var valid=direct && seen=='onFire' && b.onFire()===b && a.cached && a.mode==atmNormal;
        class Derived extends AsyncTrigger {
            function Derived(){super.AsyncTrigger(null);}
            function onFire(){global.seen='override';return this;}
        }
        var derived=new Derived; derived.trigger(); valid;
    "#
        ),
        "1"
    );
    let EngineEvent::AsyncTrigger {
        context,
        owner,
        result: RuntimeExit::Finished(Value::Obj(value)),
    } = next(&mut engine)
    else {
        panic!("trigger callback");
    };
    assert_eq!(value.object, Some(owner));
    engine.take_result(context);
    assert_eq!(run(&mut engine, "seen;"), "override");
}

#[test]
fn cached_refresh_moves_to_tail_and_property_changes_cancel_pending() {
    let (mut engine, _) = setup(Runtime::new(), Default::default());
    run(
        &mut engine,
        r#"
        var trace='';
        var a=new AsyncTrigger(function(e){global.trace+='a';},'');
        var b=new AsyncTrigger(function(e){global.trace+='b';},'');
        a.trigger(); b.trigger(); a.trigger(); a.cached=true; a.mode=atmNormal;
    "#,
    );
    for _ in 0..2 {
        let EngineEvent::AsyncTrigger {
            context,
            result: RuntimeExit::Finished(_),
            ..
        } = next(&mut engine)
        else {
            panic!("queued trigger");
        };
        engine.take_result(context);
    }
    assert_eq!(run(&mut engine, "trace;"), "ba");
    for change in [
        "a.cancel();",
        "a.cached=false;",
        "a.mode=atmAtIdle;",
        "invalidate a;",
    ] {
        run(
            &mut engine,
            &format!("a=new AsyncTrigger(null); a.trigger(); {change}"),
        );
        assert!(matches!(next(&mut engine), EngineEvent::Idle));
    }
    run(&mut engine, "b.cached=false; b.trigger(); b.trigger();");
    for _ in 0..2 {
        let EngineEvent::AsyncTrigger {
            context,
            result: RuntimeExit::Finished(_),
            ..
        } = next(&mut engine)
        else {
            panic!("uncached trigger");
        };
        engine.take_result(context);
    }
    assert_eq!(run(&mut engine, "trace;"), "babb");
}

#[test]
fn priorities_share_fifo_with_timers_and_queued_owners_survive_gc() {
    let (mut engine, now) = setup(Runtime::new(), Default::default());
    run(
        &mut engine,
        r#"
        var trace='';
        var timer=new Timer(function(e){e.target.enabled=false;global.trace+='t';},'');
        timer.interval=1; timer.enabled=true;
        var normal=new AsyncTrigger(function(e){global.trace+='n';},'');
        var idle=new AsyncTrigger(function(e){global.trace+='i';},''); idle.mode=atmAtIdle;
        var exclusive=new AsyncTrigger(function(e){global.trace+='e';},''); exclusive.mode=atmExclusive;
        normal.trigger(); idle.trigger(); exclusive.trigger();
        normal=idle=exclusive=void;
    "#,
    );
    now.0.set(Duration::from_millis(2));
    for _ in 0..4 {
        let (context, owner) = match next(&mut engine) {
            EngineEvent::Timer {
                context,
                owner,
                result: RuntimeExit::Finished(_),
            }
            | EngineEvent::AsyncTrigger {
                context,
                owner,
                result: RuntimeExit::Finished(_),
            } => (context, owner),
            event => panic!("{event:?}"),
        };
        assert!(engine.runtime().heap.is_valid(owner).unwrap());
        engine.take_result(context);
    }
    assert_eq!(run(&mut engine, "trace;"), "enti");
    assert_eq!(engine.sleep_duration(), None);
}

#[test]
fn event_capacity_rejections_preserve_queue_and_cancellation_releases_budget() {
    let (mut engine, _) = setup(
        Runtime::new(),
        TimerLimits {
            max_timers: 1,
            max_pending_events: 2,
        },
    );
    assert_eq!(
        run(
            &mut engine,
            r#"
        var a=new AsyncTrigger(null), b=new AsyncTrigger(null), failures=0;
        a.cached=false; a.trigger();
        try{a.trigger();}catch(e){failures++;}
        try{new AsyncTrigger(null);}catch(e){failures++;}
        invalidate b; a.trigger(); a.cancel();
        b=new AsyncTrigger(null); failures;
    "#
        ),
        "2"
    );
    assert!(matches!(next(&mut engine), EngineEvent::Idle));
}

#[test]
fn self_reposting_events_do_not_starve_continuous_handlers() {
    for mode in ["atmNormal", "atmAtIdle"] {
        let (mut engine, _) = setup(Runtime::new(), Default::default());
        run(
            &mut engine,
            &format!(
                r#"
            var trace='', count=0;
            function update(tick){{trace+='c';System.removeContinuousHandler(update);}}
            System.addContinuousHandler(update);
            var trigger=new AsyncTrigger(function(e){{
                global.trace+='t';
                if(++global.count<3) e.target.trigger();
            }},'');
            trigger.mode={mode}; trigger.trigger();
        "#
            ),
        );
        for _ in 0..4 {
            let context = match next(&mut engine) {
                EngineEvent::AsyncTrigger {
                    context,
                    result: RuntimeExit::Finished(_),
                    ..
                }
                | EngineEvent::System {
                    context,
                    result: RuntimeExit::Finished(_),
                    ..
                } => context,
                event => panic!("{event:?}"),
            };
            engine.take_result(context);
        }
        assert_eq!(run(&mut engine, "trace;"), "tctt");
    }
}

#[test]
fn exclusive_trigger_blocks_nested_events_until_wait_finishes_or_is_cancelled() {
    for (exclusive, cancel) in [(false, false), (true, false), (true, true)] {
        let (mut engine, now) = setup(Runtime::new(), Default::default());
        run(
            &mut engine,
            &format!(
                r#"
            var trace='';
            var a=new AsyncTrigger(function(e){{global.trace+='a';System.wait(10);global.trace+='A';return 42;}},'');
            var b=new AsyncTrigger(function(e){{global.trace+='b';}},'');
            a.mode={}; a.trigger(); b.trigger();
        "#,
                if exclusive { 1 } else { 0 }
            ),
        );
        let EngineEvent::Waiting { context, wait, .. } = next(&mut engine) else {
            panic!("waiting callback");
        };
        if exclusive {
            assert!(matches!(next(&mut engine), EngineEvent::Waiting { .. }));
        } else {
            let EngineEvent::AsyncTrigger {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } = next(&mut engine)
            else {
                panic!("nested trigger");
            };
            engine.take_result(context);
        }
        if cancel {
            engine.cancel(context);
            assert!(!engine.complete(wait, Ok(Value::Void)));
        } else {
            now.0.set(Duration::from_millis(10));
            let EngineEvent::AsyncTrigger {
                context: finished,
                result: RuntimeExit::Finished(Value::Int(42)),
                ..
            } = next(&mut engine)
            else {
                panic!("resumed callback");
            };
            assert_eq!(finished, context);
            engine.take_result(context);
        }
        if exclusive {
            let EngineEvent::AsyncTrigger {
                context,
                result: RuntimeExit::Finished(_),
                ..
            } = next(&mut engine)
            else {
                panic!("pending trigger");
            };
            engine.take_result(context);
        }
        assert_eq!(
            run(&mut engine, "trace;"),
            if cancel {
                "ab"
            } else if exclusive {
                "aAb"
            } else {
                "abA"
            }
        );
    }
}

#[path = "support/events.rs"]
mod support;
use krkr_engine::{Engine, EngineEvent};
use std::{num::NonZeroUsize, time::Duration};
use support::*;
use tjs_core::Value;
use tjs_runtime::{Runtime, RuntimeExit, SchedulerLimits};

fn submit(engine: &mut Engine<Manual>, script: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("fault_source.tjs", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"))
}

#[test]
fn handles_uncaught_values_once_in_same_slot_and_preserves_rejected_results() {
    let mut engine = Engine::new(
        Runtime::new(),
        Manual::default(),
        SchedulerLimits {
            max_contexts: 1,
            max_event_depth: NonZeroUsize::new(1).unwrap(),
        },
        Default::default(),
    )
    .unwrap();
    assert_eq!(
        run(
            &mut engine,
            r#"
        var calls=0, seen=void, error=%[message:'original'];
        System.exceptionHandler=function(e){global.calls++;global.seen=e;return true;};
        throw error;
    "#
        ),
        "void"
    );
    assert_eq!(run(&mut engine, "calls==1 && seen===error;"), "1");
    assert_eq!(
        run(&mut engine, "try{throw 'caught';}catch(e){} calls;"),
        "1"
    );
    let id = submit(
        &mut engine,
        "System.exceptionHandler=function(e){global.calls++;return false;}; throw error;",
    );
    let EngineEvent::Completed {
        context,
        result: RuntimeExit::Thrown(error),
    } = next(&mut engine)
    else {
        panic!("original error");
    };
    assert_eq!(id, context);
    let RuntimeExit::Thrown(stored) = engine.take_result(context).unwrap() else {
        panic!("stored original error");
    };
    let (Value::Obj(stored), Value::Obj(error)) = (stored.value, error.value) else {
        panic!("original object");
    };
    assert_eq!(stored, error);
    assert_eq!(run(&mut engine, "calls;"), "2");
    for missing in [
        "System.exceptionHandler=null;",
        "delete System.exceptionHandler;",
        "System=void;",
    ] {
        let id = submit(&mut engine, &format!("{missing} throw 17;"));
        assert!(matches!(
            next(&mut engine),
            EngineEvent::Completed {
                result: RuntimeExit::Thrown(tjs_core::ScriptException {
                    value: Value::Int(17),
                    ..
                }),
                ..
            }
        ));
        engine.take_result(id);
    }
}

#[test]
fn handler_getter_constructor_trace_setter_and_handler_can_wait_with_gc() {
    let (mut engine, now) = setup(Runtime::new(), Default::default());
    let id = submit(
        &mut engine,
        r#"
        var seen=false, reads=0;
        function handler(e){
            System.wait(1);
            global.seen=(e instanceof 'Custom') && e.message.indexOf('member')>=0 && e.trace.indexOf('explode')>=0;
            return true;
        }
        property hook {getter {System.wait(1);global.reads++;return handler;}}
        &System.exceptionHandler=&hook;
        class Custom {
            var message='', savedTrace='';
            function Custom(msg){message=msg;System.wait(1);}
            property trace {
                getter {return savedTrace;}
                setter(value){System.wait(1);savedTrace=value;}
            }
        }
        Exception=Custom;
        function explode(){missing_member;}
        explode();
    "#,
    );
    for _ in 0..4 {
        let EngineEvent::Waiting {
            context, request, ..
        } = next(&mut engine)
        else {
            panic!("resumable exception stage");
        };
        assert_eq!(id, context);
        assert!(engine.owns_wait(request));
        now.0.set(now.0.get() + Duration::from_millis(1));
    }
    assert!(matches!(
        next(&mut engine),
        EngineEvent::Completed {
            result: RuntimeExit::Finished(Value::Void),
            ..
        }
    ));
    engine.take_result(id);
    assert_eq!(run(&mut engine, "seen && reads==1;"), "1");
}

#[test]
fn handler_failure_reports_both_errors_without_recursing() {
    let (mut engine, _) = setup(Runtime::new(), Default::default());
    let id = submit(
        &mut engine,
        "var calls=0; System.exceptionHandler=function(e){global.calls++; throw 'secondary';}; function primaryFailure(){throw 'primary';} primaryFailure();",
    );
    let EngineEvent::Completed {
        result: RuntimeExit::Fault(error),
        ..
    } = next(&mut engine)
    else {
        panic!("handler failure");
    };
    assert!(error.message.contains("primary") && error.message.contains("secondary"));
    assert_eq!(error.trace[0].function, "primaryFailure");
    assert_eq!(error.span, error.trace[0].span);
    assert!(error.message.contains("fault_source.tjs:1:"));
    assert!(
        error.trace.iter().filter_map(|f| f.span).all(|s| engine
            .runtime()
            .sources
            .get(s.source())
            .is_some())
    );
    engine.take_result(id);
    assert_eq!(run(&mut engine, "calls;"), "1");
}

#[test]
fn thrown_object_text_survives_a_failing_handler_without_mutating_the_value() {
    let (mut engine, _) = setup(Runtime::new(), Default::default());
    let id = submit(
        &mut engine,
        r#"
        var original=new Exception('missing startup asset', 'loader.tjs:42');
        System.exceptionHandler=function(e){
            global.same=(e===original);
            e.message='handler changed the object';
            throw new Exception('quick save unavailable');
        };
        function initialize(){throw original;}
        initialize();
        "#,
    );
    let EngineEvent::Completed {
        result: RuntimeExit::Fault(error),
        ..
    } = next(&mut engine)
    else {
        panic!("handler failure");
    };
    assert!(error.message.contains("missing startup asset"), "{error}");
    assert!(error.message.contains("loader.tjs:42"), "{error}");
    assert!(error.message.contains("quick save unavailable"), "{error}");
    assert!(!error.message.contains("handler changed"), "{error}");
    assert_eq!(error.trace[0].function, "initialize");
    engine.take_result(id);
    assert_eq!(run(&mut engine, "same;"), "1");
}

#[test]
fn exception_diagnostics_do_not_invoke_script_getters_or_missing_hooks() {
    let (mut engine, _) = setup(Runtime::new(), Default::default());
    let id = submit(
        &mut engine,
        r#"
        var reads=0;
        class CustomError {
            property message { getter { global.reads++; throw 'getter invoked'; } }
            function missing(get, name, value) { global.reads++; throw 'missing invoked'; }
        }
        throw new CustomError();
        "#,
    );
    let EngineEvent::Completed {
        result: RuntimeExit::Thrown(error),
        ..
    } = next(&mut engine)
    else {
        panic!("original thrown object");
    };
    assert!(error.diagnostic.message.contains("<object"));
    engine.take_result(id);
    assert_eq!(run(&mut engine, "reads;"), "0");
}

#[test]
fn event_failure_keeps_event_identity_and_cancel_drops_suspended_handler() {
    let (mut engine, now) = setup(Runtime::new(), Default::default());
    run(
        &mut engine,
        r#"
        var seen='';
        System.exceptionHandler=function(e){System.wait(1);global.seen=e;return true;};
        var trigger=new AsyncTrigger(function(e){throw 'event';},''); trigger.trigger();
    "#,
    );
    let EngineEvent::Waiting { context, .. } = next(&mut engine) else {
        panic!("event handler wait");
    };
    now.0.set(Duration::from_millis(1));
    let EngineEvent::AsyncTrigger {
        context: finished,
        result: RuntimeExit::Finished(Value::Void),
        ..
    } = next(&mut engine)
    else {
        panic!("handled event");
    };
    assert_eq!(context, finished);
    engine.take_result(context);
    assert_eq!(run(&mut engine, "seen;"), "event");
    let id = submit(&mut engine, "throw 'cancelled';");
    let EngineEvent::Waiting { context, wait, .. } = next(&mut engine) else {
        panic!("handler wait");
    };
    assert_eq!(id, context);
    engine.cancel(id);
    assert_eq!(engine.pending_operations(), 0);
    assert!(!engine.complete(wait, Ok(Value::Void)));
    assert_eq!(run(&mut engine, "seen;"), "event");
}

#[test]
fn rejected_exception_and_failed_finalizer_remain_owned_through_handling() {
    let (mut engine, now) = setup(Runtime::new(), Default::default());
    let id = submit(
        &mut engine,
        "System.exceptionHandler=function(e){e=void;System.wait(1);return false;}; throw %[marker:42];",
    );
    assert!(matches!(next(&mut engine), EngineEvent::Waiting { .. }));
    now.0.set(Duration::from_millis(1));
    let EngineEvent::Completed {
        result: RuntimeExit::Thrown(error),
        ..
    } = next(&mut engine)
    else {
        panic!("retained exception");
    };
    let Value::Obj(object) = error.value else {
        panic!("exception object");
    };
    let heap = &mut engine.runtime_mut().heap;
    let key = heap.intern(&"marker".encode_utf16().collect::<Vec<_>>());
    assert!(matches!(
        heap.member(object.object.unwrap(), key).unwrap(),
        Some(Value::Int(42))
    ));
    engine.take_result(id);
    run(
        &mut engine,
        "var finalized=0; System.exceptionHandler=function(e){global.finalized++;return true;}; class Final {function finalize(){throw 'finalizer';}} var f=new Final; f=void;",
    );
    let EngineEvent::Completed {
        context,
        result: RuntimeExit::Finished(Value::Void),
    } = next(&mut engine)
    else {
        panic!("handled finalizer");
    };
    engine.take_result(context);
    assert_eq!(run(&mut engine, "finalized;"), "1");
    assert!(matches!(next(&mut engine), EngineEvent::Idle));
}

#[test]
fn failing_continuous_handler_is_removed_before_exception_handler_readds_it() {
    let (mut engine, now) = setup(Runtime::new(), Default::default());
    run(
        &mut engine,
        r#"
        var calls=0, errors=0;
        function update(tick){if(++calls==1) throw 'continuous';System.removeContinuousHandler(update);}
        System.exceptionHandler=function(e){global.errors++;System.addContinuousHandler(update);return true;};
        System.addContinuousHandler(update);
    "#,
    );
    let EngineEvent::System {
        context,
        result: RuntimeExit::Finished(Value::Void),
        ..
    } = next(&mut engine)
    else {
        panic!("handled continuous error");
    };
    engine.take_result(context);
    now.0.set(Duration::from_millis(16));
    let EngineEvent::System {
        context,
        result: RuntimeExit::Finished(_),
        ..
    } = next(&mut engine)
    else {
        panic!("readded continuous handler");
    };
    engine.take_result(context);
    assert_eq!(run(&mut engine, "calls==2 && errors==1;"), "1");
    assert!(matches!(next(&mut engine), EngineEvent::Idle));
    assert_eq!(engine.sleep_duration(), None);
}

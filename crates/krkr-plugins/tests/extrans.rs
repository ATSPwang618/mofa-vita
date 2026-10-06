//! New provider lifetime/capability boundary; pixel behavior is exercised by
//! fixtures/extrans.tjs through the actual desktop renderer, not a mock kernel.
use krkr_engine::{
    Engine, EngineEvent,
    protocol::window::{self, Command, Geometry, Response},
};
use std::{
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
mod support;
type Runner = Engine<MonotonicClock>;
fn submit(engine: &mut Runner, script: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("extrans provider", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine.submit(&module).unwrap_or_else(|_| panic!("submit"))
}
fn step(engine: &mut Runner, host: &window::Host) -> EngineEvent {
    let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
    while let Some(request) = host.next_request() {
        if matches!(request.command, Command::Create { .. }) {
            request.complete(Ok(Geometry {
                width: 128,
                height: 80,
                inner_width: 128,
                inner_height: 80,
                ..Default::default()
            }));
        } else {
            request.respond(Ok(Response::Done));
        }
    }
    host.take_scenes(u64::MAX);
    engine.collect([]);
    event
}
fn run(engine: &mut Runner, host: &window::Host, script: &str) -> String {
    let id = submit(engine, script);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "provider scenario timeout");
        match step(engine, host) {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                let out = engine.runtime().heap.display(value).unwrap();
                engine.take_result(id);
                return out;
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            event => panic!("{event:?}"),
        }
    }
}
#[test]
fn capabilities_and_cancelled_option_getters_release_the_provider_with_gc() {
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(
        runtime,
        MonotonicClock::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    engine.attach_windows(client).unwrap();
    assert_eq!(
        run(
            &mut engine,
            &host,
            r#"
        Plugins.link('extrans.dll');System.exitOnWindowClose=false;
        var w=new Window(),root=new Layer(w,null),a=new Layer(w,root),b=new Layer(w,root);
        a.setImageSize(64,64);b.setImageSize(64,64);
        var caught=false;try {a.beginTransition('wave',false,b,%[time:100]);} catch(e) {caught=true;}
        if(!caught) throw 'missing kernel must fail at the caller';
        if(!Plugins.unlink('extrans.dll')) throw 'failed start retained provider';
        Plugins.link('extrans.dll');
        class Options { function Options() {} var time=100;
            property maxh { getter() {System.wait(10000);return 12;} }
        }
        'ready';
    "#
        ),
        "ready"
    );
    host.set_transition_kernels(["krkr.extrans.v1".into()]);
    let id = submit(
        &mut engine,
        "a.beginTransition('wave',false,b,new Options());throw 'cancelled task resumed';",
    );
    loop {
        match step(&mut engine, &host) {
            EngineEvent::Waiting { context, .. } if context == id => break,
            EngineEvent::Yielded => {}
            event => panic!("{event:?}"),
        }
    }
    assert_eq!(
        run(&mut engine, &host, "Plugins.unlink('extrans.dll');"),
        "0"
    );
    engine.cancel(id);
    engine.collect([]);
    assert_eq!(
        run(&mut engine, &host, "Plugins.unlink('extrans.dll');"),
        "1"
    );
}

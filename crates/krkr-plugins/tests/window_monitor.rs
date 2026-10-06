use krkr_engine::{
    Engine, EngineEvent,
    protocol::window::{self, Command, Geometry, Rectangle, Response, desktop},
};
use std::{num::NonZeroUsize, sync::Arc, time::Duration};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit};
mod support;

struct Clock;
impl tjs_runtime::clock::Clock for Clock {
    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

#[test]
fn monitor_query_accepts_kag_helpers_without_losing_window_selection() {
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(runtime, Clock, Default::default(), Default::default()).unwrap();
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    engine.attach_windows(client).unwrap();
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "KAG monitor helper",
            r#"
        System.exitOnWindowClose=false;
        Plugins.link('windowEx.dll');
        function check(v) { if(!v) throw 'monitor selection'; }
        class Helper {
            function monitor(near) { return System.getMonitorInfo(near, this); }
        }
        var h=new Helper();
        var primary=System.getMonitorInfo();
        check(primary.monitor.x===0);
        check(h.monitor(true).monitor.x===primary.monitor.x);
        check(h.monitor(false)===void);
        var w=new Window();
        check(System.getMonitorInfo(true,w).monitor.x===960);
        for(var i=0;i<2;i++) {
            var bad=i ? 123 : null, rejected=false;
            try {System.getMonitorInfo(true,bad);} catch(e) {rejected=true;}
            check(rejected);
        }
        invalidate w;
        var rejected=false;
        try {System.getMonitorInfo(true,w);} catch(e) {rejected=true;}
        check(rejected);
        42;
    "#,
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let mut queries = 0;
    for _ in 0..10_000 {
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            match &request.command {
                Command::Create { .. } => request.complete(Ok(Geometry::default())),
                Command::Desktop(desktop::Command::Monitor { target, .. }) => {
                    queries += 1;
                    let primary = matches!(target, desktop::MonitorTarget::Primary);
                    assert!(primary || matches!(target, desktop::MonitorTarget::Window(_)));
                    let bounds = Rectangle {
                        x: if primary { 0 } else { 960 },
                        y: 0,
                        width: 960,
                        height: 544,
                    };
                    request.respond(Ok(Response::Desktop(desktop::Response::Monitor(Some(
                        desktop::Monitor {
                            name: "fixture".into(),
                            primary,
                            monitor: bounds,
                            work: Some(bounds),
                        },
                    )))));
                }
                Command::Desktop(other) => panic!("unexpected desktop request: {other:?}"),
                _ => request.respond(Ok(Response::Done)),
            }
        }
        match event {
            EngineEvent::Yielded | EngineEvent::Waiting { .. } => {}
            EngineEvent::Completed {
                result: RuntimeExit::Finished(Value::Int(42)),
                ..
            } => {
                assert_eq!(queries, 3);
                return;
            }
            other => panic!("monitor fixture failed: {other:?}"),
        }
    }
    panic!("monitor fixture did not complete");
}

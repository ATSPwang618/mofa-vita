use krkr_engine::{
    Engine, EngineEvent,
    protocol::{
        self,
        graphics::{self, Adjustment},
        window::{Command, Geometry, Response},
    },
};
use std::{
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
mod support;

#[test]
fn filter_captures_and_payloads_survive_gc_and_release_on_failure_or_cancellation() {
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
    let (client, host) = protocol::window::channel(Default::default(), Arc::new(|| {}));
    engine.attach_windows(client).unwrap();
    let script = r#"
        Plugins.link('layerExImage.dll'); System.exitOnWindowClose=false;
        var w=new Window(), root=new Layer(w,null), a=new Layer(w,root);
        a.setImageSize(8,8); a.setClip(1,2,3,4); a.face=dfProvince;
        var after=false, caught=false, updates=0;
        Layer.update=function(l,t,width,height) {global.updates++;};
        try { a.light(10,0); } catch(e) { caught=true; }
        if(!caught || updates) throw 'failure did not return before redraw';
        a.colorize(0,0,1); after=true;
    "#;
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("image filters lifetime", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine.submit(&module).unwrap_or_else(|_| panic!("submit"));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut draws = 0;
    loop {
        assert!(Instant::now() < deadline);
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            if let Command::Graphics(graphics::Command::Adjust {
                rectangle,
                operation: Adjustment::Filter(filter),
                ..
            }) = &request.command
            {
                assert_eq!(
                    (
                        rectangle.left,
                        rectangle.top,
                        rectangle.width,
                        rectangle.height
                    ),
                    (1, 2, 3, 4)
                );
                assert_eq!(filter.table.as_slice().len(), 1024);
                let table = Arc::downgrade(&filter.table);
                draws += 1;
                if draws == 1 {
                    request.respond(Err("injected filter failure".into()));
                } else {
                    engine.cancel(id);
                    engine.collect([]);
                    assert!(request.cancelled());
                    assert_eq!(engine.pending_operations(), 0);
                    request.respond(Ok(Response::Done));
                }
                assert!(table.upgrade().is_none());
            } else if matches!(request.command, Command::Graphics(_)) {
                request.respond(Ok(Response::Done));
            } else {
                request.complete(Ok(Geometry {
                    width: 64,
                    height: 64,
                    inner_width: 64,
                    inner_height: 64,
                    ..Default::default()
                }));
            }
        }
        host.take_scenes(u64::MAX);
        if draws == 2 {
            break;
        }
        assert!(
            matches!(
                event,
                EngineEvent::Yielded | EngineEvent::Waiting { .. } | EngineEvent::Idle
            ),
            "{event:?}"
        );
    }
    let source = engine.runtime_mut().sources.add_utf8("after image filter cancellation", "if(after || !caught || updates) throw 'late completion resumed'; Plugins.unlink('layerExImage.dll'); invalidate w; 'passed';").unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine.submit(&module).unwrap_or_else(|_| panic!("submit"));
    loop {
        assert!(Instant::now() < deadline);
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            request.respond(Ok(Response::Done));
        }
        host.take_scenes(u64::MAX);
        match event {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } if context == id => {
                assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
                engine.take_result(id);
                break;
            }
            EngineEvent::Yielded | EngineEvent::Waiting { .. } | EngineEvent::Idle => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(host.staging_budget().used(), 0);
}

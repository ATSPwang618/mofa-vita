use krkr_engine::{
    Engine, EngineEvent,
    protocol::{
        self, graphics,
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
fn perspective_owned_requests_failure_and_cancellation() {
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
        Plugins.link('perspective.dll'); System.exitOnWindowClose=false;
        var w = new Window(), root = new Layer(w,null);
        var src = new Layer(w,root), dst = new Layer(w,root);
        src.setImageSize(8,8); dst.setImageSize(8,8);
        dst.setClip(0,0,0,0); dst.face=dfProvince; dst.holdAlpha=true;
        var after=false, caught=false;
        try { dst.perspectiveCopy(src,1,2,3,4,-1.2,0.9,6.8,1.8,0.1,6.9,7.8,7.9); }
        catch(e) { caught=true; }
        if (!caught) throw 'GPU failure did not reach original catch';
        dst.perspectiveCopy(src,1,2,3,4,-1.2,0.9,6.8,1.8,0.1,6.9,7.8,7.9);
        after=true;
    "#;
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("perspective request", script)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine.submit(&module).unwrap_or_else(|_| panic!("submit"));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut draws = 0;
    let cancelled = loop {
        assert!(Instant::now() < deadline);
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            if let Command::Graphics(graphics::Command::Perspective {
                image,
                source,
                mapping,
                clip,
            }) = &request.command
            {
                assert_eq!(mapping.source, [1.0, 2.0, 5.0, 7.0]);
                assert_eq!(
                    *clip,
                    graphics::Rect {
                        left: 0,
                        top: 0,
                        width: 7,
                        height: 7
                    }
                );
                assert_ne!(image.id, source.id);
                let permit = Arc::downgrade(&source.lifetime);
                assert!(permit.upgrade().is_some());
                draws += 1;
                if draws == 1 {
                    request.respond(Err("injected GPU failure".into()));
                } else {
                    engine.cancel(id);
                    engine.collect([]);
                    assert!(request.cancelled());
                    assert_eq!(engine.pending_operations(), 0);
                    // A late GPU completion cannot resume the cancelled script.
                    request.respond(Ok(Response::Done));
                    break;
                }
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
            break true;
        }
        assert!(
            matches!(
                event,
                EngineEvent::Yielded | EngineEvent::Waiting { .. } | EngineEvent::Idle
            ),
            "{event:?}"
        );
    };
    assert!(cancelled);
    let script = "if(after || !caught) throw 'cancelled operation resumed'; Plugins.unlink('perspective.dll'); invalidate w; 'passed';";
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("perspective after cancel", script)
        .unwrap();
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
            other => panic!("after cancel: {other:?}"),
        }
    }
    assert_eq!(host.staging_budget().used(), 0);
}

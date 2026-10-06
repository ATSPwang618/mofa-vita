use krkr_engine::{
    Engine, EngineEvent,
    assets::Vfs,
    protocol::{
        self,
        graphics::{Command as Graphics, Size},
        pixels::{Bytes, Pixels},
        window::{Command, Geometry, Response},
    },
};
use std::{
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};
mod support;
fn submit(engine: &mut Engine<MonotonicClock>, text: &str) -> tjs_runtime::ContextId {
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("layerExSave lifetime", text)
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine.submit(&module).unwrap_or_else(|_| panic!("submit"))
}
#[test]
fn background_snapshot_gc_and_cancelled_metadata_release_export_resources() {
    let directory = tempfile::tempdir().unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(directory.path(), Default::default()).unwrap(),
    )
    .unwrap();
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
    let mut id = submit(
        &mut engine,
        r#"
        Plugins.link('layerExSave.dll');System.exitOnWindowClose=false;
        var w=new Window(),root=new Layer(w,null),a=new Layer(w,root);a.setImageSize(4,4);
        var done=false,entered=false;
        w.onSaveLayerImageDone=function(id,canceled,layer,name){
            if(canceled || layer===global.a || layer.name!='saveLayer:done.png')throw 'snapshot callback';
            System.wait(10);global.done=true;
        };
        w.startSaveLayerImage(a,'done.png',void);
        while(!done)System.wait(50);
        class Tags{property reso_x {getter(){global.entered=true;System.wait(60000);return 123;}}}
        a.saveLayerImagePng('cancelled.png',new Tags());throw 'cancel resumed';
    "#,
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut cancelled = false;
    let mut readbacks = 0;
    loop {
        assert!(Instant::now() < deadline, "export lifecycle timeout");
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            match request.command {
                Command::Graphics(Graphics::ReadImage { .. }) => {
                    let mut main = Bytes::zeroed(4 * 4 * 4, &host.staging_budget()).unwrap();
                    main.as_mut_slice().fill(127);
                    readbacks += 1;
                    request.respond(Ok(Response::Image(Pixels {
                        size: Size {
                            width: 4,
                            height: 4,
                        },
                        main: Some(main),
                        province: None,
                    })));
                }
                Command::Graphics(_) => request.respond(Ok(Response::Done)),
                _ => request.complete(Ok(Geometry {
                    width: 64,
                    height: 64,
                    inner_width: 64,
                    inner_height: 64,
                    ..Default::default()
                })),
            }
        }
        host.take_scenes(u64::MAX);
        // An interned key alone is not a GC root before the script declares it.
        let entered = engine
            .runtime_mut()
            .heap
            .intern(&"entered".encode_utf16().collect::<Vec<_>>());
        if !cancelled
            && matches!(
                engine
                    .runtime()
                    .heap
                    .member(engine.global(), entered)
                    .unwrap(),
                Some(Value::Int(1))
            )
            && matches!(event, EngineEvent::Waiting { .. })
        {
            engine.cancel(id);
            engine.collect([]);
            assert_eq!(engine.pending_operations(), 0);
            assert_eq!(host.staging_budget().used(), 0);
            assert!(directory.path().join("done.png").exists());
            assert!(!directory.path().join("cancelled.png").exists());
            assert_eq!(readbacks, 2);
            id = submit(
                &mut engine,
                "if(!done || !Plugins.unlink('layerExSave.dll'))throw 'export lease leaked';invalidate w;'passed';",
            );
            cancelled = true;
            continue;
        }
        match event {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(v),
            } if context == id && cancelled => {
                assert_eq!(engine.runtime().heap.display(v).unwrap(), "passed");
                engine.take_result(id);
                break;
            }
            EngineEvent::System {
                result: RuntimeExit::Finished(_),
                ..
            }
            | EngineEvent::Yielded
            | EngineEvent::Waiting { .. }
            | EngineEvent::Idle => {}
            other => panic!("{other:?}"),
        }
        if matches!(event, EngineEvent::Waiting { .. } | EngineEvent::Idle) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(host.staging_budget().used(), 0);
}

use krkr_engine::{
    Engine, EngineEvent,
    assets::{Limits, Vfs},
    protocol::{
        self,
        window::{Command, Geometry, IconCommand, IconImage, WindowId},
    },
};
use std::{
    collections::HashMap,
    fs,
    num::NonZeroUsize,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tjs_core::RunBudget;
use tjs_runtime::{Runtime, RuntimeExit};
mod support;

/// This observer consumes real decoded IO requests. It does not claim native
/// rendering evidence: the same fixtures/icons.tjs is the desktop scenario.
#[test]
fn icon_selection_io_failure_reset_and_application_ownership() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("icons")).unwrap();
    fs::write(
        directory.path().join("icons/red.ico"),
        include_bytes!("fixtures/icons/red.ico"),
    )
    .unwrap();
    fs::write(
        directory.path().join("icons/blue-pe.bin"),
        include_bytes!("fixtures/icons/blue-pe.bin"),
    )
    .unwrap();
    fs::write(
        directory.path().join("icons/bad.ico"),
        [0, 0, 1, 0, 255, 255],
    )
    .unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(directory.path(), Limits::default()).unwrap(),
    )
    .unwrap();
    krkr_plugins::register(&mut runtime.heap).unwrap();
    let mut engine = Engine::new(
        runtime,
        tjs_runtime::clock::MonotonicClock::default(),
        Default::default(),
        Default::default(),
    )
    .unwrap();
    let (client, host) = protocol::window::channel(Default::default(), Arc::new(|| {}));
    let budget = host.staging_budget();
    engine.attach_windows(client).unwrap();
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("icon flow", include_str!("fixtures/icons.tjs"))
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let mut windows: HashMap<WindowId, (Weak<AtomicBool>, Option<Arc<IconImage>>)> = HashMap::new();
    let mut application: Option<Arc<IconImage>> = None;
    let mut observed = Vec::new();
    let mut images = Vec::new();
    let mut inherited = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "icon scenario timed out");
        let event = engine.poll(RunBudget::new(1).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        windows.retain(|_, (alive, _)| {
            alive
                .upgrade()
                .is_some_and(|alive| alive.load(Ordering::Acquire))
        });
        while let Some(request) = host.next_request() {
            if request.cancelled() {
                continue;
            }
            match &request.command {
                Command::Create { alive, .. } => {
                    inherited.push(color(&application));
                    windows.insert(request.window, (alive.clone(), None));
                }
                Command::Icon(command) => {
                    match command {
                        IconCommand::Window {
                            image,
                            with_application,
                        } => {
                            // Reset must reuse the same selected immutable image.
                            if observed.len() == 2 {
                                assert!(Arc::ptr_eq(
                                    image.as_ref().unwrap(),
                                    windows[&request.window].1.as_ref().unwrap()
                                ));
                            }
                            observed.push((false, color(image), *with_application));
                            windows.get_mut(&request.window).unwrap().1 = image.clone();
                            if *with_application {
                                application = image.clone();
                            }
                            if let Some(image) = image {
                                images.push(Arc::downgrade(image));
                            }
                        }
                        IconCommand::Application(image) => {
                            observed.push((true, color(image), false));
                            application = image.clone();
                            if let Some(image) = image {
                                images.push(Arc::downgrade(image));
                            }
                        }
                    }
                    request.respond(Ok(protocol::window::Response::Done));
                    continue;
                }
                _ => {}
            }
            request.complete(Ok(Geometry {
                width: 640,
                height: 480,
                inner_width: 640,
                inner_height: 480,
                ..Default::default()
            }));
        }
        match event {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(value),
            } => {
                assert_eq!(engine.runtime().heap.display(value).unwrap(), "passed");
                engine.take_result(context);
                break;
            }
            EngineEvent::Yielded | EngineEvent::Idle | EngineEvent::Waiting { .. } => {
                std::thread::yield_now()
            }
            other => panic!("icon scenario failed: {other:?}"),
        }
    }
    // 1 = red ICO, 2 = first PE group, 0 = backend default. Failed loads do
    // not post an application update; window failure discards the selection.
    assert_eq!(
        observed,
        [
            (true, 2, false),
            (false, 1, true),
            (false, 1, false),
            (false, 0, false),
            (false, 2, false),
            (false, 0, false),
            (false, 1, false),
            (false, 0, false),
            (false, 0, true),
            (false, 1, true),
            (false, 0, false),
            (false, 0, false),
            (true, 2, false),
            (false, 1, false),
            (true, 0, false),
        ]
    );
    assert_eq!(inherited, [2, 1]);
    assert!(application.is_none());
    drop(windows);
    drop(engine);
    assert!(images.iter().all(|image| image.strong_count() == 0));
    assert_eq!(budget.used(), 0, "icon decode/selection leases leaked");
}
fn color(image: &Option<Arc<IconImage>>) -> u8 {
    let Some(image) = image else {
        return 0;
    };
    assert_eq!((image.width, image.height), (32, 32));
    assert_eq!(image.rgba.as_slice()[3], 0, "DIB AND mask transparency");
    match &image.rgba.as_slice()[4..8] {
        [255, 0, 0, 255] => 1,
        [0, 0, 255, 255] => 2,
        pixel => panic!("unexpected icon pixel {pixel:?}"),
    }
}

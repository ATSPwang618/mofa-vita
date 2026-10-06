mod support;
use krkr_engine::{
    Engine, EngineEvent,
    assets::Vfs,
    protocol::{
        self,
        graphics::Command as Graphics,
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
#[test]
fn xml_qr_spectrum_and_legacy_contracts() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("source.xml"), "<r>from storage</r>").unwrap();
    let mut runtime = Runtime::new();
    krkr_engine::install(&mut runtime, support::Log).unwrap();
    krkr_engine::storages::install(
        &mut runtime.heap,
        Vfs::new(dir.path(), Default::default()).unwrap(),
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
    let id = engine
        .runtime_mut()
        .sources
        .add_utf8(
            "XML QR and legacy plugins",
            include_str!("fixtures/xml_qr_legacy.tjs"),
        )
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, id).unwrap();
    let root = engine
        .submit(&module)
        .unwrap_or_else(|_| panic!("context capacity"));
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut qr = 0;
    let mut fft = 0;
    let mut copies = 0;
    loop {
        assert!(Instant::now() < deadline, "plugin scenario timeout");
        let event = engine.poll(RunBudget::new(64).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            match &request.command {
                Command::Graphics(Graphics::ReadImage { .. }) => {
                    panic!("QR/FFT must not read back the destination")
                }
                Command::Graphics(Graphics::PatchPixels { pixels, .. }) => {
                    let data = pixels.main.as_ref().unwrap().as_slice();
                    let w = pixels.size.width as usize;
                    assert!(
                        data[..w * 4 * 4].iter().all(|&b| b == 255),
                        "QR white margin"
                    );
                    assert_eq!(
                        &data[(4 * w + 4) * 4..(4 * w + 4) * 4 + 4],
                        &[0, 0, 0, 255],
                        "QR finder corner"
                    );
                    qr += 1;
                    request.respond(Ok(Response::Done));
                }
                Command::Graphics(Graphics::CopyPixels { pixels, .. }) => {
                    assert_eq!(pixels.size.width, 64);
                    assert_eq!(pixels.size.height, 32);
                    let data = pixels.main.as_ref().unwrap().as_slice();
                    if fft == 0 {
                        assert_eq!(&data[..4], &[0; 4]);
                        assert_eq!(&data[31 * 64 * 4..31 * 64 * 4 + 4], &[128, 128, 128, 255]);
                    } else {
                        assert_eq!(&data[31 * 64 * 4..31 * 64 * 4 + 4], &[192, 192, 192, 255]);
                    }
                    fft += 1;
                    request.respond(Ok(Response::Done));
                }
                Command::Graphics(Graphics::Copy { .. }) => {
                    copies += 1;
                    request.respond(Ok(Response::Done));
                }
                Command::Graphics(_) => request.respond(Ok(Response::Done)),
                _ => request.complete(Ok(Geometry {
                    width: 64,
                    height: 32,
                    inner_width: 64,
                    inner_height: 32,
                    ..Default::default()
                })),
            }
        }
        host.take_scenes(u64::MAX);
        match event {
            EngineEvent::Completed {
                context,
                result: RuntimeExit::Finished(Value::Int(42)),
            } if context == root => break,
            EngineEvent::Completed { result, .. } => panic!("plugin scenario failed: {result:?}"),
            EngineEvent::Waiting { .. } => std::thread::sleep(Duration::from_millis(1)),
            _ => {}
        }
    }
    assert_eq!((qr, fft, copies), (2, 2, 2));
}

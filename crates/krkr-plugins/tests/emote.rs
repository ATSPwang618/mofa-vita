mod support;
use krkr_engine::{
    Engine, EngineEvent,
    assets::Vfs,
    protocol::{
        self,
        graphics::{Command as Graphics, ImageId},
        window::{Command, Geometry, Response},
    },
};
use krkr_render_wgpu::{Gpu, Image};
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::Arc,
    time::{Duration, Instant},
};
use tjs_core::{RunBudget, Value};
use tjs_runtime::{Runtime, RuntimeExit, clock::MonotonicClock};

#[test]
#[ignore = "requires a real desktop GPU"]
fn motion_and_draw_device_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("emote.mdf"),
        include_bytes!("fixtures/emote.mdf"),
    )
    .unwrap();
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
    let source = engine
        .runtime_mut()
        .sources
        .add_utf8("E-mote pipeline", include_str!("fixtures/emote.tjs"))
        .unwrap();
    let module = tjs_front::compile(&engine.runtime().sources, source).unwrap();
    let id = engine.submit(&module).unwrap_or_else(|_| panic!("submit"));
    let gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    let mut images: HashMap<ImageId, Image> = HashMap::new();
    let (mut rendered, mut presented) = (false, false);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(Instant::now() < deadline, "E-mote pipeline timeout");
        let event = engine.poll(RunBudget::new(100).unwrap(), NonZeroUsize::new(64).unwrap());
        engine.collect([]);
        while let Some(request) = host.next_request() {
            let response = match &request.command {
                Command::Graphics(command) => match command {
                    Graphics::Create {
                        image, size, color, ..
                    } => {
                        images.insert(*image, gpu.create_image(*size, *color).unwrap());
                        Response::Done
                    }
                    Graphics::Resize { image, size, .. }
                    | Graphics::EnableImage { image, size, .. } => {
                        images.insert(image.id, gpu.create_image(*size, 0).unwrap());
                        Response::Done
                    }
                    Graphics::Meshes { image, size, batch } => {
                        let prepared = gpu.prepare_meshes(batch, &images).unwrap();
                        if !images.get(&image.id).is_some_and(|i| i.size == *size) {
                            images.insert(image.id, gpu.create_image(*size, 0).unwrap());
                        }
                        gpu.draw_meshes(images.get_mut(&image.id).unwrap(), batch, prepared)
                            .unwrap();
                        rendered |= pixels(&gpu, &images[&image.id])
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .any(|p| p[0] > 200 && p[1] < 10 && p[2] < 10 && p[3] > 200);
                        Response::Done
                    }
                    Graphics::SnapshotMain { image, source } => {
                        let snapshot = images[&source.id].shared_main();
                        images.insert(image.id, snapshot);
                        Response::Done
                    }
                    Graphics::Assign { image, source } => {
                        let snapshot = images[&source.id].shared();
                        images.insert(image.id, snapshot);
                        Response::Done
                    }
                    Graphics::Copy {
                        image,
                        source,
                        rectangle,
                        x,
                        y,
                        clip,
                        face,
                        hold_alpha,
                    } => {
                        let source = images[&source.id].source();
                        gpu.copy_rect(
                            images.get_mut(&image.id).unwrap(),
                            &source,
                            *rectangle,
                            *x,
                            *y,
                            *clip,
                            *face,
                            *hold_alpha,
                        )
                        .unwrap();
                        Response::Done
                    }
                    Graphics::ComposeScene { image, size, scene } => {
                        let mut target = images
                            .remove(&image.id)
                            .unwrap_or_else(|| gpu.create_image(*size, 0).unwrap());
                        gpu.compose(&mut target, scene, &images).unwrap();
                        images.insert(image.id, target);
                        Response::Done
                    }
                    Graphics::Fill { image, fills } => {
                        gpu.fill(images.get_mut(&image.id).unwrap(), fills).unwrap();
                        Response::Done
                    }
                    Graphics::Independ {
                        image,
                        province,
                        copy,
                    } => {
                        gpu.independ_image(images.get_mut(&image.id).unwrap(), *province, *copy)
                            .unwrap();
                        Response::Done
                    }
                    Graphics::ReadImage { .. }
                    | Graphics::PatchPixels { .. }
                    | Graphics::Upload { .. }
                    | Graphics::AssignBitmap { .. } => {
                        panic!("plugin frame unexpectedly crossed the CPU pixel boundary")
                    }
                    other => panic!("unhandled graphics {other:?}"),
                },
                _ => Response::Geometry(Geometry {
                    width: 32,
                    height: 32,
                    ..Default::default()
                }),
            };
            request.respond(Ok(response));
        }
        for (_, scene) in host.take_scenes(u64::MAX) {
            if scene.nodes.len() == 1
                && let Some(image) = &scene.nodes[0].image
            {
                presented |= images.get(&image.id).is_some_and(|image| {
                    pixels(&gpu, image)
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|p| p[0] > 150 && p[3] > 150)
                });
            }
        }
        match event {
            EngineEvent::Completed {
                context: finished,
                result: RuntimeExit::Finished(Value::Int(42)),
            } if finished == id => {
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
        std::thread::yield_now();
    }
    assert!(rendered, "E-mote produced no red source pixels");
    assert!(presented, "device never published its composed pixels");
}

fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(Instant::now() < deadline, "GPU readback timeout");
        std::thread::sleep(Duration::from_millis(1));
    }
}

use super::*;
use crate::gles_test_support as support;
use crate::memory::MIB;
use krkr_protocol::{
    budget::Budget,
    graphics::{Fill, Node, Size},
    image_cache::{Entry, Key},
    pixels::{Bytes, Pixels},
};
use std::{cell::Cell, ffi::c_void};
#[path = "../../../render-gles2/tests/support/traffic.rs"]
mod traffic;

#[test]
fn admission_recalculates_copy_on_write_after_releasing_a_cached_alias() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let target = image(&mut graphics, &mut ids, 73);
    let unrelated = image(&mut graphics, &mut ids, 90);
    let alias = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    graphics
        .execute(&Command::Assign {
            image: alias.clone(),
            source: target.clone(),
        })
        .unwrap();
    let key = cached(&graphics, alias, "copy-on-write-owner");
    graphics.gpu.collect().unwrap();
    let command = Command::Fill {
        image: target.clone(),
        fills: vec![Fill {
            rectangle: Size {
                width: 512,
                height: 512,
            }
            .rect(),
            color: 0xff123456,
            face: DrawFace::Opaque,
            hold_alpha: true,
        }],
    };
    assert!(graphics.command_allocation(&command).unwrap() >= MIB);
    let lock = graphics
        .gpu
        .resident
        .reserve(graphics.gpu.resident.available() - 64 * 1024)
        .unwrap();
    graphics.execute(&command).unwrap();
    assert!(graphics.cache.get(&key).is_none());
    assert!(
        !graphics.spilled.contains_key(&unrelated.id),
        "released sharing must not park an unrelated canvas"
    );
    assert_eq!(graphics.command_allocation(&command).unwrap(), 0);
    drop(lock);
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&target).unwrap(), 4, 4, false)
            .unwrap(),
        0xff123456
    );
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&unrelated).unwrap(), 4, 4, false)
            .unwrap(),
        0xff045a21
    );
}

#[test]
fn scanline_admission_reclaims_cache_for_table_and_backdrop_before_drawing() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let target = image(&mut graphics, &mut ids, 100);
    let source = image(&mut graphics, &mut ids, 60);
    let cold = image(&mut graphics, &mut ids, 80);
    let key = cached(&graphics, cold, "cold-scanline-input");
    graphics.gpu.collect().unwrap();
    let size = Size {
        width: 512,
        height: 512,
    };
    let mut words = vec![0, 512, 3, 512, 512, 0, 0, 0];
    for y in 0..512 {
        words.extend_from_slice(&[0, 512, 0, y, 0, 0, 0, 0]);
    }
    let rows = krkr_protocol::scanlines::Scanlines {
        rectangle: size.rect(),
        _permit: graphics.gpu.staging.reserve(words.len() * 4).unwrap(),
        words,
    };
    let command = Command::Scanlines {
        image: target.clone(),
        source: source.clone(),
        rows: Arc::new(rows),
    };
    assert!(graphics.command_allocation(&command).unwrap() >= 512 * 1024);
    let lock = graphics
        .gpu
        .resident
        .reserve(graphics.gpu.resident.available() - 64 * 1024)
        .unwrap();
    graphics.execute(&command).unwrap();
    assert!(graphics.cache.get(&key).is_none());
    drop(lock);
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&target).unwrap(), 4, 4, false)
            .unwrap(),
        0xff045021
    );
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&source).unwrap(), 4, 4, false)
            .unwrap(),
        0xff043c21
    );
}

#[test]
fn color_grants_flush_before_reads_and_execute_with_staging_full() {
    use krkr_protocol::window::{self, Command as W};
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut images = slotmap::SlotMap::with_key();
    let target = image(&mut graphics, &mut images, 72);
    let expected = image(&mut graphics, &mut images, 72);
    let mut windows = slotmap::SlotMap::with_key();
    let id = windows.insert(());
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    let color = |image: &ImageRef, opacity| Command::Color {
        image: image.clone(),
        rectangle: Size {
            width: 512,
            height: 512,
        }
        .rect(),
        color: 0x7c335da1,
        opacity,
        face: DrawFace::Opaque,
    };
    let ticket = client.request(id, W::Graphics(color(&target, 37))).unwrap();
    let request = host.next_request().unwrap();
    let W::Graphics(command) = &request.command else {
        panic!()
    };
    let response = graphics.execute(command).unwrap();
    request.offer_draws(graphics.prepare_draws(command).unwrap());
    request.respond(Ok(response));
    assert!(ticket.take().unwrap().is_ok());
    graphics.execute(&color(&expected, 37)).unwrap();
    for opacity in [53, 128, -24] {
        assert!(client.try_draw(id, &color(&target, opacity)).unwrap());
        graphics.execute(&color(&expected, opacity)).unwrap();
    }
    assert!(
        !client.try_draw(id, &color(&target, 255)).unwrap(),
        "a clear must close the color-only grant"
    );
    let read = client
        .request(
            id,
            W::Graphics(Command::Pixel {
                image: target.clone(),
                x: 4,
                y: 4,
                province: false,
            }),
        )
        .unwrap();
    let batch = host.next_request().unwrap();
    let W::Graphics(command @ Command::PreparedDraw(_)) = &batch.command else {
        panic!("read must flush admitted colors first")
    };
    let pressure = graphics
        .gpu
        .staging
        .reserve(graphics.gpu.staging.available())
        .unwrap();
    let response = graphics.execute(command);
    batch.respond(response);
    drop(pressure);
    let request = host.next_request().unwrap();
    let W::Graphics(command) = &request.command else {
        panic!()
    };
    let response = graphics.execute(command);
    request.respond(response);
    let Response::Pixel(actual) = read.take().unwrap().unwrap() else {
        panic!()
    };
    assert_eq!(
        actual,
        graphics
            .gpu
            .pixel(graphics.image(&expected).unwrap(), 4, 4, false)
            .unwrap()
    );
    assert!(host.next_request().is_none());
}

#[test]
fn parked_assignments_share_pixels_without_restoring_and_detach_on_write() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let source = image(&mut graphics, &mut ids, 73);
    let saved = std::rc::Rc::new(
        graphics
            .gpu
            .spill_canvas(graphics.image(&source).unwrap())
            .unwrap()
            .unwrap(),
    );
    graphics.images.remove(&source.id);
    graphics.spilled.insert(source.id, saved.clone());
    graphics.gpu.collect().unwrap();
    let first = image(&mut graphics, &mut ids, 80);
    let second = image(&mut graphics, &mut ids, 90);
    let lock = graphics
        .gpu
        .staging
        .reserve(graphics.gpu.staging.available())
        .unwrap();
    for command in [
        Command::Assign {
            image: first.clone(),
            source: source.clone(),
        },
        Command::SnapshotMain {
            image: second.clone(),
            source: source.clone(),
        },
        Command::Assign {
            image: source.clone(),
            source: source.clone(),
        },
    ] {
        graphics.execute(&command).unwrap();
    }
    for id in [source.id, first.id, second.id] {
        assert!(std::rc::Rc::ptr_eq(&graphics.spilled[&id], &saved));
        assert!(!graphics.images.contains_key(&id));
    }
    drop(lock);
    graphics
        .execute(&Command::Fill {
            image: first.clone(),
            fills: vec![Fill {
                rectangle: Size {
                    width: 512,
                    height: 512,
                }
                .rect(),
                color: 0xff123456,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        })
        .unwrap();
    graphics.ensure_images(&[second.id]).unwrap();
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&first).unwrap(), 4, 4, false)
            .unwrap(),
        0xff123456
    );
    for reference in [source, second] {
        assert_eq!(
            graphics
                .gpu
                .pixel(graphics.image(&reference).unwrap(), 4, 4, false)
                .unwrap(),
            0xff044921
        );
    }
}

#[test]
fn pressure_parks_distinct_planes_sharing_pixels_as_one_batch() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let source = image(&mut graphics, &mut ids, 73);
    let view = image(&mut graphics, &mut ids, 80);
    let size = Size {
        width: 512,
        height: 512,
    };
    graphics
        .execute(&Command::Copy {
            image: view.clone(),
            source: source.clone(),
            rectangle: size.rect(),
            x: 0,
            y: 0,
            clip: size.rect(),
            face: DrawFace::Alpha,
            hold_alpha: false,
        })
        .unwrap();
    let a = graphics.image(&source).unwrap();
    let b = graphics.image(&view).unwrap();
    assert!(!a.same_canvas_storage(b));
    assert!(a.shares_main_storage(b));
    assert_eq!(graphics.gpu.spill_group_reclaim_bytes(&[a]), 0);
    assert_eq!(graphics.gpu.spill_group_reclaim_bytes(&[b]), 0);
    assert_eq!(graphics.gpu.spill_batch_reclaim_bytes(&[&[a], &[b]]), MIB);
    // A snapshot outside the image table must still pin the source pixels.
    let snapshot = a.shared();
    assert_eq!(graphics.gpu.spill_batch_reclaim_bytes(&[&[a], &[b]]), 0);
    drop(snapshot);
    graphics.gpu.collect().unwrap();
    let lock = graphics
        .gpu
        .resident
        .reserve(graphics.gpu.resident.available() - 128 * 1024)
        .unwrap();
    graphics.make_room(MIB, &[]).unwrap();
    assert!(graphics.gpu.resident.available() >= MIB);
    assert!(graphics.spilled.contains_key(&source.id));
    assert!(graphics.spilled.contains_key(&view.id));
    drop(lock);
    graphics.ensure_images(&[view.id, source.id]).unwrap();
    for reference in [source, view] {
        assert_eq!(
            graphics
                .gpu
                .pixel(graphics.image(&reference).unwrap(), 4, 4, false)
                .unwrap(),
            0xff044921
        );
    }
}

#[test]
fn transition_endpoint_cache_ignores_reads_and_unrelated_writes() {
    use krkr_protocol::{
        graphics::Scene,
        transition::{Effect, Frame, SceneTransition},
    };
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let destination = image(&mut graphics, &mut ids, 60);
    let source = image(&mut graphics, &mut ids, 70);
    let unrelated = image(&mut graphics, &mut ids, 80);
    let size = Size {
        width: 512,
        height: 512,
    };
    let nodes = [destination.clone(), source.clone()]
        .into_iter()
        .enumerate()
        .map(|(i, image)| Node {
            cache: None,
            visible: i == 0,
            parent: None,
            image: Some(image),
            neutral_color: 0,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        })
        .collect();
    let scene = Scene {
        nodes,
        transitions: vec![SceneTransition {
            destination: 0,
            source: 1,
            with_children: true,
            frame: Frame {
                effect: Effect::CrossFade,
                face: DrawFace::Opaque,
                size,
                phase: 128,
            },
            rule: None,
            custom: None,
        }],
        ..Default::default()
    };
    drop(graphics.capture_scaled(scene, size, size).unwrap());
    graphics
        .execute(&Command::Pixel {
            image: source.clone(),
            x: 0,
            y: 0,
            province: false,
        })
        .unwrap();
    let fill = |image| Command::Fill {
        image,
        fills: vec![Fill {
            rectangle: size.rect(),
            color: 0xff112233,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    };
    graphics.execute(&fill(unrelated)).unwrap();
    assert!(graphics.endpoints.iter().all(Option::is_some));
    graphics.execute(&fill(source)).unwrap();
    assert!(graphics.endpoints[0].is_some());
    assert!(graphics.endpoints[1].is_none());
}

#[test]
fn blur_reclaims_cold_canvases_before_degrading_to_tiny_strips() {
    fn run(admit: bool) -> (Vec<u8>, usize) {
        let context = support::Context::new();
        let shared = Budget::new(16 * MIB);
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                krkr_render_gles2::Config {
                    work_framebuffer: true,
                    tile_edge: 1024,
                    resident: shared.child(shared.limit()),
                    scratch: shared.child(shared.limit()),
                    staging: Budget::new(16 * MIB),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut graphics = Graphics::new(gpu, Cache::new(0));
        let mut ids = slotmap::SlotMap::with_key();
        let size = Size {
            width: 2048,
            height: 768,
        };
        let reference = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &graphics.gpu.staging).unwrap();
        for (i, pixel) in data
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            pixel.copy_from_slice(&[i as u8, (i / 2048) as u8, 55, 255]);
        }
        let canvas = graphics
            .gpu
            .upload_scaled(
                &Pixels {
                    size,
                    main: Some(data),
                    province: None,
                },
                size,
            )
            .unwrap();
        graphics.put(&reference, canvas);
        let _cold: Vec<_> = (0..5).map(|n| image(&mut graphics, &mut ids, n)).collect();
        graphics.gpu.collect().unwrap();
        let pressure = shared.reserve(shared.available() - 300_000).unwrap();
        let operation = krkr_protocol::graphics::Adjustment::BoxBlur {
            radius: [1, 1],
            alpha: true,
        };
        assert!(
            graphics
                .gpu
                .blur_preferred_headroom(graphics.image(&reference).unwrap(), size.rect(), [1, 1])
                .is_some()
        );
        traffic::reset();
        if admit {
            graphics
                .execute(&Command::Adjust {
                    image: reference.clone(),
                    rectangle: size.rect(),
                    operation,
                })
                .unwrap();
        } else {
            graphics
                .write(&reference, |gpu, image| {
                    gpu.adjust(image, size.rect(), &operation)
                })
                .unwrap();
        }
        let copies = traffic::store_calls();
        drop(pressure);
        let pixels = graphics
            .gpu
            .readback(graphics.image(&reference).unwrap(), size.rect(), false)
            .unwrap();
        (pixels.data.as_slice().to_vec(), copies)
    }
    let (expected, narrow) = run(false);
    let (actual, wide) = run(true);
    assert_eq!(actual, expected);
    eprintln!("blur copy calls: narrow={narrow} admitted={wide}");
    assert!(wide * 2 < narrow, "headroom should amortize halo copies");
}

type Finish = unsafe extern "system" fn();
thread_local! {
    static FREE: Cell<Option<usize>> = const { Cell::new(None) };
    static FINISH: Cell<Option<Finish>> = const { Cell::new(None) };
    static FINISHES: Cell<usize> = const { Cell::new(0) };
}
fn intercept(name: &str, address: *const c_void) -> *const c_void {
    if name == "glFinish" {
        FINISH.set(Some(unsafe {
            std::mem::transmute::<*const c_void, Finish>(address)
        }));
        finish as *const c_void
    } else {
        address
    }
}
unsafe extern "system" fn finish() {
    FINISHES.set(FINISHES.get() + 1);
    unsafe { FINISH.get().unwrap()() };
}
fn setup(context: &support::Context) -> Graphics {
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(intercept),
            krkr_render_gles2::Config {
                work_framebuffer: true,
                ..crate::memory::graphics_config(Budget::new(32 * MIB))
            },
        )
        .unwrap()
    };
    let mut graphics = Graphics::new(gpu, Cache::new(32 * MIB));
    graphics.physical_free = || FREE.get();
    FREE.set(None);
    graphics
}
fn image(graphics: &mut Graphics, ids: &mut slotmap::SlotMap<ImageId, ()>, color: u8) -> ImageRef {
    let size = Size {
        width: 512,
        height: 512,
    };
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &graphics.gpu.staging).unwrap();
    for (i, pixel) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        pixel.copy_from_slice(&[i as u8, color, 33, 255]);
    }
    let canvas = graphics
        .gpu
        .upload_scaled(
            &Pixels {
                size,
                main: Some(data),
                province: None,
            },
            size,
        )
        .unwrap();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    graphics.put(&reference, canvas);
    reference
}
fn cached(graphics: &Graphics, reference: ImageRef, name: &str) -> Key {
    let key = Key {
        names: [Some(name.encode_utf16().collect()), None, None, None],
        color_key: 0,
        rule_size: None,
    };
    graphics.cache.insert(
        key.clone(),
        Entry {
            image: reference,
            size: Size {
                width: 512,
                height: 512,
            },
            tags: Arc::default(),
            bytes: MIB,
        },
        graphics.cache.generation(),
    );
    key
}

#[test]
fn upload_recovers_staging_from_parked_canvases_before_decode() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let reference = image(&mut graphics, &mut ids, 73);
    let saved = graphics
        .gpu
        .spill_canvas(graphics.image(&reference).unwrap())
        .unwrap()
        .unwrap();
    let bytes = saved.staging_bytes();
    assert!(bytes > 0);
    graphics.images.remove(&reference.id);
    graphics
        .spilled
        .insert(reference.id, std::rc::Rc::new(saved));
    graphics.gpu.collect().unwrap();
    let available = 256 * 1024;
    let lock = graphics
        .gpu
        .staging
        .reserve(graphics.gpu.staging.available() - available)
        .unwrap();
    let gpu_pending = MIB;
    graphics
        .make_staging_room(available + bytes, gpu_pending)
        .unwrap();
    assert!(graphics.gpu.staging.available() >= available + bytes);
    assert!(graphics.gpu.resident.available() >= gpu_pending);
    assert!(!graphics.spilled.contains_key(&reference.id));
    drop(lock);
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&reference).unwrap(), 4, 4, false)
            .unwrap(),
        0xff044921
    );
}

#[test]
fn scene_reclaims_optional_images_below_the_software_limit_without_repeated_finishes() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let live = image(&mut graphics, &mut ids, 71);
    // Evicting this oldest cache entry frees no texture: a scene still owns it.
    let live_key = cached(&graphics, live.clone(), "live");
    let keys: Vec<_> = (0..3)
        .map(|i| {
            let reference = image(&mut graphics, &mut ids, i);
            cached(&graphics, reference, &format!("cold-{i}"))
        })
        .collect();
    let mut scene = Scene {
        nodes: vec![Node {
            parent: None,
            visible: true,
            opacity: 255,
            cache: None,
            image: Some(live.clone()),
            neutral_color: 0,
            rectangle: Size {
                width: 512,
                height: 512,
            }
            .rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
        }],
        ..Default::default()
    };
    graphics.gpu.collect().unwrap();
    let before = graphics.gpu.resident.used();
    assert!(graphics.gpu.resident.available() > 100 * MIB);
    let display = Size {
        width: 64,
        height: 64,
    };
    FREE.set(Some(64 * MIB));
    FINISHES.set(0);
    graphics.prepare_scene_capture(display, &mut scene).unwrap();
    assert_eq!(
        FINISHES.get(),
        0,
        "healthy memory must keep the warm caches"
    );
    assert!(keys.iter().all(|key| graphics.cache.get(key).is_some()));

    FREE.set(Some(7 * MIB));
    graphics.prepare_scene_capture(display, &mut scene).unwrap();
    assert!(before - graphics.gpu.resident.used() >= 2 * MIB);
    assert!(graphics.cache.get(&keys[0]).is_none());
    assert!(graphics.cache.get(&keys[1]).is_none());
    assert!(
        graphics.cache.get(&keys[2]).is_some(),
        "stop after reclaiming enough bytes"
    );
    assert!(!graphics.spilled.contains_key(&live.id));
    assert!(
        graphics.cache.get(&live_key).is_some(),
        "reclaim cold allocations before a live cache hit"
    );
    let finishes = FINISHES.get();
    assert!(finishes > 0);
    graphics.prepare_scene_capture(display, &mut scene).unwrap();
    assert_eq!(
        FINISHES.get(),
        finishes,
        "unchanged pressure must not flush every frame"
    );
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&live).unwrap(), 4, 4, false)
            .unwrap(),
        0xff044721
    );

    // A further physical decline must trigger collection again.
    FREE.set(Some(5 * MIB));
    graphics.prepare_scene_capture(display, &mut scene).unwrap();
    assert!(graphics.cache.get(&keys[2]).is_none());
    assert!(!graphics.spilled.contains_key(&live.id));
}

#[test]
fn pressure_spills_only_unreferenced_command_canvases_and_restores_their_pixels() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let target = image(&mut graphics, &mut ids, 80);
    let cold = image(&mut graphics, &mut ids, 90);
    graphics.gpu.collect().unwrap();
    FREE.set(Some(7 * MIB));
    graphics
        .execute(&Command::Fill {
            image: target.clone(),
            fills: vec![Fill {
                rectangle: Size {
                    width: 4,
                    height: 4,
                }
                .rect(),
                color: 0xff123456,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        })
        .unwrap();
    assert!(!graphics.spilled.contains_key(&target.id));
    assert!(graphics.spilled.contains_key(&cold.id));
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&target).unwrap(), 1, 1, false)
            .unwrap(),
        0xff123456
    );
    FREE.set(Some(64 * MIB));
    graphics.ensure_images(&[cold.id]).unwrap();
    assert!(!graphics.spilled.contains_key(&cold.id));
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&cold).unwrap(), 4, 4, false)
            .unwrap(),
        0xff045a21
    );
}

#[test]
fn full_clear_discards_parked_pixels_without_decode_workspace() {
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let reference = image(&mut graphics, &mut ids, 73);
    let alias = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let saved = std::rc::Rc::new(
        graphics
            .gpu
            .spill_canvas(graphics.image(&reference).unwrap())
            .unwrap()
            .unwrap(),
    );
    graphics.images.remove(&reference.id);
    graphics.spilled.insert(reference.id, saved.clone());
    graphics.spilled.insert(alias.id, saved);
    graphics
        .lifetimes
        .insert(alias.id, Arc::downgrade(&alias.lifetime));
    graphics.gpu.collect().unwrap();
    let source = image(&mut graphics, &mut ids, 72);
    let lock = graphics
        .gpu
        .staging
        .reserve(graphics.gpu.staging.available())
        .unwrap();
    graphics
        .execute(&Command::Fill {
            image: reference.clone(),
            fills: vec![Fill {
                rectangle: Size {
                    width: 512,
                    height: 512,
                }
                .rect(),
                color: 0x73123456,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        })
        .unwrap();
    drop(lock);
    assert!(graphics.spilled.contains_key(&alias.id));
    assert!(!graphics.spilled.contains_key(&reference.id));
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&reference).unwrap(), 3, 5, false)
            .unwrap(),
        0x73123456
    );
    for command in [
        Command::Assign {
            image: reference.clone(),
            source: source.clone(),
        },
        Command::SnapshotMain {
            image: reference.clone(),
            source: source.clone(),
        },
    ] {
        graphics.images.remove(&reference.id);
        graphics
            .spilled
            .insert(reference.id, graphics.spilled[&alias.id].clone());
        let lock = graphics
            .gpu
            .staging
            .reserve(graphics.gpu.staging.available())
            .unwrap();
        graphics.execute(&command).unwrap();
        drop(lock);
        assert!(!graphics.spilled.contains_key(&reference.id));
        assert_eq!(
            graphics
                .gpu
                .pixel(graphics.image(&reference).unwrap(), 4, 4, false)
                .unwrap(),
            0xff044821
        );
    }
    graphics.ensure_images(&[alias.id]).unwrap();
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&alias).unwrap(), 4, 4, false)
            .unwrap(),
        0xff044921
    );
}

#[test]
fn affine_clear_skips_parked_destination_and_preserves_alias() {
    use krkr_protocol::transform::{Filter, ImageOperation, Sampling, Transform};
    let context = support::Context::new();
    let mut graphics = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let target = image(&mut graphics, &mut ids, 73);
    let source = image(&mut graphics, &mut ids, 72);
    let alias = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let saved = std::rc::Rc::new(
        graphics
            .gpu
            .spill_canvas(graphics.image(&target).unwrap())
            .unwrap()
            .unwrap(),
    );
    graphics.images.remove(&target.id);
    graphics.spilled.insert(target.id, saved.clone());
    graphics.spilled.insert(alias.id, saved);
    graphics
        .lifetimes
        .insert(alias.id, Arc::downgrade(&alias.lifetime));
    graphics.gpu.collect().unwrap();
    let lock = graphics
        .gpu
        .staging
        .reserve(graphics.gpu.staging.available() - 64 * 1024)
        .unwrap();
    graphics
        .execute(&Command::Transform {
            image: target.clone(),
            source,
            rectangle: Size {
                width: 512,
                height: 512,
            }
            .rect(),
            transform: Transform::Affine([[0.0, 0.0], [255.5, 0.0], [0.0, 255.5]]),
            sampling: Sampling {
                filter: Filter::Nearest,
                sharpness: 0.0,
                no_clip: false,
            },
            operation: ImageOperation::Copy { hold_alpha: false },
            clip: Size {
                width: 512,
                height: 512,
            }
            .rect(),
            clear: Some(0x73123456),
        })
        .unwrap();
    drop(lock);
    assert!(graphics.spilled.contains_key(&alias.id));
    assert!(!graphics.spilled.contains_key(&target.id));
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&target).unwrap(), 400, 400, false)
            .unwrap(),
        0x73123456
    );
    assert_eq!(
        (graphics
            .gpu
            .pixel(graphics.image(&target).unwrap(), 4, 4, false)
            .unwrap()
            >> 8)
            & 255,
        72
    );
    graphics.ensure_images(&[alias.id]).unwrap();
    assert_eq!(
        graphics
            .gpu
            .pixel(graphics.image(&alias).unwrap(), 4, 4, false)
            .unwrap(),
        0xff044921
    );
}

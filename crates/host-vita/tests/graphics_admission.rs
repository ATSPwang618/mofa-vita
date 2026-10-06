#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
// This integration test owns a native EGL context; FFI is confined to the test.
#![allow(unsafe_code)]
#[path = "../../render-gles2/tests/support/mod.rs"]
mod support;
use krkr_host_vita::graphics::Graphics;
use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, Command, ImageRef, Node, Scene, Size},
    image_cache::{Cache, Entry, Key},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};
use std::sync::Arc;

#[test]
fn transition_endpoints_page_separately_and_keep_scaled_pixels() {
    use krkr_protocol::{
        graphics::{DrawFace, Fill},
        transition::{Effect, Frame, SceneTransition},
    };
    fn run(compact: bool) -> Vec<Vec<u8>> {
        let context = support::Context::new();
        let budget = Budget::new(if compact { 11 } else { 64 } * 1024 * 1024);
        let size = Size {
            width: 512,
            height: 512,
        };
        let physical = Size {
            width: 256,
            height: 256,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: true,
                    resident: budget.child(budget.limit()),
                    scratch: budget.child(6 * 1024 * 1024),
                    staging: Budget::new(16 * 1024 * 1024),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut host = Graphics::new(gpu, Cache::new(0));
        let mut ids = slotmap::SlotMap::with_key();
        let mut nodes = Vec::new();
        let mut references = Vec::new();
        for side in 0..2 {
            let root = nodes.len();
            nodes.push(Node {
                cache: None,
                visible: side == 0,
                parent: None,
                image: None,
                neutral_color: 0x123456,
                rectangle: size.rect(),
                image_left: 0,
                image_top: 0,
                blend: Blend::Opaque,
                opacity: 255,
            });
            for layer in 0..4 {
                let reference = ImageRef {
                    id: ids.insert(()),
                    lifetime: Arc::default(),
                };
                host.execute(&Command::PrepareUpload {
                    image: reference.clone(),
                    source: None,
                    size,
                    main: true,
                    province: false,
                })
                .unwrap();
                let mut data =
                    Bytes::zeroed(size.rgba_bytes().unwrap(), &host.gpu.staging).unwrap();
                for (i, pixel) in data
                    .as_mut_slice()
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .enumerate()
                {
                    pixel.copy_from_slice(&[(i % 251) as u8, 30 + side * 70, 35 + layer * 40, 170]);
                }
                host.execute(&Command::UploadScaled {
                    image: reference.clone(),
                    logical_size: size,
                    pixels: Arc::new(Pixels {
                        size,
                        main: Some(data),
                        province: None,
                    }),
                })
                .unwrap();
                nodes.push(Node {
                    cache: None,
                    visible: true,
                    parent: Some(root),
                    image: Some(reference.clone()),
                    neutral_color: 0,
                    rectangle: size.rect(),
                    image_left: -3,
                    image_top: 2,
                    blend: Blend::Alpha,
                    opacity: 191,
                });
                references.push(reference);
            }
        }
        let mut output = Vec::new();
        for (step, phase) in [0, 128, 200, 256, 128].into_iter().enumerate() {
            if step == 4 {
                // A live endpoint mutation must invalidate the composed cache.
                host.execute(&Command::Fill {
                    image: references[7].clone(),
                    fills: vec![Fill {
                        rectangle: size.rect(),
                        color: 0xffee2200,
                        face: DrawFace::Alpha,
                        hold_alpha: false,
                    }],
                })
                .unwrap();
            }
            let scene = Scene {
                nodes: nodes.clone(),
                transitions: vec![SceneTransition {
                    destination: 0,
                    source: 5,
                    with_children: true,
                    frame: Frame {
                        effect: Effect::CrossFade,
                        face: DrawFace::Opaque,
                        size,
                        phase,
                    },
                    rule: None,
                    custom: None,
                }],
                ..Default::default()
            };
            let snapshot = if compact {
                host.capture_scaled(scene, size, physical)
            } else {
                host.capture(scene)
            }
            .unwrap_or_else(|e| panic!("compact={compact} phase={phase}: {e}"));
            host.prepare_scene(physical).unwrap();
            let image = host
                .gpu
                .scene_surface_scaled(size, physical, &snapshot.scene, &snapshot.images)
                .unwrap();
            let pixels = host.gpu.readback(&image, physical.rect(), false).unwrap();
            output.push(pixels.data.as_slice().to_vec());
        }
        output
    }
    assert_eq!(run(true), run(false));
}

#[test]
fn incremental_text_keeps_cached_images_under_resident_pressure() {
    use krkr_protocol::{
        graphics::DrawFace,
        text::{Glyph, PlacedGlyph, Run, Style},
    };
    let context = support::Context::new();
    let size = Size {
        width: 640,
        height: 480,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                resident: Budget::new(4 * 1024 * 1024),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let cache = Cache::new(64 * 1024);
    let mut host = Graphics::new(gpu, cache.clone());
    let mut ids = slotmap::SlotMap::with_key();
    let image = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let cached = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    for (reference, size) in [
        (&image, size),
        (
            &cached,
            Size {
                width: 32,
                height: 32,
            },
        ),
    ] {
        host.execute(&Command::Create {
            image: reference.id,
            lifetime: Arc::downgrade(&reference.lifetime),
            size,
            color: 0xff000000,
        })
        .unwrap();
    }
    let key = Key {
        names: [Some("warm.png".encode_utf16().collect()), None, None, None],
        color_key: 0,
        rule_size: None,
    };
    cache.insert(
        key.clone(),
        Entry {
            image: cached,
            size: Size {
                width: 32,
                height: 32,
            },
            tags: Arc::default(),
            bytes: 4096,
        },
        cache.generation(),
    );
    let mut mask = Bytes::zeroed(18 * 20, &host.gpu.staging).unwrap();
    mask.as_mut_slice().fill(255);
    let glyph = Arc::new(Glyph {
        id: 123456,
        size: Size {
            width: 18,
            height: 20,
        },
        origin: [0, 0],
        advance: [24, 0],
        levels: 256,
        mask,
    });
    let draw = |host: &mut Graphics, x, y| {
        let glyphs = vec![PlacedGlyph {
            glyph: glyph.clone(),
            x,
            y,
            color: 0xffffff,
        }];
        let permit = host
            .gpu
            .staging
            .reserve(std::mem::size_of::<PlacedGlyph>())
            .unwrap();
        host.execute(&Command::Text {
            image: image.clone(),
            run: Run { glyphs, permit },
            style: Style {
                color: 0xffffff,
                opacity: 255,
                antialias: true,
                shadow_level: 0,
                shadow_color: 0,
                shadow_width: 0,
                shadow_offset: [0, 0],
                face: DrawFace::Opaque,
                hold_alpha: true,
            },
            clip: size.rect(),
        })
        .unwrap();
    };
    draw(&mut host, 5, 5);
    host.gpu.collect().unwrap();
    let pressure = host
        .gpu
        .resident
        .reserve(host.gpu.resident.available() - 64 * 1024)
        .unwrap();
    // Reuse one cell, append to the same line, then wrap to the next line.
    for (x, y) in [(5, 5), (85, 5), (5, 85)] {
        draw(&mut host, x, y);
        assert!(
            cache.get(&key).is_some(),
            "a small text write evicted a warm asset"
        );
    }
    drop(pressure);
    let krkr_protocol::window::Response::Image(result) =
        host.execute(&Command::ReadImage { image }).unwrap()
    else {
        panic!("expected pixels");
    };
    let pixels = result.main.as_ref().unwrap().as_slice();
    for y in 0..480usize {
        for x in 0..640usize {
            let ink = [(5, 5), (85, 5), (5, 85)]
                .iter()
                .any(|&(left, top)| (left..left + 18).contains(&x) && (top..top + 20).contains(&y));
            let value = if ink { 254 } else { 0 };
            assert_eq!(
                &pixels[(y * 640 + x) * 4..][..4],
                &[value, value, value, 255]
            );
        }
    }
}

#[test]
fn transition_capture_keeps_hidden_endpoints_without_pinning_unrelated_hidden_layers() {
    use krkr_protocol::{
        graphics::DrawFace,
        transition::{Effect, Frame, SceneTransition},
    };
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let mut host = Graphics::new(gpu, Cache::new(0));
    let size = Size {
        width: 16,
        height: 16,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let mut nodes = Vec::new();
    let mut references = Vec::new();
    for n in 0..7 {
        let image = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        host.execute(&Command::Create {
            image: image.id,
            lifetime: Arc::downgrade(&image.lifetime),
            size,
            color: 0xff123456,
        })
        .unwrap();
        nodes.push(Node {
            parent: match n {
                2 | 5 => Some(1),
                6 => Some(5),
                4 => Some(3),
                _ => None,
            },
            visible: !matches!(n, 1 | 3),
            opacity: 255,
            cache: None,
            image: Some(image.clone()),
            neutral_color: 0,
            rectangle: krkr_protocol::graphics::Rect {
                left: match n {
                    5 => 24,
                    6 => -24,
                    _ => 0,
                },
                ..size.rect()
            },
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
        });
        references.push(image);
    }
    for (with_children, width) in [(false, 16), (true, 16), (true, 32)] {
        let snapshot = host
            .capture(Scene {
                nodes: nodes.clone(),
                transitions: vec![SceneTransition {
                    destination: 0,
                    source: 1,
                    with_children,
                    frame: Frame {
                        effect: Effect::CrossFade,
                        face: DrawFace::Alpha,
                        size: Size { width, ..size },
                        phase: 128,
                    },
                    rule: None,
                    custom: None,
                }],
                ..Default::default()
            })
            .unwrap();
        assert!(snapshot.images.contains_key(&references[0].id));
        assert!(snapshot.images.contains_key(&references[1].id));
        assert_eq!(
            snapshot.images.contains_key(&references[2].id),
            with_children
        );
        assert!(!snapshot.images.contains_key(&references[3].id));
        assert!(!snapshot.images.contains_key(&references[4].id));
        assert_eq!(
            snapshot.images.contains_key(&references[5].id),
            with_children && width == 32
        );
        assert!(!snapshot.images.contains_key(&references[6].id));
        let original = Scene {
            nodes: nodes.clone(),
            transitions: snapshot.scene.transitions.clone(),
            ..Default::default()
        };
        let mut all_images = std::collections::HashMap::new();
        for reference in &references {
            all_images.insert(
                reference.id,
                host.gpu.create_image(size, 0xff123456).unwrap(),
            );
        }
        let expected = host
            .gpu
            .scene_surface_scaled(size, size, &original, &all_images)
            .unwrap();
        let actual = host
            .gpu
            .scene_surface_scaled(size, size, &snapshot.scene, &snapshot.images)
            .unwrap();
        assert_eq!(
            host.gpu
                .readback(&actual, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            host.gpu
                .readback(&expected, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
        );
        host.gpu
            .scene_surface_scaled(size, size, &snapshot.scene, &snapshot.images)
            .unwrap();
    }
}

#[test]
fn private_intermediates_page_losslessly_inside_existing_gpu_and_staging_limits() {
    let context = support::Context::new();
    let size = Size {
        width: 512,
        height: 512,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                resident: Budget::new(2 * 1024 * 1024 + 65536),
                scratch: Budget::new(4 * 1024 * 1024),
                staging: Budget::new(8 * 1024 * 1024),
                work_framebuffer: true,
                canvas_limit: Some(size),
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut host = Graphics::new(gpu, Cache::new(0));
    let mut ids = slotmap::SlotMap::with_key();
    let mut images = Vec::new();
    let mut aliases = Vec::new();
    for n in 0..3u8 {
        let image = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        host.execute(&Command::PrepareUpload {
            image: image.clone(),
            source: None,
            size,
            main: true,
            province: false,
        })
        .unwrap();
        let mut raw = Bytes::zeroed(size.rgba_bytes().unwrap(), &host.gpu.staging).unwrap();
        for (i, p) in raw
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            p.copy_from_slice(&[(i % 256) as u8, 73, n, if i % 11 == 0 { 0 } else { 255 }]);
        }
        host.execute(&Command::UploadScaled {
            image: image.clone(),
            pixels: Arc::new(Pixels {
                size,
                main: Some(raw),
                province: None,
            }),
            logical_size: size,
        })
        .unwrap();
        let alias = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        host.execute(&Command::Assign {
            image: alias.clone(),
            source: image.clone(),
        })
        .unwrap();
        aliases.push(alias);
        images.push(image);
    }
    for _ in 0..2 {
        for (n, image) in images.iter().enumerate().chain(aliases.iter().enumerate()) {
            let krkr_protocol::window::Response::Image(pixels) = host
                .execute(&Command::ReadImage {
                    image: image.clone(),
                })
                .unwrap()
            else {
                panic!("expected image")
            };
            for (i, p) in pixels
                .main
                .as_ref()
                .unwrap()
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
            {
                assert_eq!(
                    *p,
                    [
                        (i % 256) as u8,
                        73,
                        n as u8,
                        if i % 11 == 0 { 0 } else { 255 }
                    ]
                );
            }
            assert!(host.gpu.resident.used() <= host.gpu.resident.limit());
            assert!(host.gpu.staging.used() <= host.gpu.staging.limit());
        }
    }
}

#[test]
fn local_shared_fill_keeps_cached_images_when_one_tile_fits() {
    use krkr_protocol::graphics::{DrawFace, Fill, Rect};
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 8,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let cache = Cache::new(1024 * 1024);
    let mut host = Graphics::new(gpu, cache.clone());
    let mut ids = slotmap::SlotMap::with_key();
    let size = Size {
        width: 24,
        height: 16,
    };
    let image = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    host.execute(&Command::Create {
        image: image.id,
        lifetime: Arc::downgrade(&image.lifetime),
        size,
        color: 0x71325476,
    })
    .unwrap();
    let snapshot = host
        .capture(Scene {
            nodes: vec![Node {
                parent: None,
                visible: true,
                opacity: 255,
                cache: None,
                image: Some(image.clone()),
                neutral_color: 0,
                rectangle: size.rect(),
                image_left: 0,
                image_top: 0,
                blend: Blend::Opaque,
            }],
            ..Default::default()
        })
        .unwrap();
    let optional = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    host.execute(&Command::Create {
        image: optional.id,
        lifetime: Arc::downgrade(&optional.lifetime),
        size,
        color: 0x19283746,
    })
    .unwrap();
    let key = Key {
        names: [Some("cached".encode_utf16().collect()), None, None, None],
        color_key: 0,
        rule_size: None,
    };
    cache.insert(
        key.clone(),
        Entry {
            image: optional,
            size,
            tags: Arc::default(),
            bytes: size.rgba_bytes().unwrap(),
        },
        cache.generation(),
    );
    host.gpu.collect().unwrap();
    let _pressure = host
        .gpu
        .resident
        .reserve(host.gpu.resident.available() - 256)
        .unwrap();
    host.execute(&Command::Fill {
        image: image.clone(),
        fills: vec![Fill {
            rectangle: Rect {
                left: 0,
                top: 0,
                width: 8,
                height: 8,
            },
            color: 0xabcdef12,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    })
    .unwrap();
    assert!(
        cache.get(&key).is_some(),
        "local edit must not evict a whole cached image"
    );
    assert_eq!(
        host.gpu
            .pixel(&snapshot.images[&image.id], 1, 1, false)
            .unwrap(),
        0x71325476
    );
    // A whole-image clear can alias the cached solid even with no free bytes.
    assert_eq!(host.gpu.resident.available(), 0);
    host.execute(&Command::Fill {
        image,
        fills: vec![Fill {
            rectangle: size.rect(),
            color: 0x19283746,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    })
    .unwrap();
    assert!(
        cache.get(&key).is_some(),
        "solid reuse needs no admission eviction"
    );
}

#[test]
fn pressure_batches_cache_evictions_and_counts_only_unpinned_storage() {
    use std::{cell::Cell, ffi::c_void};
    type Finish = unsafe extern "system" fn();
    thread_local! {
        static REAL: Cell<Option<Finish>> = const { Cell::new(None) };
        static FINISHES: Cell<usize> = const { Cell::new(0) };
    }
    unsafe extern "system" fn finish() {
        FINISHES.set(FINISHES.get() + 1);
        unsafe { REAL.get().unwrap()() };
    }
    for scene in [false, true] {
        let context = support::Context::new();
        let shared = Budget::new(8 * 1024 * 1024);
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(|name, address| {
                    if name == "glFinish" {
                        REAL.set(Some(std::mem::transmute::<*const c_void, Finish>(address)));
                        finish as *const c_void
                    } else {
                        address
                    }
                }),
                Config {
                    work_framebuffer: true,
                    tile_edge: 128,
                    resident: shared.child(8 * 1024 * 1024),
                    scratch: shared.child(8 * 1024 * 1024),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let cache = Cache::new(1024 * 1024);
        let mut graphics = Graphics::new(gpu, cache.clone());
        let size = Size {
            width: 128,
            height: 96,
        };
        let bytes = size.rgba_bytes().unwrap();
        let mut ids = slotmap::SlotMap::with_key();
        let mut keys = Vec::new();
        let mut pinned = None;
        for i in 0..6 {
            let image = ImageRef {
                id: ids.insert(()),
                lifetime: Arc::default(),
            };
            graphics
                .execute(&Command::PrepareUpload {
                    image: image.clone(),
                    size,
                    main: true,
                    province: false,
                    source: None,
                })
                .unwrap();
            graphics
                .execute(&Command::Fill {
                    image: image.clone(),
                    fills: vec![krkr_protocol::graphics::Fill {
                        rectangle: size.rect(),
                        color: 0xff29496d,
                        face: krkr_protocol::graphics::DrawFace::Alpha,
                        hold_alpha: false,
                    }],
                })
                .unwrap();
            if i == 0 {
                pinned = Some(image.clone());
            }
            let key = Key {
                names: [
                    Some(format!("image{i}.png").encode_utf16().collect()),
                    None,
                    None,
                    None,
                ],
                color_key: 0,
                rule_size: None,
            };
            keys.push(key.clone());
            cache.insert(
                key,
                Entry {
                    image,
                    size,
                    tags: Arc::default(),
                    bytes,
                },
                cache.generation(),
            );
        }
        // Complete setup uploads first so the measured finishes are retirements.
        graphics.gpu.collect().unwrap();
        let required = bytes * if scene { 8 } else { 4 };
        let free = required - bytes * 4 + bytes / 2;
        let pressure = graphics
            .gpu
            .scratch
            .reserve(shared.available() - free)
            .unwrap();
        FINISHES.set(0);
        if scene {
            graphics.prepare_scene(size).unwrap();
            assert!(graphics.gpu.scratch.available() >= required);
        } else {
            graphics
                .execute(&Command::PrepareUpload {
                    image: ImageRef {
                        id: ids.insert(()),
                        lifetime: Arc::default(),
                    },
                    size: Size {
                        width: 128,
                        height: 384,
                    },
                    main: true,
                    province: false,
                    source: None,
                })
                .unwrap();
        }
        let finishes = FINISHES.get();
        assert_eq!(finishes, 1, "scene={scene}: per-image GPU waits");
        for key in &keys[1..5] {
            assert!(
                cache.get(key).is_none(),
                "scene={scene}: unpinned allocation retained"
            );
        }
        assert!(
            cache.get(&keys[0]).is_some(),
            "a live alias cannot yield space and should remain cached"
        );
        assert!(
            cache.get(&keys[5]).is_some(),
            "evicted beyond the actual deficit"
        );
        // The oldest live alias stays cached, and its bitmap remains readable.
        let image = pinned.unwrap();
        let snapshot = graphics
            .capture(Scene {
                nodes: vec![Node {
                    parent: None,
                    visible: true,
                    opacity: 255,
                    cache: None,
                    image: Some(image.clone()),
                    neutral_color: 0,
                    rectangle: size.rect(),
                    image_left: 0,
                    image_top: 0,
                    blend: Blend::Opaque,
                }],
                ..Default::default()
            })
            .unwrap();
        drop(pressure);
        assert!(
            graphics
                .gpu
                .readback(&snapshot.images[&image.id], size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [41, 73, 109, 255])
        );
        eprintln!("{scene:?} scene pressure: 4 retired textures, {finishes} GPU finish");
    }
}

#[test]
fn scene_admission_keeps_warm_surfaces_and_waits_only_for_real_pressure() {
    use std::{cell::Cell, ffi::c_void};
    type Finish = unsafe extern "system" fn();
    thread_local! {
        static REAL: Cell<Option<Finish>> = const { Cell::new(None) };
        static FINISHES: Cell<usize> = const { Cell::new(0) };
    }
    unsafe extern "system" fn finish() {
        FINISHES.set(FINISHES.get() + 1);
        unsafe { REAL.get().unwrap()() };
    }
    let context = support::Context::new();
    let shared = Budget::new(8 * 1024 * 1024);
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(|name, address| {
                if name == "glFinish" {
                    REAL.set(Some(std::mem::transmute::<*const c_void, Finish>(address)));
                    finish as *const c_void
                } else {
                    address
                }
            }),
            Config {
                work_framebuffer: true,
                tile_edge: 128,
                resident: shared.child(8 * 1024 * 1024),
                scratch: shared.child(4 * 1024 * 1024),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut graphics = Graphics::new(gpu, Cache::new(64 * 1024));
    let size = Size {
        width: 128,
        height: 96,
    };
    let bytes = size.rgba_bytes().unwrap();
    let surfaces: Vec<_> = (0..8)
        .map(|_| graphics.gpu.create_surface_image(size).unwrap())
        .collect();
    // Pressure from other live resources leaves one free surface, but eight
    // dead surfaces are about to enter the reuse queue. Neither stage needs
    // a global collection or permits returned to the parent.
    let pressure = graphics
        .gpu
        .scratch
        .reserve(graphics.gpu.scratch.available() - bytes)
        .unwrap();
    drop(surfaces);
    let used = shared.used();
    FINISHES.set(0);
    graphics.prepare_scene(size).unwrap();
    graphics.maintain().unwrap();
    graphics.prepare_scene(size).unwrap();
    assert_eq!(shared.used(), used, "warm allocations were freed");
    assert_eq!(FINISHES.get(), 0, "reusable storage triggered a GPU finish");
    let mut reused: Vec<_> = (0..8)
        .map(|_| graphics.gpu.create_surface_image(size).unwrap())
        .collect();
    assert_eq!(shared.used(), used, "warm surfaces were allocated again");
    graphics
        .gpu
        .fill(
            &mut reused[0],
            &[krkr_protocol::graphics::Fill {
                rectangle: size.rect(),
                color: 0xff29496d,
                face: krkr_protocol::graphics::DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
    assert!(
        graphics
            .gpu
            .readback(&reused[0], size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [41, 73, 109, 255])
    );
    // With no reusable storage left, pressure must still enter collection.
    FINISHES.set(0);
    let extra = graphics.gpu.create_surface_image(size).unwrap();
    drop(extra);
    graphics.prepare_scene(size).unwrap();
    assert!(FINISHES.get() > 0, "real pressure bypassed retirement");
    drop(pressure);
}

#[test]
fn prepared_scaled_upload_keeps_optional_images_when_resident_budget_is_full() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                resident: Budget::new(512 * 1024),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let cache = Cache::new(64 * 1024);
    let mut graphics = Graphics::new(gpu, cache.clone());
    let stored = Size {
        width: 64,
        height: 36,
    };
    let logical = Size {
        width: 128,
        height: 72,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let image = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let cached = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    for image in [&image, &cached] {
        graphics
            .execute(&Command::PrepareUpload {
                image: image.clone(),
                size: stored,
                main: true,
                province: false,
                source: None,
            })
            .unwrap();
    }
    let key = Key {
        names: [
            Some("cached.png".encode_utf16().collect()),
            None,
            None,
            None,
        ],
        color_key: 0,
        rule_size: None,
    };
    cache.insert(
        key.clone(),
        Entry {
            image: cached,
            size: stored,
            tags: Arc::default(),
            bytes: stored.rgba_bytes().unwrap(),
        },
        cache.generation(),
    );
    let mut bytes = Bytes::zeroed(stored.rgba_bytes().unwrap(), &graphics.gpu.staging).unwrap();
    for pixel in bytes.as_mut_slice().as_chunks_mut::<4>().0.iter_mut() {
        pixel.copy_from_slice(&[41, 73, 109, 255]);
    }
    let pixels = Arc::new(Pixels {
        size: stored,
        main: Some(bytes),
        province: None,
    });
    let lock = graphics
        .gpu
        .resident
        .reserve(graphics.gpu.resident.available())
        .unwrap();
    graphics
        .execute(&Command::UploadScaled {
            image: image.clone(),
            pixels,
            logical_size: logical,
        })
        .unwrap();
    assert!(
        cache.get(&key).is_some(),
        "an upload needing no new texture evicted the image cache"
    );
    drop(lock);
    let snapshot = graphics
        .capture(Scene {
            nodes: vec![Node {
                parent: None,
                visible: true,
                opacity: 255,
                cache: None,
                image: Some(image.clone()),
                neutral_color: 0,
                rectangle: logical.rect(),
                image_left: 0,
                image_top: 0,
                blend: Blend::Opaque,
            }],
            ..Default::default()
        })
        .unwrap();
    let result = &snapshot.images[&image.id];
    assert_eq!(result.size, logical);
    assert_eq!(result.stored_size(), Some(stored));
    assert!(
        graphics
            .gpu
            .readback(result, logical.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| *pixel == [41, 73, 109, 255])
    );
}

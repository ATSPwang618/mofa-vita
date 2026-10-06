#![cfg(target_os = "linux")]
use super::*;
use crate::gles_test_support as support;
use krkr_protocol::graphics::{Blend, Command as G, Fill, ImageLifetime, ImageRef, Node};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn setup(context: &support::Context) -> (window::Client, Windows) {
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            krkr_render_gles2::Config {
                staging: host.staging_budget(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    (client, Windows::new(host, gpu))
}

#[test]
fn compact_movie_to_poster_keeps_full_window_coverage_at_both_canvas_qualities() {
    use glow::HasContext;
    use krkr_protocol::{
        graphics::Scene,
        pixels::{Bytes, Yuv420, Yuv420Layout},
        texture::{Compressed, Format, reorder_bc},
    };
    let logical = Size {
        width: 1024,
        height: 576,
    };
    let stored = Size {
        width: 960,
        height: 544,
    };
    for limit in [
        DISPLAY,
        Size {
            width: 720,
            height: 408,
        },
    ] {
        let context = support::Context::sized(960, 544);
        let gl = context.gl();
        let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                krkr_render_gles2::Config {
                    work_framebuffer: true,
                    render_target_cache_entries: 8,
                    render_target_cache_bytes: 8 * 1024 * 1024,
                    canvas_limit: Some(limit),
                    staging: host.staging_budget(),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut windows = Windows::new(host, gpu);
        let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
        let id = ids.insert(());
        let _live = create(&client, &mut windows, id);
        request(
            &client,
            &mut windows,
            id,
            Command::Size {
                width: logical.width,
                height: logical.height,
                inner: true,
            },
        );
        request(&client, &mut windows, id, Command::Visible(true));
        let mut image_ids = slotmap::SlotMap::with_key();
        let poster = ImageRef {
            id: image_ids.insert(()),
            lifetime: Arc::default(),
        };
        let movie = ImageRef {
            id: image_ids.insert(()),
            lifetime: Arc::default(),
        };
        for image in [&poster, &movie] {
            request(
                &client,
                &mut windows,
                id,
                Command::Graphics(G::Create {
                    image: image.id,
                    lifetime: Arc::downgrade(&image.lifetime),
                    size: stored,
                    color: 0xffffffff,
                }),
            );
        }
        // Match the scene's cropped 904x512 KTX plus 1024x576 scale metadata.
        // White padding makes sampling the entire POT backing observable.
        let poster_size = Size {
            width: 904,
            height: 512,
        };
        let tile_size = Size {
            width: 1024,
            height: 512,
        };
        let format = Format::Bc1RgbVita;
        let length = format.byte_len(tile_size).unwrap();
        let mut linear = Bytes::zeroed(length, &windows.graphics.gpu.staging).unwrap();
        for (i, block) in linear.as_mut_slice().chunks_exact_mut(8).enumerate() {
            let endpoint = if i % 256 < 226 { 0x07e0u16 } else { 0xffffu16 };
            block[..2].copy_from_slice(&endpoint.to_le_bytes());
        }
        let mut native = Bytes::zeroed(length, &windows.graphics.gpu.staging).unwrap();
        reorder_bc(
            tile_size,
            format,
            linear.as_slice(),
            native.as_mut_slice(),
            true,
        )
        .unwrap();
        let texture = Compressed::tiled(poster_size, tile_size, format, native, 0).unwrap();
        request(
            &client,
            &mut windows,
            id,
            Command::Graphics(G::LoadCompressed {
                image: poster.clone(),
                logical_size: logical,
                texture: Arc::new(texture),
            }),
        );
        let mut data = Bytes::zeroed(
            Yuv420::byte_len(stored).unwrap(),
            &windows.graphics.gpu.staging,
        )
        .unwrap();
        data.as_mut_slice()[..960 * 544].fill(81);
        for uv in data.as_mut_slice()[960 * 544..].as_chunks_mut::<2>().0 {
            uv.copy_from_slice(&[90, 240]);
        }
        request(
            &client,
            &mut windows,
            id,
            Command::Graphics(G::UploadYuv {
                image: movie.clone(),
                logical_size: logical,
                pixels: Arc::new(Yuv420 {
                    size: stored,
                    layout: Yuv420Layout::Nv12,
                    data,
                }),
            }),
        );
        let node = |image| Node {
            parent: None,
            visible: true,
            opacity: 255,
            cache: None,
            image: Some(image),
            neutral_color: 0xffffff,
            rectangle: logical.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
        };
        for (nodes, expected) in [
            (
                vec![node(poster.clone()), node(movie.clone())],
                [254_u8, 0, 0, 255],
            ),
            (vec![node(poster.clone())], [0_u8, 255, 0, 255]),
        ] {
            client
                .publish(
                    id,
                    Scene {
                        nodes,
                        ..Default::default()
                    },
                )
                .unwrap();
            windows.pump().unwrap();
            assert!(windows.render().unwrap());
            for (x, y) in [(4, 4), (955, 4), (4, 539), (955, 539), (480, 272)] {
                let mut pixel = [0; 4];
                unsafe {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    gl.read_pixels(
                        x,
                        y,
                        1,
                        1,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(&mut pixel)),
                    );
                }
                assert!(
                    pixel
                        .into_iter()
                        .zip(expected)
                        .all(|(a, b)| a.abs_diff(b) <= 1),
                    "limit={limit:?} pixel={x},{y}: {pixel:?}, expected={expected:?}"
                );
            }
        }
    }
}

#[test]
fn loading_screen_survives_startup_until_a_visible_game_frame_is_ready() {
    use glow::HasContext;
    let context = support::Context::sized(960, 544);
    let (client, mut windows) = setup(&context);
    let gl = context.gl();
    // Stand in for the launcher's last displayed frame.
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.clear_color(1.0, 0.0, 1.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    let pixel = || {
        let mut rgba = [0; 4];
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.read_pixels(
                480,
                272,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut rgba)),
            );
        }
        rgba
    };
    assert!(!windows.render().unwrap());
    assert_eq!(pixel(), [255, 0, 255, 255]);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _alive = create(&client, &mut windows, id);
    request(&client, &mut windows, id, Command::Visible(true));
    assert!(!windows.render().unwrap(), "a window alone is not a frame");
    assert_eq!(pixel(), [255, 0, 255, 255]);
    request(&client, &mut windows, id, Command::Visible(false));
    client
        .publish(
            id,
            krkr_protocol::graphics::Scene {
                nodes: vec![Node {
                    parent: None,
                    visible: true,
                    opacity: 255,
                    cache: None,
                    image: None,
                    neutral_color: 0x336699,
                    rectangle: DISPLAY.rect(),
                    image_left: 0,
                    image_top: 0,
                    blend: Blend::Opaque,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    windows.pump().unwrap();
    assert!(!windows.render().unwrap());
    assert!(
        windows.windows[&id].snapshot.is_none(),
        "hidden frames must release their fences"
    );
    assert!(windows.windows[&id].canvas.is_some());
    assert_eq!(pixel(), [255, 0, 255, 255]);
    request(&client, &mut windows, id, Command::Visible(true));
    assert!(windows.render().unwrap());
    assert_eq!(pixel(), [51, 102, 153, 255]);
    assert!(!windows.render().unwrap(), "an idle game needs no redraws");
    request(&client, &mut windows, id, Command::Visible(false));
    assert!(
        windows.render().unwrap(),
        "hiding an already shown game must still clear it"
    );
    assert_eq!(pixel(), [0, 0, 0, 255]);
}

#[test]
fn visible_aliases_are_compacted_before_the_scene_snapshot_pins_them() {
    use krkr_protocol::budget::Budget;
    let context = support::Context::new();
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    let shared = Budget::new(12 * 1024 * 1024);
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            krkr_render_gles2::Config {
                resident: shared.child(shared.limit()),
                scratch: shared.child(6 * 1024 * 1024),
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 512,
                staging: host.staging_budget(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut windows = Windows::new(host, gpu);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _alive = create(&client, &mut windows, id);
    request(
        &client,
        &mut windows,
        id,
        Command::Size {
            width: size.width,
            height: size.height,
            inner: true,
        },
    );
    let mut image_ids = slotmap::SlotMap::with_key();
    let image = ImageRef {
        id: image_ids.insert(()),
        lifetime: Arc::default(),
    };
    let alias = ImageRef {
        id: image_ids.insert(()),
        lifetime: Arc::default(),
    };
    windows
        .graphics
        .execute(&G::Create {
            image: image.id,
            lifetime: Arc::downgrade(&image.lifetime),
            size,
            color: 0x00445566,
        })
        .unwrap();
    windows
        .graphics
        .execute(&G::Fill {
            image: image.clone(),
            fills: vec![Fill {
                rectangle: Rect {
                    left: 130,
                    top: 100,
                    width: 60,
                    height: 40,
                },
                color: 0x80776655,
                face: krkr_protocol::graphics::DrawFace::Alpha,
                hold_alpha: false,
            }],
        })
        .unwrap();
    windows
        .graphics
        .execute(&G::Assign {
            image: alias.clone(),
            source: image.clone(),
        })
        .unwrap();
    windows.graphics.gpu.collect().unwrap();
    let before = windows.graphics.gpu.resident.used();
    let _pressure = shared.reserve(shared.available() - 1024 * 1024).unwrap();
    client
        .publish(
            id,
            krkr_protocol::graphics::Scene {
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
            },
        )
        .unwrap();
    windows.pump().unwrap();
    assert!(
        before - windows.graphics.gpu.resident.used() > 300_000,
        "reserving the frame must free old shared borders before pinning its pixels"
    );
    let snapshot = windows.windows[&id].snapshot.as_ref().unwrap();
    let captured = &snapshot.images[&image.id];
    assert_eq!(
        windows
            .graphics
            .gpu
            .pixel(captured, 150, 120, false)
            .unwrap(),
        0x80776655
    );
    assert_eq!(
        windows.graphics.gpu.pixel(captured, 0, 0, false).unwrap(),
        0x00445566
    );
}

#[test]
fn stats_overlay_is_opt_in_and_does_not_keep_an_idle_game_rendering() {
    use glow::HasContext;
    let context = support::Context::sized(960, 544);
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(&client, &mut windows, id, Command::Visible(true));
    client
        .publish(
            id,
            krkr_protocol::graphics::Scene {
                nodes: vec![Node {
                    cache: None,
                    visible: true,
                    parent: None,
                    image: None,
                    neutral_color: 0x336699,
                    rectangle: DISPLAY.rect(),
                    image_left: 0,
                    image_top: 0,
                    blend: Blend::Opaque,
                    opacity: 255,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    windows.pump().unwrap();
    assert!(windows.render().unwrap());
    assert!(!windows.render().unwrap());
    let baseline = windows.graphics.gpu.resident.used();
    windows.set_show_stats(true);
    assert!(windows.render().unwrap());
    assert!(!windows.render().unwrap());
    assert!(windows.graphics.gpu.resident.used() > baseline);
    let mut rgba = vec![0; (960 * 544 * 4) as usize];
    unsafe {
        context.gl().read_pixels(
            0,
            0,
            960,
            544,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut rgba)),
        );
    }
    assert!(rgba.chunks_exact(4).any(|p| p == [118, 222, 255, 255]));
    let canvas = windows.windows[&id].canvas.as_ref().unwrap();
    assert_eq!(
        windows.graphics.gpu.pixel(canvas, 20, 20, false).unwrap(),
        0xff336699
    );
    if let Some(directory) = std::env::var_os("KRKR_LAUNCHER_PREVIEW") {
        let top_down: Vec<_> = rgba
            .chunks_exact(960 * 4)
            .rev()
            .flatten()
            .copied()
            .collect();
        std::fs::write(
            std::path::Path::new(&directory).join("vita-stats.rgba"),
            top_down,
        )
        .unwrap();
    }
    windows.set_show_stats(false);
    assert!(windows.render().unwrap());
    assert!(!windows.render().unwrap());
    windows.graphics.gpu.collect().unwrap();
    assert_eq!(windows.graphics.gpu.resident.used(), baseline);
}
fn request(
    client: &window::Client,
    windows: &mut Windows,
    id: WindowId,
    command: Command,
) -> Response {
    let ticket = client.request(id, command).unwrap();
    for _ in 0..16 {
        windows.pump().unwrap();
        if let Some(response) = ticket.take() {
            return response.unwrap();
        }
        windows.render().unwrap();
    }
    panic!("request did not complete across frame fences")
}
fn create(client: &window::Client, windows: &mut Windows, id: WindowId) -> Arc<AtomicBool> {
    let live = Arc::new(AtomicBool::new(true));
    request(
        client,
        windows,
        id,
        Command::Create {
            alive: Arc::downgrade(&live),
            caption: String::new(),
        },
    );
    live
}
fn drain(client: &window::Client) -> Vec<window::Event> {
    std::iter::from_fn(|| client.pop_event()).collect()
}

#[test]
fn resize_policy_and_z_order_return_complete_window_state() {
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(&client, &mut windows, id, Command::Visible(true));
    for command in [
        Command::DisableResize(true),
        Command::DisableResize(false),
        Command::ZOrder {
            order: window::ZOrder::Top,
            activate: false,
        },
        Command::ZOrder {
            order: window::ZOrder::Bottom,
            activate: false,
        },
    ] {
        let Response::Snapshot(snapshot) = request(&client, &mut windows, id, command) else {
            panic!("window setters require a snapshot, not just geometry");
        };
        assert!(snapshot.visible);
        assert_eq!(snapshot.geometry.inner_width, 640);
        assert_eq!(snapshot.geometry.inner_height, 480);
        assert_eq!(snapshot.normal, Some(rectangle(snapshot.geometry)));
        assert_eq!(snapshot.maximize_box, Some(true));
        assert_eq!(snapshot.minimize_box, Some(true));
    }
}

#[test]
fn hidden_pointer_buttons_deliver_clicks_without_revealing_cursor() {
    use crate::input::{CIRCLE, CROSS, Sample, State};
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(
        &client,
        &mut windows,
        id,
        Command::Size {
            width: 960,
            height: 544,
            inner: true,
        },
    );
    request(&client, &mut windows, id, Command::Visible(true));
    let start = Instant::now();
    let mut input = State::new(start);
    input.poll(Sample::default(), start, &mut windows).unwrap();
    input
        .poll(
            Sample {
                analog: [200, 128],
                ..Default::default()
            },
            start + Duration::from_millis(20),
            &mut windows,
        )
        .unwrap();
    assert!(windows.pointer_visible);
    input
        .poll(
            Sample::default(),
            start + Duration::from_secs(3),
            &mut windows,
        )
        .unwrap();
    assert!(!windows.pointer_visible);
    let position = windows.pointer();
    drain(&client);
    for (second, buttons) in [(4, CIRCLE), (5, CROSS)] {
        input
            .poll(
                Sample {
                    buttons,
                    ..Default::default()
                },
                start + Duration::from_secs(second),
                &mut windows,
            )
            .unwrap();
        assert!(!windows.pointer_visible);
        input
            .poll(
                Sample::default(),
                start + Duration::from_secs(second) + Duration::from_millis(20),
                &mut windows,
            )
            .unwrap();
        assert!(!windows.pointer_visible);
        assert_eq!(windows.pointer(), position);
        let events = drain(&client);
        assert_eq!(events.len(), if buttons == CIRCLE { 3 } else { 2 });
        assert!(matches!(events[0].input, Input::MouseDown { .. }));
        if buttons == CIRCLE {
            assert!(matches!(events[1].input, Input::Click { .. }));
        }
        assert!(matches!(
            events.last().unwrap().input,
            Input::MouseUp { .. }
        ));
    }
    input
        .poll(
            Sample {
                analog: [128, 200],
                ..Default::default()
            },
            start + Duration::from_secs(6),
            &mut windows,
        )
        .unwrap();
    assert!(windows.pointer_visible);
}

#[test]
fn queued_frame_finishes_before_later_writes_without_duplicate_image_storage() {
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(
        &client,
        &mut windows,
        id,
        Command::Size {
            width: 8,
            height: 4,
            inner: true,
        },
    );
    let mut images = slotmap::SlotMap::with_key();
    let image = ImageRef {
        id: images.insert(()),
        lifetime: Arc::default(),
    };
    let size = Size {
        width: 8,
        height: 4,
    };
    request(
        &client,
        &mut windows,
        id,
        Command::Graphics(G::Create {
            image: image.id,
            lifetime: Arc::downgrade(&image.lifetime),
            size,
            color: 0xff123456,
        }),
    );
    let scene = || krkr_protocol::graphics::Scene {
        nodes: vec![Node {
            cache: None,
            visible: true,
            parent: None,
            image: Some(image.clone()),
            neutral_color: 0,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        }],
        ..Default::default()
    };
    // Warm the reusable composition surface before consuming all remaining
    // resident capacity. A frame fence must not require a second bitmap.
    client.publish(id, scene()).unwrap();
    windows.pump().unwrap();
    windows.render().unwrap();
    windows.graphics.gpu.collect().unwrap();
    let _pressure = windows
        .graphics
        .gpu
        .resident
        .reserve(windows.graphics.gpu.resident.available())
        .unwrap();
    client.publish(id, scene()).unwrap();
    let ticket = client
        .request(
            id,
            Command::Graphics(G::Fill {
                image: image.clone(),
                fills: vec![Fill {
                    rectangle: size.rect(),
                    color: 0xffabcdef,
                    face: krkr_protocol::graphics::DrawFace::Alpha,
                    hold_alpha: false,
                }],
            }),
        )
        .unwrap();
    // Repeated pumps before the host's next present must keep the same fence.
    for _ in 0..3 {
        windows.pump().unwrap();
        assert!(ticket.take().is_none());
    }
    windows.render().unwrap();
    let canvas = windows.windows[&id].canvas.as_ref().unwrap();
    assert_eq!(
        windows.graphics.gpu.pixel(canvas, 0, 0, false).unwrap(),
        0xff123456
    );
    windows.pump().unwrap();
    assert!(ticket.take().unwrap().is_ok());
    let Response::Pixel(pixel) = request(
        &client,
        &mut windows,
        id,
        Command::Graphics(G::Pixel {
            image,
            x: 0,
            y: 0,
            province: false,
        }),
    ) else {
        panic!()
    };
    assert_eq!(pixel, 0xffabcdef);
}

#[test]
fn scene_fence_pins_images_before_later_writes_and_releases_after_raster() {
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(
        &client,
        &mut windows,
        id,
        Command::Size {
            width: 8,
            height: 4,
            inner: true,
        },
    );
    let mut ids = slotmap::SlotMap::with_key();
    let image = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::new(ImageLifetime::default()),
    };
    request(
        &client,
        &mut windows,
        id,
        Command::Graphics(G::Create {
            image: image.id,
            lifetime: Arc::downgrade(&image.lifetime),
            size: Size {
                width: 8,
                height: 4,
            },
            color: 0xff123456,
        }),
    );
    let scene = krkr_protocol::graphics::Scene {
        nodes: vec![Node {
            cache: None,
            visible: true,
            parent: None,
            image: Some(image.clone()),
            neutral_color: 0,
            rectangle: Rect {
                width: 8,
                height: 4,
                ..Default::default()
            },
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        }],
        ..Default::default()
    };
    client.publish(id, scene).unwrap();
    // Put this write behind the scene before the host consumes either update.
    request(
        &client,
        &mut windows,
        id,
        Command::Graphics(G::Fill {
            image: image.clone(),
            fills: vec![Fill {
                rectangle: Rect {
                    width: 8,
                    height: 4,
                    ..Default::default()
                },
                color: 0xffabcdef,
                face: krkr_protocol::graphics::DrawFace::Alpha,
                hold_alpha: false,
            }],
        }),
    );
    windows.render().unwrap();
    let canvas = windows.windows[&id].canvas.as_ref().unwrap();
    assert_eq!(
        windows.graphics.gpu.pixel(canvas, 0, 0, false).unwrap(),
        0xff123456
    );
    let Response::Pixel(pixel) = request(
        &client,
        &mut windows,
        id,
        Command::Graphics(G::Pixel {
            image: image.clone(),
            x: 0,
            y: 0,
            province: false,
        }),
    ) else {
        panic!()
    };
    assert_eq!(pixel, 0xffabcdef);
    assert!(windows.windows[&id].snapshot.is_none());
    assert_eq!(Arc::strong_count(&image.lifetime), 1);
}

#[test]
fn prepared_fills_obey_capacity_readback_and_scene_fences() {
    for work in [false, true] {
        let context = support::Context::new();
        let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                krkr_render_gles2::Config {
                    work_framebuffer: work,
                    staging: host.staging_budget(),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut windows = Windows::new(host, gpu);
        let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
        let id = ids.insert(());
        let _live = create(&client, &mut windows, id);
        let size = Size {
            width: 16,
            height: 8,
        };
        request(
            &client,
            &mut windows,
            id,
            Command::Size {
                width: 16,
                height: 8,
                inner: true,
            },
        );
        let mut images = slotmap::SlotMap::with_key();
        let image = ImageRef {
            id: images.insert(()),
            lifetime: Arc::default(),
        };
        request(
            &client,
            &mut windows,
            id,
            Command::Graphics(G::Create {
                image: image.id,
                lifetime: Arc::downgrade(&image.lifetime),
                size,
                color: 0xff000000,
            }),
        );
        let fill = |x, y, color| G::Fill {
            image: image.clone(),
            fills: vec![Fill {
                rectangle: Rect {
                    left: x,
                    top: y,
                    width: 1,
                    height: 1,
                },
                color,
                face: krkr_protocol::graphics::DrawFace::Alpha,
                hold_alpha: false,
            }],
        };
        for i in 0..krkr_protocol::graphics::DRAW_BATCH_CAPACITY {
            assert!(
                client
                    .try_draw(id, &fill((i % 16) as i32, (i / 16) as i32, 0xff234567))
                    .unwrap()
            );
        }
        assert!(
            !client.try_draw(id, &fill(0, 0, 0xffee2244)).unwrap(),
            "full grants must close"
        );
        windows.pump().unwrap();
        assert!(client.try_draw(id, &fill(0, 0, 0xffee2244)).unwrap());
        assert!(client.try_draw(id, &fill(1, 0, 0xff2288ee)).unwrap());
        client
            .publish(
                id,
                krkr_protocol::graphics::Scene {
                    nodes: vec![Node {
                        cache: None,
                        visible: true,
                        parent: None,
                        image: Some(image.clone()),
                        neutral_color: 0,
                        rectangle: size.rect(),
                        image_left: 0,
                        image_top: 0,
                        blend: Blend::Opaque,
                        opacity: 255,
                    }],
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(
            !client.try_draw(id, &fill(0, 0, 0xff00cc00)).unwrap(),
            "scene fences close grants"
        );
        // The scene must contain the admitted fills, but exclude this later write.
        request(
            &client,
            &mut windows,
            id,
            Command::Graphics(fill(0, 0, 0xff00cc00)),
        );
        windows.render().unwrap();
        let canvas = windows.windows[&id].canvas.as_ref().unwrap();
        assert_eq!(
            windows.graphics.gpu.pixel(canvas, 0, 0, false).unwrap(),
            0xffee2244
        );
        assert_eq!(
            windows.graphics.gpu.pixel(canvas, 1, 0, false).unwrap(),
            0xff2288ee
        );
        assert_eq!(
            windows.graphics.gpu.pixel(canvas, 15, 7, false).unwrap(),
            0xff234567
        );
        assert!(client.try_draw(id, &fill(0, 0, 0xff885522)).unwrap());
        let Response::Pixel(value) = request(
            &client,
            &mut windows,
            id,
            Command::Graphics(G::Pixel {
                image: image.clone(),
                x: 0,
                y: 0,
                province: false,
            }),
        ) else {
            panic!("expected pixel response")
        };
        assert_eq!(value, 0xff885522, "readback must flush the grant first");
        assert!(!client.try_draw(id, &fill(0, 0, 0)).unwrap());
        assert_eq!(Arc::strong_count(&image.lifetime), 1);
    }
}

#[test]
fn clipped_subtrees_do_not_pin_bitmaps_but_visible_crops_keep_scene_fences() {
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    let mut images = slotmap::SlotMap::with_key();
    let image = ImageRef {
        id: images.insert(()),
        lifetime: Arc::default(),
    };
    let size = Size {
        width: 32,
        height: 24,
    };
    request(
        &client,
        &mut windows,
        id,
        Command::Graphics(G::Create {
            image: image.id,
            lifetime: Arc::downgrade(&image.lifetime),
            size,
            color: 0xff123456,
        }),
    );
    let root = Node {
        cache: None,
        visible: true,
        parent: None,
        image: None,
        neutral_color: 0,
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    };
    let mut parent = root.clone();
    parent.parent = Some(0);
    parent.rectangle = Rect {
        left: 2,
        top: 3,
        width: 10,
        height: 10,
    };
    let mut outside = root.clone();
    outside.parent = Some(1);
    outside.image = Some(image.clone());
    outside.rectangle.left = 20;
    let mut descendant = outside.clone();
    descendant.parent = Some(2);
    descendant.rectangle.left = -20;
    let mut scene = krkr_protocol::graphics::Scene {
        nodes: vec![root.clone(), parent, outside, descendant],
        ..Default::default()
    };
    let snapshot = windows
        .graphics
        .capture(krkr_protocol::graphics::Scene {
            nodes: scene.nodes.clone(),
            ..Default::default()
        })
        .unwrap();
    assert!(snapshot.images.is_empty());
    assert!(snapshot.scene.nodes[2].image.is_none());
    assert!(snapshot.scene.nodes[3].image.is_none());
    let bytes = windows.graphics.gpu.resident.used();
    let fill = |color| G::Fill {
        image: image.clone(),
        fills: vec![Fill {
            rectangle: size.rect(),
            color,
            face: krkr_protocol::graphics::DrawFace::Alpha,
            hold_alpha: false,
        }],
    };
    windows.graphics.execute(&fill(0xffabcdef)).unwrap();
    assert_eq!(windows.graphics.gpu.resident.used(), bytes);
    // A partially visible child still needs its exact old bitmap version.
    scene.nodes[2].rectangle.left = 5;
    let snapshot = windows.graphics.capture(scene).unwrap();
    assert_eq!(snapshot.images.len(), 1);
    windows.graphics.execute(&fill(0xff456789)).unwrap();
    assert_eq!(
        windows
            .graphics
            .gpu
            .pixel(&snapshot.images[&image.id], 0, 0, false)
            .unwrap(),
        0xffabcdef
    );
    assert!(windows.graphics.gpu.resident.used() > bytes);
}

#[test]
fn full_hd_windows_keep_script_geometry_and_allocate_only_panel_sized_canvases() {
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    windows.graphics.gpu.scratch = krkr_protocol::budget::Budget::new(3 * 1024 * 1024);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(
        &client,
        &mut windows,
        id,
        Command::Size {
            width: 1920,
            height: 1080,
            inner: true,
        },
    );
    let root = Node {
        cache: None,
        visible: true,
        parent: None,
        image: None,
        neutral_color: 0x123456,
        rectangle: Size {
            width: 1920,
            height: 1080,
        }
        .rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    };
    let mut group = root.clone();
    group.parent = Some(0);
    group.opacity = 128;
    let mut child = root.clone();
    child.parent = Some(1);
    child.neutral_color = 0x123456;
    child.rectangle = Rect {
        left: 701,
        top: 301,
        width: 100,
        height: 80,
    };
    client
        .publish(
            id,
            krkr_protocol::graphics::Scene {
                nodes: vec![root, group, child],
                ..Default::default()
            },
        )
        .unwrap();
    windows.pump().unwrap();
    windows.render().unwrap();
    let canvas = windows.windows[&id].canvas.as_ref().unwrap();
    assert_eq!(
        canvas.size,
        Size {
            width: 1920,
            height: 1080
        }
    );
    assert_eq!(
        canvas.stored_size(),
        Some(Size {
            width: 960,
            height: 540
        })
    );
    assert_eq!(windows.mapping(id).unwrap().input((480, 272)), (960, 540));
    assert_eq!(
        windows.graphics.gpu.pixel(canvas, 710, 310, false).unwrap() & 0xffffff,
        0x123456
    );
    windows.graphics.gpu.collect().unwrap();
    assert_eq!(windows.graphics.gpu.scratch.used(), 960 * 540 * 4);
    assert!(!windows.render().unwrap());
}

#[test]
fn modal_restores_focus_and_held_buttons_cannot_click_through() {
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let main = ids.insert(());
    let modal = ids.insert(());
    let _main_live = create(&client, &mut windows, main);
    let _modal_live = create(&client, &mut windows, modal);
    request(
        &client,
        &mut windows,
        main,
        Command::Size {
            width: 1280,
            height: 720,
            inner: true,
        },
    );
    request(&client, &mut windows, main, Command::Visible(true));
    assert_eq!(
        windows.mapping(main).unwrap().destination(),
        Rect {
            left: 0,
            top: 2,
            width: 960,
            height: 540
        }
    );
    assert_eq!(windows.mapping(main).unwrap().input((480, 272)), (640, 360));
    assert_eq!(windows.mapping(main).unwrap().input((480, 0)), (640, -3));
    let now = Instant::now();
    let mut input = crate::input::State::new(now);
    input.poll(Default::default(), now, &mut windows).unwrap();
    drain(&client);
    let down = crate::input::Sample {
        buttons: crate::input::CIRCLE,
        ..Default::default()
    };
    input
        .poll(down, now + Duration::from_millis(8), &mut windows)
        .unwrap();
    assert!(
        drain(&client)
            .iter()
            .any(|e| matches!(e.input, Input::MouseDown { x: 640, y: 360, .. }))
    );
    let active = Arc::new(AtomicBool::new(true));
    let ticket = client
        .request(
            modal,
            Command::ShowModal {
                active: Arc::downgrade(&active),
            },
        )
        .unwrap();
    windows.pump().unwrap();
    assert!(ticket.take().is_none());
    assert_eq!(windows.focused(), Some(modal));
    drain(&client);
    input
        .poll(down, now + Duration::from_millis(16), &mut windows)
        .unwrap();
    input
        .poll(
            Default::default(),
            now + Duration::from_millis(24),
            &mut windows,
        )
        .unwrap();
    assert!(drain(&client).is_empty());
    active.store(false, Ordering::Release);
    windows.pump().unwrap();
    assert!(matches!(
        ticket.take().unwrap().unwrap(),
        Response::Geometry(_)
    ));
    assert_eq!(windows.focused(), Some(main));
    assert!(!windows.windows[&modal].visible);
    input
        .poll(
            Default::default(),
            now + Duration::from_millis(32),
            &mut windows,
        )
        .unwrap();
    drain(&client);
    input
        .poll(down, now + Duration::from_millis(40), &mut windows)
        .unwrap();
    input
        .poll(
            Default::default(),
            now + Duration::from_millis(48),
            &mut windows,
        )
        .unwrap();
    let events = drain(&client);
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0].input, Input::MouseDown { .. }));
    assert!(matches!(events[1].input, Input::Click { .. }));
    assert!(matches!(events[2].input, Input::MouseUp { .. }));
    assert!(events.iter().all(|e| e.window == main));
    // An upscaled 640x480 client has side bars. A touch beginning just outside
    // the left edge must not round into the first column, even if dragged in.
    request(
        &client,
        &mut windows,
        main,
        Command::Size {
            width: 640,
            height: 480,
            inner: true,
        },
    );
    let map = windows.mapping(main).unwrap();
    let left = map.destination().left;
    assert_eq!(map.input((left - 1, 100)).0, -1);
    drain(&client);
    input
        .poll(
            crate::input::Sample {
                touch: Some((1, (left - 1, 100))),
                ..Default::default()
            },
            now + Duration::from_millis(56),
            &mut windows,
        )
        .unwrap();
    input
        .poll(
            crate::input::Sample {
                touch: Some((1, (left + 10, 100))),
                ..Default::default()
            },
            now + Duration::from_millis(64),
            &mut windows,
        )
        .unwrap();
    input
        .poll(
            Default::default(),
            now + Duration::from_millis(72),
            &mut windows,
        )
        .unwrap();
    assert!(
        drain(&client)
            .iter()
            .all(|event| matches!(event.input, Input::MouseMove { .. }))
    );
}

#[test]
fn vita_right_stick_scrolls_and_drag_release_does_not_click() {
    use crate::input::{Sample, State};
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(&client, &mut windows, id, Command::Visible(true));
    let now = Instant::now();
    let mut state = State::new(now);
    state.poll(Sample::default(), now, &mut windows).unwrap();
    drain(&client);
    for (ms, scroll) in [(8, 0), (20, 0), (410, 0), (418, 128), (426, 255)] {
        state
            .poll(
                Sample {
                    scroll,
                    ..Default::default()
                },
                now + Duration::from_millis(ms),
                &mut windows,
            )
            .unwrap();
    }
    let events = drain(&client);
    let deltas: Vec<_> = events
        .iter()
        .filter_map(|e| match e.input {
            Input::Wheel { delta, .. } => Some(delta),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, [120, 120, -120]);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.input, Input::KeyDown { .. } | Input::KeyUp { .. }))
    );
    for (ms, touch) in [
        (500, Some((3, (700, 150)))),
        (510, Some((3, (700, 350)))),
        (520, None),
    ] {
        state
            .poll(
                Sample {
                    touch,
                    ..Default::default()
                },
                now + Duration::from_millis(ms),
                &mut windows,
            )
            .unwrap();
    }
    let events = drain(&client);
    assert!(
        events
            .iter()
            .any(|e| matches!(e.input, Input::MouseMove { shift: 8, .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e.input, Input::MouseUp { button: 0, .. }))
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.input, Input::Click { .. }))
    );
    // The second contact cancels a left press, then scrolls. Lifting only
    // one finger must not start a new click/drag underneath the gesture.
    for (ms, touch, scroll_touch) in [
        (600, Some((4, (400, 200))), None),
        (610, Some((4, (400, 200))), Some((450, 200))),
        (620, Some((4, (400, 221))), Some((450, 221))),
        (630, Some((4, (400, 239))), Some((450, 239))),
        (640, Some((7, (450, 239))), None),
        (650, None, None),
    ] {
        state
            .poll(
                Sample {
                    touch,
                    scroll_touch,
                    ..Default::default()
                },
                now + Duration::from_millis(ms),
                &mut windows,
            )
            .unwrap();
    }
    let events = drain(&client);
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.input, Input::MouseDown { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.input, Input::MouseUp { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e.input, Input::Wheel { delta: 120, .. }))
            .count(),
        2
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.input, Input::Click { .. }))
    );
}

#[test]
fn long_select_opens_keyboard_without_enter_and_chords_release_cleanly() {
    use crate::input::{Sample, State, bindings::*};
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let mut ids = slotmap::SlotMap::<WindowId, ()>::with_key();
    let id = ids.insert(());
    let _live = create(&client, &mut windows, id);
    request(&client, &mut windows, id, Command::Visible(true));
    let now = Instant::now();
    let mut state = State::new(now);
    state.poll(Default::default(), now, &mut windows).unwrap();
    drain(&client);
    for (ms, buttons) in [(8, SELECT), (360, SELECT), (380, 0)] {
        state
            .poll(
                Sample {
                    buttons,
                    ..Default::default()
                },
                now + Duration::from_millis(ms),
                &mut windows,
            )
            .unwrap();
    }
    assert!(windows.input_panel_open());
    assert!(drain(&client).is_empty());
    state
        .poll(
            Sample {
                buttons: CROSS,
                ..Default::default()
            },
            now + Duration::from_millis(400),
            &mut windows,
        )
        .unwrap();
    assert!(!windows.input_panel_open());
    state
        .poll(
            Default::default(),
            now + Duration::from_millis(410),
            &mut windows,
        )
        .unwrap();
    assert!(drain(&client).is_empty());
    let mut bindings = Bindings::default();
    bindings.entries.push(Binding {
        buttons: L | CIRCLE,
        action: Action::Key {
            key: 83,
            modifiers: 4,
        },
    });
    state.set_bindings(bindings);
    for (ms, buttons) in [(500, CIRCLE), (530, CIRCLE | L), (540, CIRCLE), (550, 0)] {
        state
            .poll(
                Sample {
                    buttons,
                    ..Default::default()
                },
                now + Duration::from_millis(ms),
                &mut windows,
            )
            .unwrap();
    }
    let events = drain(&client);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.input, Input::Click { .. } | Input::MouseDown { .. }))
    );
    let keys: Vec<_> = events
        .iter()
        .filter_map(|e| match e.input {
            Input::KeyDown { key, .. } => Some((key, true)),
            Input::KeyUp { key, .. } => Some((key, false)),
            _ => None,
        })
        .collect();
    assert_eq!(keys, [(17, true), (83, true), (83, false), (17, false)]);
}

#[test]
fn native_host_runs_real_engine_and_all_seven_extrans_providers() {
    let script = include_str!("../../../krkr-plugins/tests/fixtures/extrans.tjs").replace(
        "root.saveLayerImage(\"target/extrans-output.png\",\"png\");",
        "",
    );
    run_engine_fixture(script);
}

#[test]
fn native_host_runs_perspective_plugin_pixel_and_lifetime_contracts() {
    let script = include_str!("../../../krkr-plugins/tests/fixtures/perspective.tjs").replace(
        "root.saveLayerImage(\"target/perspective-output.png\",\"png\");",
        "",
    );
    run_engine_fixture(script);
}

fn run_engine_fixture(script: String) {
    run_engine_fixture_with_files(script, &[]);
}

#[test]
fn native_host_runs_filter_lens_vortex_and_stretch_integer_contracts() {
    run_engine_fixture(
        include_str!("../../../krkr-plugins/tests/fixtures/filter_warp.tjs").to_owned(),
    );
}

#[test]
fn native_host_runs_engine_adjustments_and_ksupport_color_fields() {
    run_engine_fixture(
        include_str!("../../../krkr-plugins/tests/fixtures/image_adjustments.tjs").to_owned(),
    );
}

#[test]
fn native_host_runs_ya_lines_and_indexed_focus_batches() {
    run_engine_fixture(
        include_str!("../../../krkr-plugins/tests/fixtures/ya_lines.tjs").to_owned(),
    );
}

#[test]
fn native_host_blurs_both_alpha_faces_without_clipping_the_source_neighborhood() {
    run_engine_fixture(
        include_str!("../../../krkr-plugins/tests/fixtures/box_blur.tjs").to_owned(),
    );
}

#[test]
fn native_host_runs_point_filters_and_both_random_fill_providers() {
    run_engine_fixture(
        include_str!("../../../krkr-plugins/tests/fixtures/point_filters.tjs").to_owned(),
    );
}

#[test]
fn native_host_runs_gaussian_smudge_and_blur_through_plugin_commands() {
    run_engine_fixture(
        include_str!("../../../krkr-plugins/tests/fixtures/neighborhood_filters.tjs").to_owned(),
    );
}

#[test]
fn native_host_draws_emote_and_d3d_device_through_real_mesh_commands() {
    let script = include_str!("../../../krkr-plugins/tests/fixtures/emote.tjs").replace(
        "check(p.animating",
        "var red = false; for(var y=0;y<32;y+=2) for(var x=0;x<32;x+=2)\n\
         if(output.getMainPixel(x,y)==0xff0000) red=true;\n\
         check(red, 'E-mote texture pixels reached the GLES target');\n\
         check(p.animating",
    );
    run_engine_fixture_with_files(
        script,
        &[(
            "emote.mdf",
            include_bytes!("../../../krkr-plugins/tests/fixtures/emote.mdf"),
        )],
    );
}

fn run_engine_fixture_with_files(script: String, files: &[(&str, &[u8])]) {
    let context = support::Context::new();
    let (client, mut windows) = setup(&context);
    let root = tempfile::tempdir().unwrap();
    for (name, bytes) in files {
        std::fs::write(root.path().join(name), bytes).unwrap();
    }
    // Reuse the authoritative provider scenario, omitting its exported gallery.
    let script = script + "\nSystem.exit(37);";
    std::fs::write(root.path().join("startup.tjs"), script).unwrap();
    let path = root.path().to_owned();
    let stopped = Arc::new(AtomicBool::new(false));
    let cancel = stopped.clone();
    let worker = std::thread::spawn(move || {
        crate::bootstrap::run_with_windows(&path, Some(client), &cancel)
    });
    let deadline = Instant::now() + Duration::from_secs(90);
    let result: Result<(), String> = (|| {
        while !worker.is_finished() {
            if Instant::now() >= deadline {
                return Err("engine/provider scenario timed out".into());
            }
            windows.pump()?;
            windows.render()?;
            windows.graphics.maintain()?;
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    })();
    stopped.store(true, Ordering::Release);
    if let Err(error) = &result {
        windows.host.disconnect(error.clone());
    }
    worker.thread().unpark();
    let outcome = worker.join().unwrap();
    result.unwrap();
    assert_eq!(outcome.unwrap(), 37);
}

use crate::test_traffic as traffic;
use crate::{Config, Gpu, Image, test_support::Context};
use glow::HasContext;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};

const SIZE: Size = Size {
    width: 64,
    height: 64,
};

#[test]
fn retired_native_targets_reuse_attachments_without_lending_them_to_sample_textures() {
    let context = Context::new();
    let (gpu, _images) = setup(&context, 0);
    let native = gpu.device.render_texture(SIZE, &gpu.scratch).unwrap();
    let name = native.name();
    let framebuffer = native.framebuffer.get();
    drop(native);

    let ordinary = gpu.device.sample_texture(SIZE, &gpu.scratch).unwrap();
    assert_ne!(ordinary.name(), name);
    assert!(ordinary.framebuffer.get().is_none());
    traffic::reset();
    let reused = gpu.device.render_texture(SIZE, &gpu.scratch).unwrap();
    assert_eq!(reused.name(), name);
    assert_eq!(reused.framebuffer.get(), framebuffer);
    assert_eq!(traffic::texture_allocations(), 0);
    assert_eq!(traffic::finish_calls(), 0);
    drop(reused);
    drop(ordinary);
    gpu.collect().unwrap();
}

#[test]
fn composition_targets_keep_slots_when_persistent_canvases_fill_the_cache() {
    let context = Context::new();
    let mut counts = Vec::new();
    for entries in [0, 8] {
        let (gpu, images) = setup(&context, entries);
        let mut held = Vec::new();
        for _ in 0..8 {
            let texture = gpu.device.sample_texture(SIZE, &gpu.resident).unwrap();
            for _ in 0..2 {
                texture.framebuffer().unwrap();
            }
            held.push(texture);
        }
        gpu.resolve().unwrap();
        let target = Image {
            size: SIZE,
            device: gpu.device.clone(),
            canvas: false,
            text: false,
            main: Some(gpu.overwrite_plane(SIZE, &gpu.scratch).unwrap()),
            province: None,
        };
        traffic::reset();
        gpu.draw(
            target.plane(false).unwrap(),
            Some(images[0].plane(false).unwrap()),
            SIZE.rect(),
            &crate::drawing::Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
        )
        .unwrap();
        gpu.resolve().unwrap();
        counts.push(traffic::store_calls());
        assert_eq!(read(&gpu, &target), read(&gpu, &images[0]));
    }
    assert!(counts[0] > 0);
    assert_eq!(counts[1], 0);
    eprintln!(
        "composition stores with occupied canvas cache: {} -> {}",
        counts[0], counts[1]
    );
}
fn setup(context: &Context, entries: usize) -> (Gpu, Vec<Image>) {
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                render_target_cache_entries: entries,
                render_target_cache_bytes: 8 * 64 * 64 * 4,
                tile_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut images: Vec<_> = (0..3)
        .map(|seed| {
            let mut data = Bytes::zeroed(64 * 64 * 4, &Budget::new(64 * 64 * 4)).unwrap();
            for (i, pixel) in data
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                pixel.copy_from_slice(&[(i % 239) as u8, seed * 50, (i / 64) as u8, 173]);
            }
            gpu.upload_scaled(
                &Pixels {
                    size: SIZE,
                    main: Some(data),
                    province: None,
                },
                SIZE,
            )
            .unwrap()
        })
        .collect();
    for _ in 0..3 {
        for image in &mut images {
            gpu.fill(
                image,
                &[Fill {
                    rectangle: Rect {
                        left: 1,
                        top: 1,
                        width: 3,
                        height: 3,
                    },
                    color: 0xff123456,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
        }
    }
    gpu.resolve().unwrap();
    if entries != 0 {
        assert!(
            gpu.device
                .targets
                .borrow_mut()
                .get(images[0].main.as_ref().unwrap().tiles[0].texture.name())
                .is_some()
        );
    }
    (gpu, images)
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, SIZE.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn copy_pixels(target: &mut [u8], source: &[u8], region: Rect, x: usize, y: usize) {
    for row in 0..region.height as usize {
        let from = ((region.top as usize + row) * 64 + region.left as usize) * 4;
        let to = ((y + row) * 64 + x) * 4;
        let count = region.width as usize * 4;
        target[to..to + count].copy_from_slice(&source[from..from + count]);
    }
}

#[test]
fn cached_copy_targets_skip_intermediate_stores_and_preserve_cropped_pixels() {
    let context = Context::new();
    let mut baseline = 0;
    for entries in [0, 8] {
        let (gpu, images) = setup(&context, entries);
        let mut expected = read(&gpu, &images[0]);
        let source_pixels = read(&gpu, &images[1]);
        let source = &images[1].main.as_ref().unwrap().tiles[0].texture;
        let target = &images[0].main.as_ref().unwrap().tiles[0].texture;
        // Leave an unrelated texture on the work surface before the copies.
        images[2].main.as_ref().unwrap().tiles[0]
            .texture
            .read_framebuffer()
            .unwrap();
        traffic::reset();
        for i in 0..12 {
            let region = Rect {
                left: i,
                top: 7,
                width: 23,
                height: 19,
            };
            gpu.device
                .copy_region_at(source, region, target, 29 - i, 31)
                .unwrap();
            gpu.resolve().unwrap();
            copy_pixels(&mut expected, &source_pixels, region, (29 - i) as usize, 31);
        }
        let stores = traffic::stored_pixels();
        assert_eq!(traffic::read_calls(), 0);
        if entries == 0 {
            baseline = stores;
            assert!(baseline > 0);
        } else {
            assert_eq!(stores, 0);
            println!("cached copy work stores: {baseline} -> {stores} pixels");
        }
        assert_eq!(read(&gpu, &images[0]), expected);
        assert_eq!(read(&gpu, &images[1]), source_pixels);
    }
}

#[test]
fn copies_resolve_pending_source_writes_and_snapshot_overlapping_self_copies() {
    let context = Context::new();
    let (gpu, images) = setup(&context, 8);
    let source = &images[1].main.as_ref().unwrap().tiles[0].texture;
    let target = &images[0].main.as_ref().unwrap().tiles[0].texture;
    let mut expected = read(&gpu, &images[0]);
    let area = Rect {
        left: 4,
        top: 6,
        width: 31,
        height: 25,
    };
    let framebuffer = gpu.device.prepare_work_draw(source, area).unwrap().unwrap();
    gpu.device.work_draw_region(source, area).unwrap();
    unsafe {
        let gl = &gpu.device.gl;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(area.left, area.top, area.width as i32, area.height as i32);
        gl.color_mask(true, true, true, true);
        gl.clear_color(1.0, 0.0, 0.0, 1.0);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    gpu.device
        .copy_region_at(source, area, target, 9, 13)
        .unwrap();
    copy_pixels(
        &mut expected,
        &[255, 0, 0, 255].repeat(64 * 64),
        area,
        9,
        13,
    );
    assert_eq!(read(&gpu, &images[0]), expected);

    let region = Rect {
        left: 2,
        top: 3,
        width: 40,
        height: 42,
    };
    gpu.device
        .copy_region_at(target, region, target, 7, 8)
        .unwrap();
    let snapshot = expected.clone();
    copy_pixels(&mut expected, &snapshot, region, 7, 8);
    assert_eq!(read(&gpu, &images[0]), expected);
}

#[test]
fn cropped_copies_preserve_caller_viewport_and_skip_in_bounds_changes() {
    let context = Context::new();
    for entries in [0, 8] {
        let (gpu, images) = setup(&context, entries);
        let source = &images[1].main.as_ref().unwrap().tiles[0].texture;
        let target = &images[0].main.as_ref().unwrap().tiles[0].texture;
        let source_pixels = read(&gpu, &images[1]);
        let mut expected = read(&gpu, &images[0]);
        for viewport in [
            [0, 0, 64, 64],
            [5, 4, 55, 55],
            [-5, -7, 80, 80],
            [0, 0, 1, 1],
            [0, 0, 0, 0],
        ] {
            for i in 0..12 {
                let region = Rect {
                    left: i,
                    top: 7,
                    width: 23,
                    height: 19,
                };
                unsafe {
                    gpu.device
                        .gl
                        .viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
                }
                traffic::reset();
                gpu.device
                    .copy_region_at(source, region, target, 29 - i, 31)
                    .unwrap();
                gpu.resolve().unwrap();
                if viewport[2] >= 55 {
                    assert_eq!(traffic::viewport_calls(), 0, "viewport={viewport:?}");
                } else {
                    assert!(traffic::viewport_calls() > 0);
                }
                let mut actual = [0; 4];
                unsafe {
                    gpu.device
                        .gl
                        .get_parameter_i32_slice(glow::VIEWPORT, &mut actual);
                }
                assert_eq!(actual, viewport);
                copy_pixels(&mut expected, &source_pixels, region, (29 - i) as usize, 31);
                assert_eq!(read(&gpu, &images[0]), expected);
            }
        }
    }
}

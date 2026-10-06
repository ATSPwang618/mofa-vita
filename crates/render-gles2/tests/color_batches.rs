#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{Color, DrawFace, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn image(gpu: &Gpu, size: Size) -> Image {
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in data
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[
            (i * 7) as u8,
            (i * 13) as u8,
            (i * 19) as u8,
            37 + (i % 219) as u8,
        ]);
    }
    gpu.upload_scaled(
        &Pixels {
            size,
            main: Some(data),
            province: None,
        },
        size,
    )
    .unwrap()
}

#[test]
fn interleaved_columns_keep_pixels_and_reduce_surface_switches() {
    let context = support::Context::new();
    let size = Size {
        width: 96,
        height: 32,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 32,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let colors: Vec<_> = (0..24)
        .flat_map(|x| [x, 72 + x])
        .map(|x| Color {
            rectangle: krkr_protocol::graphics::Rect {
                left: x,
                top: 0,
                width: 1,
                height: 32,
            },
            color: 0x004492d3,
            opacity: 1 + (x * 9 % 253) as i16,
            face: DrawFace::Opaque,
        })
        .collect();
    let mut expected = image(&gpu, size);
    let mut actual = image(&gpu, size);
    assert!(gpu.color_batch_regions(&actual).is_some());
    traffic::reset();
    for c in &colors {
        gpu.color(&mut expected, c.rectangle, c.color, c.opacity, c.face)
            .unwrap();
    }
    let expected = gpu
        .readback(&expected, size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec();
    let before = (traffic::load_calls(), traffic::store_calls());
    let before_draws = traffic::draw_calls();
    traffic::reset();
    gpu.colors(&mut actual, &colors).unwrap();
    assert_eq!(traffic::texture_allocations(), 0);
    assert_eq!(traffic::read_calls(), 0);
    let actual = gpu
        .readback(&actual, size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec();
    let after = (traffic::load_calls(), traffic::store_calls());
    let after_draws = traffic::draw_calls();
    assert_eq!(actual, expected);
    assert!(
        after.0 * 4 < before.0 && after.1 * 4 < before.1,
        "before={before:?}, after={after:?}"
    );
    assert_eq!(after_draws - after.0, 2, "one color draw per touched tile");
    assert!(
        after_draws * 8 < before_draws,
        "draws: {before_draws} -> {after_draws}"
    );
    println!(
        "strip draws: {before_draws} -> {after_draws}; surface calls: {before:?} -> {after:?}"
    );
}

#[test]
fn strips_do_not_require_extra_staging_when_the_budget_is_full() {
    let context = support::Context::new();
    let size = Size {
        width: 48,
        height: 16,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut actual = image(&gpu, size);
    let mut expected = image(&gpu, size);
    let colors: Vec<_> = (0..24)
        .map(|x| Color {
            rectangle: krkr_protocol::graphics::Rect {
                left: x * 2,
                top: 0,
                width: 2,
                height: 16,
            },
            color: 0x28c2a97d,
            opacity: 1 + x as i16 * 10,
            face: DrawFace::Alpha,
        })
        .collect();
    for c in &colors {
        gpu.color(&mut expected, c.rectangle, c.color, c.opacity, c.face)
            .unwrap();
    }
    gpu.collect().unwrap();
    let occupied = gpu.staging.reserve(gpu.staging.available() - 128).unwrap();
    traffic::reset();
    gpu.colors(&mut actual, &colors).unwrap();
    assert_eq!(traffic::buffer_uploads(), 0);
    drop(occupied);
    assert_eq!(
        gpu.readback(&actual, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        gpu.readback(&expected, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
    );
}

#[test]
fn scaled_horizontal_and_vertical_strips_keep_byte_exact_signed_colors() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 32,
                canvas_limit: Some(Size {
                    width: 80,
                    height: 32,
                }),
                small_canvas_edge: 0,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let logical = Size {
        width: 101,
        height: 41,
    };
    let stored = Size {
        width: 80,
        height: 32,
    };
    for horizontal in [false, true] {
        for face in [DrawFace::Alpha, DrawFace::Opaque, DrawFace::AddAlpha] {
            let make_image = || gpu.logical_image(image(&gpu, stored), logical).unwrap();
            let mut actual = make_image();
            let mut expected = make_image();
            let colors: Vec<_> = (0..18)
                .map(|i| Color {
                    rectangle: if horizontal {
                        krkr_protocol::graphics::Rect {
                            left: -3,
                            top: i * 2,
                            width: 106,
                            height: 2,
                        }
                    } else {
                        krkr_protocol::graphics::Rect {
                            left: i * 5 - 2,
                            top: -3,
                            width: 5,
                            height: 47,
                        }
                    },
                    color: 0xf01958e3 + i as u32 * 151,
                    opacity: if face != DrawFace::AddAlpha && i % 3 == 0 {
                        -37 - i as i16
                    } else {
                        1 + i as i16 * 13
                    },
                    face,
                })
                .collect();
            assert!(gpu.color_batch_regions(&actual).is_some());
            for c in &colors {
                gpu.color(&mut expected, c.rectangle, c.color, c.opacity, c.face)
                    .unwrap();
            }
            traffic::reset();
            gpu.colors(&mut actual, &colors).unwrap();
            assert!(
                traffic::buffer_uploads() > 0,
                "strip batch was not selected"
            );
            assert_eq!(
                gpu.readback(&actual, logical.rect(), false)
                    .unwrap()
                    .data
                    .as_slice(),
                gpu.readback(&expected, logical.rect(), false)
                    .unwrap()
                    .data
                    .as_slice(),
                "horizontal={horizontal}, face={face:?}"
            );
        }
    }
}

#[test]
fn overlapping_colors_match_sequential_alpha_and_keep_snapshots() {
    let context = support::Context::new();
    let size = Size {
        width: 40,
        height: 20,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 16,
                ..Default::default()
            },
        )
        .unwrap()
    };
    for face in [DrawFace::Alpha, DrawFace::Opaque, DrawFace::AddAlpha] {
        let mut expected = image(&gpu, size);
        let mut actual = image(&gpu, size);
        let colors: Vec<_> = [
            37,
            123,
            252,
            if face == DrawFace::AddAlpha { 23 } else { -64 },
        ]
        .into_iter()
        .enumerate()
        .map(|(i, opacity)| Color {
            rectangle: krkr_protocol::graphics::Rect {
                left: (i * 3) as i32 - 2,
                top: 2,
                width: 24,
                height: 12,
            },
            color: 0x12e7395a + i as u32,
            opacity,
            face,
        })
        .collect();
        for c in &colors {
            gpu.color(&mut expected, c.rectangle, c.color, c.opacity, c.face)
                .unwrap();
        }
        gpu.colors(&mut actual, &colors).unwrap();
        assert_eq!(
            gpu.readback(&actual, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            gpu.readback(&expected, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
        );
        let snapshot = actual.shared();
        let saved = gpu
            .readback(&snapshot, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .to_vec();
        assert!(gpu.color_batch_regions(&actual).is_none());
        gpu.colors(&mut actual, &colors).unwrap();
        for c in &colors {
            gpu.color(&mut expected, c.rectangle, c.color, c.opacity, c.face)
                .unwrap();
        }
        assert_eq!(
            gpu.readback(&actual, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            gpu.readback(&expected, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
        );
        assert_eq!(
            gpu.readback(&snapshot, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            saved
        );
    }
}

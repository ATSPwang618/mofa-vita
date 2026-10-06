#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn source(gpu: &Gpu, size: Size) -> (Image, Vec<u8>) {
    let data: Vec<_> = (0..size.height)
        .flat_map(|y| (0..size.width).flat_map(move |x| [x as u8, y as u8, 71, (x + y) as u8]))
        .collect();
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    let mut bytes = Bytes::zeroed(data.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(&data);
    gpu.upload(
        &mut image,
        &Pixels {
            size,
            main: Some(bytes),
            province: None,
        },
    )
    .unwrap();
    (image, data)
}

fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn cropped_sprite_copies_share_pixels_from_a_cleared_canvas_and_preserve_snapshots() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let (mut input, data) = source(
        &gpu,
        Size {
            width: 32,
            height: 24,
        },
    );
    let size = Size {
        width: 64,
        height: 48,
    };
    let mut target = gpu.create_image(size, 0).unwrap();
    let src = Rect {
        left: 8,
        top: 4,
        width: 16,
        height: 14,
    };
    let dst = Rect {
        left: 5,
        top: 7,
        ..src
    };
    assert!(gpu.copy_is_view(&target, &input, src, dst));
    let charged = gpu.resident.used();
    traffic::reset();
    gpu.copy_rect(
        &mut target,
        &input,
        src,
        dst.left,
        dst.top,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    assert_eq!(gpu.resident.used(), charged);
    assert!(target.shares_main_storage(&input));
    let actual = pixels(&gpu, &target);
    for y in 0..size.height {
        for x in 0..size.width {
            let expected = if (5..21).contains(&x) && (7..21).contains(&y) {
                let offset = (((y - 7 + 4) * 32 + x - 5 + 8) * 4) as usize;
                &data[offset..offset + 4]
            } else {
                &[0; 4]
            };
            let offset = ((y * size.width + x) * 4) as usize;
            assert_eq!(&actual[offset..offset + 4], expected, "{x},{y}");
        }
    }
    let snapshot = target.shared();
    gpu.fill(
        &mut input,
        &[Fill {
            rectangle: src,
            color: 0xff123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &target), actual);
    gpu.fill(
        &mut target,
        &[Fill {
            rectangle: dst,
            color: 0xffabcdef,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &snapshot), actual);
    assert_ne!(pixels(&gpu, &target), actual);
}

#[test]
fn moving_clipped_sprites_keep_every_preserved_border_without_new_pixel_storage() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let (input, data) = source(
        &gpu,
        Size {
            width: 32,
            height: 24,
        },
    );
    let size = Size {
        width: 64,
        height: 48,
    };
    let clip = Rect {
        left: 8,
        top: 4,
        width: 30,
        height: 34,
    };
    for (x, y) in [(-5, 10), (5, -3), (24, 30)] {
        let mut target = gpu.create_image(size, 0x70554433).unwrap();
        let charged = gpu.resident.used();
        gpu.copy_rect(
            &mut target,
            &input,
            input.size.rect(),
            x,
            y,
            clip,
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        assert_eq!(gpu.resident.used(), charged);
        assert!(target.shares_main_storage(&input));
        let actual = pixels(&gpu, &target);
        for py in 0..size.height {
            for px in 0..size.width {
                let sx = px as i32 - x;
                let sy = py as i32 - y;
                let expected = if (8..38).contains(&px)
                    && (4..38).contains(&py)
                    && (0..32).contains(&sx)
                    && (0..24).contains(&sy)
                {
                    let offset = ((sy * 32 + sx) * 4) as usize;
                    &data[offset..offset + 4]
                } else {
                    &[0x55, 0x44, 0x33, 0x70]
                };
                let offset = ((py * size.width + px) * 4) as usize;
                assert_eq!(&actual[offset..offset + 4], expected, "{x},{y}: {px},{py}");
            }
        }
    }
}

#[test]
fn cropped_reduced_canvases_share_aligned_pixels_but_keep_density_changes_filtered() {
    let context = support::Context::new();
    for limit in [
        Size {
            width: 64,
            height: 48,
        },
        Size {
            width: 65,
            height: 49,
        },
    ] {
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: true,
                    canvas_limit: Some(limit),
                    small_canvas_edge: 0,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let (input, data) = source(
            &gpu,
            Size {
                width: 32,
                height: 24,
            },
        );
        let input = gpu
            .logical_image(
                input,
                Size {
                    width: 64,
                    height: 48,
                },
            )
            .unwrap();
        let size = Size {
            width: 128,
            height: 96,
        };
        gpu.set_canvas_size(size);
        let mut target = gpu.create_image(size, 0).unwrap();
        let src = Rect {
            left: 16,
            top: 8,
            width: 32,
            height: 28,
        };
        let dst = Rect {
            left: 10,
            top: 14,
            ..src
        };
        let aligned = limit.width == 64;
        assert_eq!(gpu.copy_is_view(&target, &input, src, dst), aligned);
        let charged = gpu.resident.used();
        gpu.copy_rect(
            &mut target,
            &input,
            src,
            dst.left,
            dst.top,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        if aligned {
            assert_eq!(gpu.resident.used(), charged);
            assert!(target.shares_main_storage(&input));
            let actual = pixels(&gpu, &target);
            for y in 0..size.height {
                for x in 0..size.width {
                    let expected = if (10..42).contains(&x) && (14..42).contains(&y) {
                        let offset = ((((y - 14 + 8) / 2) * 32 + (x - 10 + 16) / 2) * 4) as usize;
                        &data[offset..offset + 4]
                    } else {
                        &[0; 4]
                    };
                    let offset = ((y * size.width + x) * 4) as usize;
                    assert_eq!(&actual[offset..offset + 4], expected, "{x},{y}");
                }
            }
        } else {
            assert!(!target.shares_main_storage(&input));
            assert!(gpu.resident.used() > charged);
        }
    }
}

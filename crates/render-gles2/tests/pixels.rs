#![cfg(target_os = "linux")]
mod support;
use krkr_protocol::{
    graphics::{Blend, BlendOptions, DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn compact_hit_plane_keeps_logical_coordinates_with_small_staging() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                staging: krkr_protocol::budget::Budget::new(512 * 1024),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 64,
        height: 36,
    };
    let logical = Size {
        width: 1920,
        height: 1080,
    };
    let data: Vec<u8> = (0..stored.width * stored.height)
        .flat_map(|i| [23, 45, 67, (i % 251) as u8])
        .collect();
    let native = uploaded(&gpu, stored, &data, None);
    let image = gpu.logical_image(native, logical).unwrap();
    let plane = gpu.read_hit_plane(&image, false).unwrap();
    assert_eq!(plane.size, logical);
    assert_eq!(plane.bytes(), (stored.width * stored.height) as usize);
    assert_eq!(gpu.staging.used(), plane.bytes());
    for y in 0..logical.height {
        for x in 0..logical.width {
            let sx = (2 * x + 1) * stored.width / (2 * logical.width);
            let sy = (2 * y + 1) * stored.height / (2 * logical.height);
            assert_eq!(
                plane.sample(x.into(), y.into()),
                ((sy * stored.width + sx) % 251) as u8
            );
        }
    }
    assert_eq!(plane.sample(-1, 0), 0);
    assert_eq!(plane.sample(1920, 1080), 0);
    assert!(matches!(
        gpu.read_hit_plane(&image, true).unwrap().data,
        krkr_protocol::hit::Data::Empty
    ));
}

fn uploaded(
    gpu: &Gpu,
    size: Size,
    data: &[u8],
    province: Option<&[u8]>,
) -> krkr_render_gles2::Image {
    let mut main = Bytes::zeroed(data.len(), &gpu.staging).unwrap();
    main.as_mut_slice().copy_from_slice(data);
    let province = province.map(|values| {
        let mut bytes = Bytes::zeroed(values.len(), &gpu.staging).unwrap();
        bytes.as_mut_slice().copy_from_slice(values);
        bytes
    });
    let mut image = gpu.reserve_upload(size, true, province.is_some()).unwrap();
    gpu.upload(
        &mut image,
        &Pixels {
            size,
            main: Some(main),
            province,
        },
    )
    .unwrap();
    image
}

#[test]
fn dodge5_arithmetic_matches_the_legacy_table_for_every_channel_pair() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 256,
        height: 256,
    };
    let table = krkr_render::blend::lookup_table();
    let destination: Vec<_> = (0..256u32)
        .flat_map(|_| (0..256u32).flat_map(|d| [d as u8, 255 - d as u8, (d ^ 85) as u8, 137]))
        .collect();
    for (alpha, opacity, hold_alpha) in [
        (255, 255, false),
        (255, 128, true),
        (127, 254, false),
        (1, 255, true),
        (0, 255, false),
        (255, 0, false),
    ] {
        let source: Vec<_> = (0..256u32)
            .flat_map(|s| {
                (0..256u32).flat_map(move |_| [s as u8, 255 - s as u8, (s ^ 170) as u8, alpha])
            })
            .collect();
        let source_image = uploaded(&gpu, size, &source, None);
        let mut target = uploaded(&gpu, size, &destination, None);
        gpu.operate(
            &mut target,
            &source_image,
            size.rect(),
            0,
            0,
            size.rect(),
            BlendOptions {
                mode: Blend::PsColorDodge5,
                opacity,
                face: DrawFace::Opaque,
                hold_alpha,
            },
        )
        .unwrap();
        let result = gpu.readback(&target, size.rect(), false).unwrap();
        let a = if opacity == 255 {
            u32::from(alpha)
        } else {
            u32::from(alpha) * u32::from(opacity) / 256
        };
        for (i, actual) in result.data.as_slice().chunks_exact(4).enumerate() {
            let d = &destination[i * 4..i * 4 + 4];
            let s = &source[i * 4..i * 4 + 4];
            let expected = if opacity == 0 {
                [d[0], d[1], d[2], d[3]]
            } else {
                let mut rgba = [0u8; 4];
                for c in 0..3 {
                    let value = u32::from(s[c]) * a / 256;
                    rgba[c] = table[((value * 256 + u32::from(d[c])) * 4 + 1) as usize];
                }
                rgba[3] = if hold_alpha { 137 } else { 0 };
                rgba
            };
            assert_eq!(
                actual, expected,
                "pair={i} alpha={alpha} opacity={opacity} hold={hold_alpha}"
            );
        }
        drop(result);
        drop(target);
        drop(source_image);
        gpu.collect().unwrap();
    }
}

#[test]
fn window_clip_and_transparent_cursor_preserve_the_underlying_frame() {
    use glow::HasContext;
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Default::default()).unwrap() };
    let screen = Size {
        width: 64,
        height: 64,
    };
    let size = Size {
        width: 2,
        height: 2,
    };
    let background = gpu.create_image(size, 0xff204060).unwrap();
    let window = gpu.create_image(size, 0xffff0000).unwrap();
    let cursor = uploaded(
        &gpu,
        size,
        &[
            0, 255, 0, 255, 255, 255, 255, 0, 255, 255, 255, 128, 0, 0, 0, 0,
        ],
        None,
    );
    gpu.present(&background, screen, screen.rect()).unwrap();
    gpu.present_window(
        &window,
        screen,
        Rect {
            left: 10,
            top: 10,
            width: 20,
            height: 20,
        },
        Rect {
            left: 15,
            top: 15,
            width: 5,
            height: 5,
        },
    )
    .unwrap();
    gpu.present_cursor(
        &cursor,
        screen,
        Rect {
            left: 8,
            top: 8,
            ..size.rect()
        },
    )
    .unwrap();
    gpu.flush().unwrap();
    let gl = context.gl();
    for ((x, y), expected) in [
        ((10, 10), [32, 64, 96, 255]),
        ((15, 15), [255, 0, 0, 255]),
        ((20, 20), [32, 64, 96, 255]),
        ((8, 8), [0, 255, 0, 255]),
        ((9, 8), [32, 64, 96, 255]),
        ((8, 9), [144, 160, 176, 255]),
    ] {
        let mut rgba = [0; 4];
        unsafe {
            gl.read_pixels(
                x,
                63 - y,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut rgba)),
            );
        }
        assert_eq!(rgba, expected, "display pixel ({x},{y})");
    }
    // Cursor blending must not leak into later offscreen operations.
    let mut next = gpu.create_image(size, 0x10203040).unwrap();
    gpu.copy_rect(
        &mut next,
        &cursor,
        size.rect(),
        0,
        0,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    assert_eq!(gpu.pixel(&next, 1, 0, false).unwrap(), 0x00ffffff);
}

#[test]
fn bitmap_replacement_and_plane_patches_preserve_province_and_fail_atomically() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Default::default()).unwrap() };
    let size = Size {
        width: 2,
        height: 1,
    };
    let mut image = uploaded(&gpu, size, &[10, 20, 30, 40, 50, 60, 70, 80], Some(&[7, 9]));
    let mut patch = Bytes::zeroed(2, &gpu.staging).unwrap();
    patch.as_mut_slice().copy_from_slice(&[12, 14]);
    gpu.patch_pixels(
        &mut image,
        &Pixels {
            size,
            main: None,
            province: Some(patch),
        },
    )
    .unwrap();
    assert_eq!(gpu.pixel(&image, 0, 0, false).unwrap(), 0x280a141e);
    let larger = Size {
        width: 3,
        height: 2,
    };
    let bitmap = Pixels {
        size: larger,
        main: Some(Bytes::zeroed(24, &gpu.staging).unwrap()),
        province: None,
    };
    let next = gpu.assign_bitmap(Some(&image), &bitmap).unwrap();
    assert_eq!(
        gpu.readback(&next, larger.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        &[12, 14, 0, 0, 0, 0]
    );
    let bad = Pixels {
        size,
        main: Some(Bytes::zeroed(7, &gpu.staging).unwrap()),
        province: None,
    };
    assert!(gpu.patch_pixels(&mut image, &bad).is_err());
    assert_eq!(gpu.pixel(&image, 0, 0, false).unwrap(), 0x280a141e);
    assert_eq!(gpu.pixel(&image, 1, 0, true).unwrap(), 14);
}

#[test]
fn all_blend_modes_match_legacy_integer_fixtures() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 8,
                ..Config::default()
            },
        )
        .unwrap()
    };
    // Fixed legacy krkrz blend_functor_c.h fixtures, also checked by WGPU.
    // D=RGBA(80,150,220,100), S=RGBA(200,100,40,160); HDA enabled.
    let cases = [
        (1, [200, 100, 40], [140, 125, 130]),
        (2, [155, 118, 107], [117, 134, 163]),
        (3, [255, 250, 255], [180, 200, 240]),
        (4, [25, 0, 5], [53, 73, 113]),
        (5, [62, 58, 34], [71, 104, 127]),
        (8, [255, 247, 255], [131, 186, 238]),
        (9, [80, 100, 40], [80, 125, 130]),
        (10, [200, 150, 220], [140, 150, 220]),
        (11, [218, 192, 226], [150, 171, 223]),
        (12, [229, 155, 121], [154, 152, 170]),
        (13, [155, 118, 107], [117, 134, 163]),
        (14, [189, 212, 241], [134, 181, 230]),
        (15, [45, 56, 85], [62, 103, 152]),
        (16, [68, 92, 103], [74, 121, 161]),
        (17, [166, 176, 223], [123, 163, 221]),
        (18, [108, 135, 204], [94, 142, 212]),
        (19, [141, 129, 125], [110, 139, 172]),
        (20, [105, 139, 206], [92, 144, 213]),
        (21, [189, 210, 241], [134, 180, 230]),
        (22, [156, 198, 243], [105, 170, 230]),
        (23, [50, 56, 102], [65, 103, 161]),
        (24, [155, 150, 220], [117, 150, 220]),
        (25, [80, 118, 107], [80, 134, 163]),
        (26, [105, 87, 195], [92, 118, 207]),
        (27, [45, 88, 195], [18, 119, 208]),
        (28, [126, 139, 202], [103, 144, 211]),
    ];
    let size = Size {
        width: 52,
        height: 1,
    };
    let mut destination = gpu.create_image(size, 0x645096dc).unwrap();
    let source = gpu
        .create_image(
            Size {
                width: 1,
                height: 1,
            },
            0xa0c86428,
        )
        .unwrap();
    let mut expected = Vec::new();
    for (index, (mode, full, half)) in cases.into_iter().enumerate() {
        for (column, (opacity, rgb)) in [(255, full), (128, half)].into_iter().enumerate() {
            gpu.operate(
                &mut destination,
                &source,
                source.size.rect(),
                (index * 2 + column) as i32,
                0,
                size.rect(),
                BlendOptions {
                    mode: Blend::from_legacy(mode).unwrap(),
                    opacity,
                    face: DrawFace::Opaque,
                    hold_alpha: true,
                },
            )
            .unwrap();
            expected.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 100]);
        }
    }
    let actual = gpu.readback(&destination, size.rect(), false).unwrap();
    for (index, (actual, expected)) in actual
        .data
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected.as_chunks::<4>().0.iter())
        .enumerate()
    {
        assert_eq!(
            actual,
            expected,
            "mode {} opacity {}",
            cases[index / 2].0,
            if index % 2 == 0 { 255 } else { 128 }
        );
    }
}

#[test]
fn alpha_faces_color_and_presentation_keep_their_distinct_rules() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 1,
        height: 1,
    };
    let source = gpu.create_image(size, 0xa0c86428).unwrap();
    for (mode, face, opacity, hold_alpha, expected) in [
        (
            Blend::Alpha,
            DrawFace::Opaque,
            255,
            false,
            [155, 118, 107, 137],
        ),
        (
            Blend::Alpha,
            DrawFace::Alpha,
            255,
            false,
            [176, 109, 75, 198],
        ),
        (
            Blend::Alpha,
            DrawFace::Alpha,
            128,
            true,
            [144, 123, 123, 149],
        ),
        (
            Blend::Alpha,
            DrawFace::AddAlpha,
            255,
            true,
            [154, 117, 106, 198],
        ),
        (
            Blend::AddAlpha,
            DrawFace::Alpha,
            255,
            false,
            [80, 150, 220, 100],
        ),
        (
            Blend::AddAlpha,
            DrawFace::AddAlpha,
            128,
            true,
            [154, 152, 170, 149],
        ),
        (
            Blend::Opaque,
            DrawFace::Alpha,
            255,
            true,
            [200, 100, 40, 255],
        ),
        (
            Blend::Opaque,
            DrawFace::AddAlpha,
            128,
            true,
            [239, 174, 149, 178],
        ),
        (
            Blend::PsDifference5,
            DrawFace::Mask,
            255,
            false,
            [45, 88, 195, 100],
        ),
    ] {
        let mut target = gpu.create_image(size, 0x645096dc).unwrap();
        gpu.operate(
            &mut target,
            &source,
            size.rect(),
            0,
            0,
            size.rect(),
            BlendOptions {
                mode,
                face,
                opacity,
                hold_alpha,
            },
        )
        .unwrap();
        assert_eq!(
            gpu.readback(&target, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            expected,
            "{mode:?}/{face:?}/{opacity}"
        );
    }
    for (face, opacity, expected) in [
        (DrawFace::Opaque, 128, [139, 124, 129, 100]),
        (DrawFace::Alpha, 128, [165, 114, 91, 179]),
        (DrawFace::AddAlpha, 128, [139, 124, 129, 178]),
        (DrawFace::Alpha, -128, [80, 150, 220, 49]),
        (DrawFace::Alpha, -255, [80, 150, 220, 0]),
        (DrawFace::Opaque, -1, [79, 149, 219, 100]),
        (DrawFace::Opaque, 255, [200, 100, 40, 100]),
        (DrawFace::Alpha, 255, [200, 100, 40, 255]),
        (DrawFace::Mask, 0, [80, 150, 220, 40]),
    ] {
        let mut target = gpu.create_image(size, 0x645096dc).unwrap();
        gpu.color(&mut target, size.rect(), 0xc86428, opacity, face)
            .unwrap();
        assert_eq!(
            gpu.readback(&target, size.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            expected,
            "color/{face:?}/{opacity}"
        );
    }
    let image = uploaded(
        &gpu,
        Size {
            width: 1,
            height: 2,
        },
        &[255, 0, 0, 255, 0, 0, 255, 255],
        None,
    );
    gpu.present(
        &image,
        Size {
            width: 64,
            height: 64,
        },
        Rect {
            left: 8,
            top: 16,
            width: 32,
            height: 32,
        },
    )
    .unwrap();
    // GL's default framebuffer reads bottom-first. Script row zero is at top.
    use glow::HasContext;
    let gl = context.gl();
    let mut rgba = [0_u8; 4];
    for (x, y, expected) in [
        (10, 46, [255, 0, 0, 255]),
        (10, 18, [0, 0, 255, 255]),
        (0, 0, [0, 0, 0, 255]),
    ] {
        unsafe {
            gl.read_pixels(
                x,
                y,
                1,
                1,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut rgba)),
            );
        }
        assert_eq!(rgba, expected);
    }
}

#[test]
fn tiles_preserve_upload_orientation_clipping_shared_images_and_province() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 5,
        height: 3,
    };
    let data: Vec<u8> = (0..15).flat_map(|i| [i, 255 - i, 40, 80 + i]).collect();
    let province: Vec<u8> = (100..115).collect();
    let mut image = uploaded(&gpu, size, &data, Some(&province));
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        data
    );
    assert_eq!(
        gpu.readback(&image, size.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        province
    );
    let captured = image.shared();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 1,
                top: 1,
                width: 3,
                height: 1,
            },
            color: 33,
            face: DrawFace::Mask,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(
        gpu.readback(&captured, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        data
    );
    let mut changed = data.clone();
    for x in 1..4 {
        changed[(5 + x) * 4 + 3] = 33;
    }
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        changed
    );
    gpu.copy_rect(
        &mut image,
        &captured,
        Rect {
            left: -1,
            top: 0,
            width: 6,
            height: 3,
        },
        -2,
        1,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    for y in 1..3 {
        for x in 0..4 {
            changed[(y * 5 + x) * 4..(y * 5 + x + 1) * 4]
                .copy_from_slice(&data[((y - 1) * 5 + x + 1) * 4..((y - 1) * 5 + x + 2) * 4]);
        }
    }
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        changed
    );
    assert_eq!(
        gpu.readback(&image, size.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        province
    );
    let resized = gpu
        .resize(
            &image,
            Size {
                width: 7,
                height: 4,
            },
            0xffaabbcc,
        )
        .unwrap();
    assert_eq!(gpu.pixel(&resized, 6, 3, false).unwrap(), 0xffaabbcc);
    assert_eq!(gpu.pixel(&resized, 6, 3, true).unwrap(), 0);
    assert_eq!(
        gpu.readback(&resized, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        changed
    );
    drop((image, captured, resized));
    assert!(gpu.resident.used() > 256 * 321 * 4);
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), 256 * 321 * 4);
    assert_eq!(gpu.scratch.used(), 0);
    assert_eq!(gpu.staging.used(), 0);
}

#[test]
fn compact_image_reads_and_partial_edit_use_logical_coordinates() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let small = uploaded(
        &gpu,
        Size {
            width: 2,
            height: 2,
        },
        &[
            255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 64, 255, 255, 255, 32,
        ],
        None,
    );
    let mut image = gpu
        .logical_image(
            small,
            Size {
                width: 8,
                height: 8,
            },
        )
        .unwrap();
    let capture = image.shared();
    assert_eq!(image.resident_bytes(), 16);
    assert_eq!(gpu.pixel(&image, 0, 0, false).unwrap(), 0xffff0000);
    assert_eq!(gpu.pixel(&image, 7, 0, false).unwrap(), 0x8000ff00);
    assert_eq!(gpu.pixel(&image, 3, 7, false).unwrap(), 0x400000ff);
    assert_eq!(image.resident_bytes(), 16);
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 7,
                top: 7,
                width: 1,
                height: 1,
            },
            color: 0xffaabbcc,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&image, 7, 7, false).unwrap(), 0xffaabbcc);
    assert_eq!(gpu.pixel(&image, 6, 7, false).unwrap(), 0x20ffffff);
    assert_eq!(gpu.pixel(&capture, 7, 7, false).unwrap(), 0x20ffffff);
    assert_eq!(capture.resident_bytes(), 16);
}
#[test]
fn regional_upload_preserves_neighbors_snapshots_and_province() {
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };

    let size = Size {
        width: 8,
        height: 6,
    };
    let rectangle = Rect {
        left: 3,
        top: 2,
        width: 3,
        height: 2,
    };
    let patch_size = Size {
        width: rectangle.width,
        height: rectangle.height,
    };
    let mut data =
        krkr_protocol::pixels::Bytes::zeroed(patch_size.rgba_bytes().unwrap(), &gpu.staging)
            .unwrap();
    let patch_bytes = [
        17, 31, 47, 0, 11, 99, 27, 127, 255, 128, 1, 255, 63, 8, 99, 3, 171, 19, 39, 201, 31, 97,
        211, 93,
    ];
    data.as_mut_slice().copy_from_slice(&patch_bytes);
    let patch = krkr_protocol::pixels::Pixels {
        size: patch_size,
        main: Some(data),
        province: None,
    };
    for compact in [false, true] {
        let stored = if compact {
            Size {
                width: 2,
                height: 2,
            }
        } else {
            size
        };
        let source = gpu.create_image(stored, 0x80112233).unwrap();
        let mut target = gpu.logical_image(source, size).unwrap();
        gpu.fill(
            &mut target,
            &[Fill {
                rectangle: size.rect(),
                color: 73,
                face: DrawFace::Province,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let snapshot = target.shared();
        let original = gpu
            .readback(&target, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .to_vec();
        // Contiguous GLES uploads need no extra CPU staging. Exhaust texture
        // storage to test admission failure before any target pixels change.
        let resident = std::mem::replace(&mut gpu.resident, krkr_protocol::budget::Budget::new(0));
        assert!(gpu.patch_region(&mut target, rectangle, &patch).is_err());
        gpu.resident = resident;
        assert_eq!(
            gpu.readback(&target, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec(),
            original,
            "failed patch must preserve pixels"
        );
        assert!(
            gpu.patch_region(
                &mut target,
                Rect {
                    left: -1,
                    ..rectangle
                },
                &patch
            )
            .is_err()
        );
        gpu.patch_region(&mut target, rectangle, &patch).unwrap();
        let mut expected = original.clone();
        for row in 0..rectangle.height as usize {
            let dst = ((rectangle.top as usize + row) * size.width as usize
                + rectangle.left as usize)
                * 4;
            let src = row * rectangle.width as usize * 4;
            expected[dst..dst + 12].copy_from_slice(&patch_bytes[src..src + 12]);
        }
        assert_eq!(
            gpu.readback(&target, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec(),
            expected,
            "region write must copy straight RGBA exactly"
        );
        assert_eq!(
            gpu.readback(&snapshot, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec(),
            original,
            "shared source changed"
        );
        assert_eq!(
            gpu.readback(&target, size.rect(), true)
                .unwrap()
                .data
                .as_slice()
                .to_vec(),
            vec![73; 48],
            "main patch changed province pixels"
        );
    }
}

#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, BlendOptions, DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
    sprites::{Sprite, Sprites},
    transform::{Filter, ImageOperation, Sampling, Transform},
};
use krkr_render_gles2::{Config, Gpu, Image};
fn pixels(gpu: &Gpu, size: Size, data: &[u8]) -> Pixels {
    let mut bytes = Bytes::zeroed(data.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(data);
    Pixels {
        size,
        main: Some(bytes),
        province: None,
    }
}
fn upload(gpu: &Gpu, size: Size, data: &[u8]) -> Image {
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(&mut image, &pixels(gpu, size, data)).unwrap();
    image
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn data(size: Size) -> Vec<u8> {
    (0..size.width * size.height)
        .flat_map(|i| {
            [
                (i * 37 + 11) as u8,
                (i * 53 + 21) as u8,
                (i * 19 + 7) as u8,
                (i * 23 + 40) as u8,
            ]
        })
        .collect()
}

#[test]
fn particle_trails_clear_together_without_switching_tiles_per_particle() {
    let context = support::Context::new();
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
    let size = Size {
        width: 64,
        height: 32,
    };
    let original = data(size);
    let source = gpu
        .create_image(
            Size {
                width: 1,
                height: 1,
            },
            0xff112233,
        )
        .unwrap();
    let clear = (0..32)
        .map(|i| Rect {
            left: (i % 2) * 32 + (i / 2 % 4) * 6,
            top: (i / 8) * 6,
            width: 8,
            height: 8,
        })
        .collect();
    let batch = Sprites {
        clear,
        sprites: vec![],
        _permit: gpu.staging.reserve(1024).unwrap(),
    };
    let clip = Rect {
        left: 3,
        top: 2,
        width: 58,
        height: 27,
    };
    for hold_alpha in [false, true] {
        let mut target = upload(&gpu, size, &original);
        gpu.collect().unwrap();
        traffic::reset();
        gpu.draw_sprites(
            &mut target,
            &source,
            &batch,
            clip,
            BlendOptions {
                mode: Blend::Alpha,
                face: if hold_alpha {
                    DrawFace::Opaque
                } else {
                    DrawFace::Alpha
                },
                opacity: 255,
                hold_alpha,
            },
        )
        .unwrap();
        gpu.resolve().unwrap();
        assert_eq!(
            traffic::clear_calls(),
            0,
            "small particle clears should share geometry"
        );
        assert!(
            traffic::store_calls() <= 2,
            "each tile should publish its clears together"
        );
        let mut expected = original.clone();
        for area in batch.clear.iter().filter_map(|r| r.intersection(clip)) {
            for y in area.top as u32..area.top as u32 + area.height {
                for x in area.left as u32..area.left as u32 + area.width {
                    let at = ((y * size.width + x) * 4) as usize;
                    expected[at..at + if hold_alpha { 3 } else { 4 }].fill(0);
                }
            }
        }
        assert_eq!(read(&gpu, &target), expected);
    }
}

#[test]
fn wrapped_copy_uses_absolute_coordinates_negative_shifts_and_compact_tiles() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 4,
        height: 3,
    };
    let logical = Size {
        width: 8,
        height: 6,
    };
    let bytes = data(stored);
    let source = gpu
        .logical_image(upload(&gpu, stored, &bytes), logical)
        .unwrap();
    let size = Size {
        width: 11,
        height: 8,
    };
    let mut target = gpu.create_image(size, 0x80504030).unwrap();
    let rect = Rect {
        left: 1,
        top: 1,
        width: 5,
        height: 3,
    };
    let dest = Rect {
        left: 2,
        top: 1,
        width: 8,
        height: 6,
    };
    let clip = Rect {
        left: 3,
        top: 2,
        width: 6,
        height: 5,
    };
    let shift = (i32::MIN, -6);
    gpu.copy_wrapped(&mut target, &source, rect, dest, shift, clip)
        .unwrap();
    let mut expected = [80, 64, 48, 128].repeat((size.width * size.height) as usize);
    for y in 2..7 {
        for x in 3..9 {
            let sx = 1 + (i64::from(x) + i64::from(shift.0)).rem_euclid(5);
            let sy = 1 + (i64::from(y) + i64::from(shift.1)).rem_euclid(3);
            let at = ((sy / 2) * 4 + sx / 2) as usize * 4;
            let to = (y * 11 + x) as usize * 4;
            expected[to..to + 4].copy_from_slice(&bytes[at..at + 4]);
        }
    }
    assert_eq!(read(&gpu, &target), expected);
    let snapshot = target.shared();
    gpu.copy_wrapped(
        &mut target,
        &snapshot,
        size.rect(),
        size.rect(),
        (-11, 1),
        size.rect(),
    )
    .unwrap();
    let actual = read(&gpu, &target);
    for y in 0..8 {
        for x in 0..11 {
            let to = (y * 11 + x) * 4;
            let from = (((y + 1) % 8) * 11 + x) * 4;
            assert_eq!(&actual[to..to + 4], &expected[from..from + 4]);
        }
    }
    assert_eq!(read(&gpu, &snapshot), expected);
}

#[test]
fn decoder_upload_combines_right_blue_mask_and_preserves_unwritten_planes() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 4,
        height: 4,
    };
    let source_size = Size {
        width: 5,
        height: 3,
    };
    let bytes = data(source_size);
    let mut target = gpu.reserve_upload(size, true, true).unwrap();
    gpu.fill(
        &mut target,
        &[
            Fill {
                rectangle: size.rect(),
                color: 0x80504030,
                face: DrawFace::Alpha,
                hold_alpha: false,
            },
            Fill {
                rectangle: size.rect(),
                color: 19,
                face: DrawFace::Province,
                hold_alpha: false,
            },
        ],
    )
    .unwrap();
    let frame = pixels(&gpu, source_size, &bytes);
    let snapshot = target.shared();
    gpu.copy_pixels(
        &mut target,
        &frame,
        true,
        Size {
            width: 3,
            height: 2,
        },
    )
    .unwrap();
    let mut expected = [80, 64, 48, 128].repeat(16);
    for y in 0..2 {
        for x in 0..2 {
            let to = (y * 4 + x) * 4;
            let from = (y * 5 + x) * 4;
            expected[to..to + 3].copy_from_slice(&bytes[from..from + 3]);
            expected[to + 3] = bytes[(y * 5 + x + 2) * 4 + 2];
        }
    }
    assert_eq!(read(&gpu, &target), expected);
    assert_eq!(read(&gpu, &snapshot), [80, 64, 48, 128].repeat(16));
    assert_eq!(
        gpu.readback(&target, size.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        &[19; 16]
    );
    gpu.copy_pixels(
        &mut target,
        &frame,
        false,
        Size {
            width: 3,
            height: 1,
        },
    )
    .unwrap();
    expected[..12].copy_from_slice(&bytes[..12]);
    assert_eq!(read(&gpu, &target), expected);
}

#[test]
fn nearest_affine_and_opaque_sprites_sample_compact_atlas_without_scratch_textures() {
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 3,
        height: 2,
    };
    let logical = Size {
        width: 6,
        height: 4,
    };
    let bytes = data(stored);
    let source = gpu
        .logical_image(upload(&gpu, stored, &bytes), logical)
        .unwrap();
    let size = Size {
        width: 8,
        height: 6,
    };
    let mut target = gpu.create_image(size, 0).unwrap();
    let points = [[0.5, 0.5], [6.5, 0.5], [0.5, 4.5]];
    let scratch = gpu.scratch.clone();
    gpu.scratch = Budget::new(0);
    gpu.transform(
        &mut target,
        &source,
        logical.rect(),
        Transform::Affine(points),
        Sampling {
            filter: Filter::Nearest,
            sharpness: 0.,
            no_clip: false,
        },
        ImageOperation::Copy { hold_alpha: false },
        size.rect(),
        None,
    )
    .unwrap();
    let copy = gpu.create_image(size, 0).unwrap();
    let batch = Sprites {
        clear: vec![],
        sprites: vec![Sprite {
            source: logical.rect(),
            points,
            opacity: 255,
        }],
        _permit: gpu.staging.reserve(128).unwrap(),
    };
    let mut sprites = copy;
    gpu.draw_sprites(
        &mut sprites,
        &source,
        &batch,
        size.rect(),
        BlendOptions {
            mode: Blend::Opaque,
            face: DrawFace::Opaque,
            opacity: 0,
            hold_alpha: false,
        },
    )
    .unwrap();
    assert_eq!(gpu.scratch.used(), 0);
    gpu.scratch = scratch;
    let mut expected = vec![0; 8 * 6 * 4];
    for y in 0..4 {
        for x in 0..6 {
            let from = ((y / 2) * 3 + x / 2) * 4;
            let to = ((y + 1) * 8 + x + 1) * 4;
            expected[to..to + 4].copy_from_slice(&bytes[from..from + 4]);
        }
    }
    assert_eq!(read(&gpu, &target), expected);
    assert_eq!(read(&gpu, &sprites), expected);
}

#[test]
fn sprite_batch_order_clear_and_per_sprite_opacity_keep_legacy_blend_rules() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let source = upload(
        &gpu,
        Size {
            width: 1,
            height: 1,
        },
        &[200, 100, 40, 160],
    );
    let size = Size {
        width: 3,
        height: 1,
    };
    let mut target = gpu.create_image(size, 0x645096dc).unwrap();
    let sprite = |x: f64, opacity| Sprite {
        source: source.size.rect(),
        points: [[x - 0.5, -0.5], [x + 0.5, -0.5], [x - 0.5, 0.5]],
        opacity,
    };
    let batch = Sprites {
        clear: vec![Rect {
            left: 2,
            top: 0,
            width: 1,
            height: 1,
        }],
        sprites: vec![sprite(0., 128), sprite(1., 128), sprite(1., 128)],
        _permit: gpu.staging.reserve(512).unwrap(),
    };
    gpu.draw_sprites(
        &mut target,
        &source,
        &batch,
        size.rect(),
        BlendOptions {
            mode: Blend::Additive,
            face: DrawFace::Opaque,
            opacity: 255,
            hold_alpha: true,
        },
    )
    .unwrap();
    assert_eq!(
        read(&gpu, &target),
        [180, 200, 240, 100, 255, 250, 255, 100, 0, 0, 0, 100]
    );
}

#[test]
fn first_province_write_is_zero_initialized_and_copying_an_absent_plane_clears_only_the_roi() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 3,
        height: 2,
    };
    let logical = Size {
        width: 6,
        height: 4,
    };
    let bytes = data(stored);
    let mut target = gpu
        .logical_image(upload(&gpu, stored, &bytes), logical)
        .unwrap();
    let original = target.shared();
    assert_eq!(target.write_bytes(true), 6 * 4 * 4);
    let roi = Rect {
        left: 1,
        top: 1,
        width: 4,
        height: 2,
    };
    gpu.fill(
        &mut target,
        &[Fill {
            rectangle: roi,
            color: 91,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let saved = target.shared();
    gpu.copy_rect(
        &mut target,
        &original,
        Rect {
            left: 0,
            top: 0,
            width: 2,
            height: 3,
        },
        2,
        0,
        logical.rect(),
        DrawFace::Province,
        false,
    )
    .unwrap();
    assert_eq!(target.stored_size(), Some(stored));
    assert!(!original.has_province());
    let mut expected = [0u8; 24];
    for y in 1..3 {
        for x in 1..5 {
            expected[y * 6 + x] = 91;
        }
    }
    assert_eq!(
        gpu.readback(&saved, logical.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        &expected
    );
    for y in 0..3 {
        expected[y * 6 + 2..y * 6 + 4].fill(0);
    }
    assert_eq!(
        gpu.readback(&target, logical.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        &expected
    );
    assert_eq!(read(&gpu, &target), read(&gpu, &original));
}

#[test]
fn work_surface_preserves_images_and_copy_on_write() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 4,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 9,
        height: 7,
    };
    let expected = data(size);
    let images: Vec<_> = (0..8).map(|_| upload(&gpu, size, &expected)).collect();
    // Many image tiles share one work FBO; switching targets preserves pixels.
    for image in &images {
        assert_eq!(read(&gpu, image), expected);
    }
    let mut changed = images[0].shared();
    gpu.fill(
        &mut changed,
        &[Fill {
            rectangle: Rect {
                left: 2,
                top: 1,
                width: 5,
                height: 4,
            },
            color: 0x7f123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let mut modified = expected.clone();
    for y in 1..5 {
        for x in 2..7 {
            let at = (y * size.width as usize + x) * 4;
            modified[at..at + 4].copy_from_slice(&[0x12, 0x34, 0x56, 0x7f]);
        }
    }
    assert_eq!(read(&gpu, &changed), modified);
    for image in &images {
        assert_eq!(read(&gpu, image), expected);
    }
    gpu.collect().unwrap();
    assert_eq!(gpu.scratch.used(), 4 * 4 * 4);
}

#[test]
fn hd_canvas_padding_preserves_pixels_with_one_work_surface() {
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
    // The exact failing Vita sequence: four tiles remain alive while resize
    // allocates replacement tiles for a height rounded up by the game to 1088.
    let original = gpu
        .create_image(
            Size {
                width: 1920,
                height: 1080,
            },
            0xff123456,
        )
        .unwrap();
    let resized = gpu
        .resize(
            &original,
            Size {
                width: 1920,
                height: 1088,
            },
            0xffabcdef,
        )
        .unwrap();
    for (image, y, color) in [
        (&original, 1079, [0x12, 0x34, 0x56, 0xff]),
        (&resized, 1079, [0x12, 0x34, 0x56, 0xff]),
        (&resized, 1080, [0xab, 0xcd, 0xef, 0xff]),
        (&resized, 1087, [0xab, 0xcd, 0xef, 0xff]),
    ] {
        for x in [0, 1023, 1024, 1919] {
            let pixel = gpu
                .readback(
                    image,
                    Rect {
                        left: x,
                        top: y,
                        width: 1,
                        height: 1,
                    },
                    false,
                )
                .unwrap();
            assert_eq!(pixel.data.as_slice(), &color);
        }
    }
}

#[test]
fn compact_canvas_resize_and_edits_keep_display_density() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let logical = Size {
        width: 1920,
        height: 1080,
    };
    gpu.set_canvas_size(logical);
    let original = gpu.create_image(logical, 0xff123456).unwrap();
    assert_eq!(original.size, logical);
    assert_eq!(
        original.stored_size(),
        Some(Size {
            width: 1,
            height: 1
        })
    );
    assert_eq!(original.resident_bytes(), 4);
    let padded = Size {
        width: 1920,
        height: 1088,
    };
    let mut changed = gpu.resize(&original, padded, 0xffabcdef).unwrap();
    assert_eq!(
        changed.stored_size(),
        Some(Size {
            width: 960,
            height: 544
        })
    );
    assert_eq!(gpu.pixel(&changed, 1919, 1079, false).unwrap(), 0xff123456);
    assert_eq!(gpu.pixel(&changed, 1919, 1080, false).unwrap(), 0xffabcdef);
    let previous = changed.shared();
    gpu.fill(
        &mut changed,
        &[Fill {
            rectangle: Rect {
                left: 960,
                top: 0,
                width: 960,
                height: 1088,
            },
            color: 0xff765432,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.pixel(&changed, 958, 0, false).unwrap(), 0xff123456);
    assert_eq!(gpu.pixel(&changed, 960, 0, false).unwrap(), 0xff765432);
    assert_eq!(gpu.pixel(&previous, 960, 0, false).unwrap(), 0xff123456);
    // A shared cropped half may retain the old allocation plus one constant
    // texel, without allocating another physical half or full canvas.
    assert!(changed.resident_bytes() <= 960 * 544 * 4 + 4);
    // Scratch composition is already physical and must not shrink a second time.
    let mut scratch = gpu
        .create_surface_image(Size {
            width: 960,
            height: 544,
        })
        .unwrap();
    let rectangle = scratch.size.rect();
    gpu.fill(
        &mut scratch,
        &[Fill {
            rectangle,
            color: 0xffeeeeee,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(scratch.stored_size(), Some(scratch.size));
    gpu.collect().unwrap();
}

#[test]
fn compact_upload_reuses_prepared_storage_and_does_not_shrink_on_repeated_edits() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 35,
        height: 12,
    };
    let logical = Size {
        width: 71,
        height: 25,
    };
    let data = pixels(&gpu, stored, &data(stored));
    let mut image = gpu.reserve_upload(stored, true, false).unwrap();
    let allocated = gpu.resident.used();
    gpu.upload_scaled_into(&mut image, &data, logical).unwrap();
    assert_eq!(gpu.resident.used(), allocated);
    for _ in 0..16 {
        gpu.color(&mut image, logical.rect(), 0xff102030, 128, DrawFace::Alpha)
            .unwrap();
        assert_eq!(image.size, logical);
        assert_eq!(image.stored_size(), Some(stored));
    }
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), allocated);
}

#[test]
fn work_backdrop_matches_direct_fbo_without_per_blend_scratch_allocations() {
    fn render(work: bool) -> Vec<u8> {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 4,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 9,
            height: 7,
        };
        let bytes = data(size);
        let source = upload(&gpu, size, &bytes);
        let mut target = source.shared();
        let area = Rect {
            left: 1,
            top: 1,
            width: 7,
            height: 5,
        };
        // The first write copies shared tiles; later blends reuse their backing.
        for _ in 0..3 {
            gpu.color(&mut target, area, 0xff204080, 147, DrawFace::Alpha)
                .unwrap();
            gpu.copy_rect(
                &mut target,
                &source,
                area,
                0,
                2,
                size.rect(),
                DrawFace::Mask,
                false,
            )
            .unwrap();
        }
        if work {
            assert_eq!(gpu.scratch.used(), 4 * 4 * 4);
        }
        assert_eq!(read(&gpu, &source), bytes);
        read(&gpu, &target)
    }
    assert_eq!(render(true), render(false));
}

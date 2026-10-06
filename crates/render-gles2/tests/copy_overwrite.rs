#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{DrawFace, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn patterned(gpu: &Gpu, size: Size) -> Image {
    let mut data = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in data.as_mut_slice().chunks_exact_mut(4).enumerate() {
        p.copy_from_slice(&[
            (i * 37) as u8,
            (i * 11) as u8,
            (i * 7) as u8,
            (i * 23) as u8,
        ]);
    }
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(
        &mut image,
        &Pixels {
            size,
            main: Some(data),
            province: None,
        },
    )
    .unwrap();
    image
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn smaller_tiles_bound_sparse_shared_effect_canvas_copies() {
    let render = |tile_edge| {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge,
                    work_framebuffer: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 1050,
            height: 630,
        };
        let source = patterned(
            &gpu,
            Size {
                width: 32,
                height: 32,
            },
        );
        let original = patterned(&gpu, size);
        let original_pixels = read(&gpu, &original);
        let mut output = original.shared();
        gpu.collect().unwrap();
        let before = gpu.resident.used();
        traffic::reset();
        gpu.copy_rect(
            &mut output,
            &source,
            source.size.rect(),
            48,
            48,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        gpu.flush().unwrap();
        let extra = gpu.resident.used() - before;
        let loads = traffic::loaded_pixels();
        let result = read(&gpu, &output);
        assert_eq!(read(&gpu, &original), original_pixels);
        (result, extra, loads)
    };
    let (before, large_bytes, large_loads) = render(1024);
    let (after, small_bytes, small_loads) = render(512);
    assert_eq!(after, before);
    assert_eq!(large_bytes, 1024 * 630 * 4);
    assert_eq!(small_bytes, 512 * 512 * 4);
    assert!(small_loads < large_loads);
}

#[test]
fn identical_edits_to_shared_canvases_reuse_results_and_track_all_three_versions() {
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
        width: 80,
        height: 60,
    };
    let original = patterned(&gpu, size);
    let mut stamp = patterned(
        &gpu,
        Size {
            width: 9,
            height: 7,
        },
    );
    let copy = |output: &mut Image, source: &Image| {
        gpu.copy_rect(
            output,
            source,
            source.size.rect(),
            5,
            6,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        gpu.flush().unwrap();
    };
    let mut first = original.shared();
    copy(&mut first, &stamp);
    let expected = read(&gpu, &first);
    let mut second = original.shared();
    let before = gpu.resident.used();
    traffic::reset();
    copy(&mut second, &stamp);
    assert_eq!(traffic::draw_calls(), 0);
    assert_eq!(gpu.resident.used(), before);
    assert_eq!(read(&gpu, &second), expected);
    // A result edit must not mutate another owner or validate stale pixels.
    gpu.fill(
        &mut first,
        &[krkr_protocol::graphics::Fill {
            rectangle: size.rect(),
            color: 0xffabcdef,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(read(&gpu, &second), expected);
    // Changing either input invalidates the corresponding memoized result.
    let stamp_area = stamp.size.rect();
    gpu.fill(
        &mut stamp,
        &[krkr_protocol::graphics::Fill {
            rectangle: stamp_area,
            color: 0xff314159,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let mut third = original.shared();
    copy(&mut third, &stamp);
    assert_eq!(gpu.pixel(&third, 5, 6, false).unwrap(), 0xff314159);
    let mut changed = original.shared();
    gpu.fill(
        &mut changed,
        &[krkr_protocol::graphics::Fill {
            rectangle: Rect {
                left: 70,
                top: 50,
                width: 1,
                height: 1,
            },
            color: 0xff123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let mut fourth = changed.shared();
    copy(&mut fourth, &stamp);
    assert_eq!(gpu.pixel(&fourth, 70, 50, false).unwrap(), 0xff123456);
    assert_eq!(gpu.pixel(&fourth, 5, 6, false).unwrap(), 0xff314159);
}

#[test]
fn replacing_a_shared_frame_copies_only_the_new_picture() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 960,
            height: 544,
        };
        let old = patterned(&gpu, size);
        let original = read(&gpu, &old);
        // The crop deliberately prevents the existing full-plane sharing path.
        let source = patterned(&gpu, Size { width: 961, ..size });
        let mut baseline = old.shared();
        gpu.flush().unwrap();
        traffic::reset();
        gpu.independ(&mut baseline, false, true).unwrap();
        gpu.copy_rect(
            &mut baseline,
            &source,
            size.rect(),
            0,
            0,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        gpu.flush().unwrap();
        let before = (traffic::loaded_pixels(), traffic::stored_pixels());
        let expected = read(&gpu, &baseline);
        let mut optimized = old.shared();
        traffic::reset();
        gpu.copy_rect(
            &mut optimized,
            &source,
            size.rect(),
            0,
            0,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        gpu.flush().unwrap();
        let after = (traffic::loaded_pixels(), traffic::stored_pixels());
        assert_eq!(traffic::read_calls(), 0);
        if work {
            assert_eq!(before.0 - after.0, 960 * 544);
            // The old work-surface copy can be discarded before its store,
            // but still loaded all old pixels unnecessarily.
            assert!(after.1 <= before.1);
        } else {
            assert_eq!(before.1 - after.1, 960 * 544);
        }
        assert_eq!(read(&gpu, &optimized), expected);
        assert_eq!(read(&gpu, &old), original);
        eprintln!("work={work}: shared frame copy load/store pixels {before:?} -> {after:?}");
    }
}

#[test]
fn compact_replacement_skips_old_resampling_and_keeps_source_grid() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    tile_edge: 8,
                    canvas_limit: Some(Size {
                        width: 8,
                        height: 8,
                    }),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let logical = Size {
            width: 37,
            height: 23,
        };
        gpu.set_canvas_size(logical);
        let old = gpu
            .logical_image(
                patterned(
                    &gpu,
                    Size {
                        width: 32,
                        height: 24,
                    },
                ),
                logical,
            )
            .unwrap();
        let original = read(&gpu, &old);
        let source = patterned(
            &gpu,
            Size {
                width: 41,
                height: 27,
            },
        );
        let src = Rect {
            left: 1,
            top: 2,
            ..logical.rect()
        };
        let mut reference = gpu.create_image(logical, 0).unwrap();
        gpu.flush().unwrap();
        traffic::reset();
        gpu.copy_rect(
            &mut reference,
            &source,
            src,
            0,
            0,
            logical.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        gpu.flush().unwrap();
        let draws = traffic::draw_calls();
        let expected = read(&gpu, &reference);
        let mut image = old.shared();
        traffic::reset();
        gpu.copy_rect(
            &mut image,
            &source,
            src,
            0,
            0,
            logical.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        gpu.flush().unwrap();
        assert!(
            traffic::draw_calls() <= draws + usize::from(work),
            "no discarded-image resampling pass"
        );
        assert_eq!(read(&gpu, &image), expected);
        assert_eq!(read(&gpu, &old), original);
    }
}

#[test]
fn partial_copies_preserve_tile_edges_channels_and_aliases() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 8,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 24,
            height: 16,
        };
        let source = patterned(&gpu, size);
        let source_bytes = read(&gpu, &source);
        let old = gpu.create_image(size, 0x784512ab).unwrap();
        let original = read(&gpu, &old);
        for (face, hold) in [
            (DrawFace::Alpha, false),
            (DrawFace::Mask, false),
            (DrawFace::Opaque, true),
        ] {
            let mut image = old.shared();
            let rectangle = Rect {
                left: 1,
                top: 2,
                width: 20,
                height: 12,
            };
            let clip = Rect {
                left: 2,
                top: 3,
                width: 20,
                height: 11,
            };
            gpu.copy_rect(&mut image, &source, rectangle, 2, 2, clip, face, hold)
                .unwrap();
            let mut expected = original.clone();
            for y in 3..14usize {
                for x in 2..22usize {
                    let to = (y * 24 + x) * 4;
                    let from = (y * 24 + x - 1) * 4;
                    let channels = match face {
                        DrawFace::Mask => 3..4,
                        DrawFace::Opaque => 0..3,
                        _ => 0..4,
                    };
                    for c in channels {
                        expected[to + c] = source_bytes[from + c];
                    }
                }
            }
            assert_eq!(read(&gpu, &image), expected);
            assert_eq!(read(&gpu, &old), original);
        }
        let mut image = old.shared();
        gpu.collect().unwrap();
        let _pressure = gpu
            .resident
            .reserve(gpu.resident.available() - 256)
            .unwrap();
        assert!(
            gpu.copy_rect(
                &mut image,
                &source,
                Rect {
                    width: 23,
                    ..size.rect()
                },
                1,
                0,
                size.rect(),
                DrawFace::Alpha,
                false
            )
            .is_err()
        );
        assert_eq!(read(&gpu, &image), original);
    }
}

#[test]
fn identity_copies_share_equal_margins_but_preserve_different_outer_pixels() {
    use krkr_protocol::graphics::Fill;
    for (work, canvas_limit) in [
        (false, None),
        (true, None),
        (
            true,
            Some(Size {
                width: 80,
                height: 64,
            }),
        ),
        (
            true,
            Some(Size {
                width: 60,
                height: 48,
            }),
        ),
    ] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: 32,
                    work_framebuffer: work,
                    canvas_limit,
                    small_canvas_edge: 0,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 80,
            height: 64,
        };
        gpu.set_canvas_size(size);
        let clip = Rect {
            left: 7,
            top: 9,
            width: 62,
            height: 44,
        };
        let fill = |rectangle, color| Fill {
            rectangle,
            color,
            face: DrawFace::Alpha,
            hold_alpha: false,
        };
        let mut source = gpu.create_image(size, 0x00223344).unwrap();
        gpu.fill(&mut source, &[fill(clip, 0x7f998877)]).unwrap();
        let original = read(&gpu, &source);
        let mut target = gpu.create_image(size, 0x00223344).unwrap();
        let old_target = target.shared();
        gpu.collect().unwrap();
        let pressure = gpu.resident.reserve(gpu.resident.available()).unwrap();
        gpu.copy_rect(
            &mut target,
            &source,
            size.rect(),
            0,
            0,
            clip,
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        assert_eq!(read(&gpu, &target), original);
        assert!(
            read(&gpu, &old_target)
                .chunks_exact(4)
                .all(|p| p == [0x22, 0x33, 0x44, 0])
        );
        drop(pressure);
        gpu.fill(
            &mut target,
            &[fill(
                Rect {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 1,
                },
                0xffabcdef,
            )],
        )
        .unwrap();
        gpu.copy_rect(
            &mut target,
            &source,
            size.rect(),
            0,
            0,
            clip,
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        assert_eq!(gpu.pixel(&target, 0, 0, false).unwrap(), 0xffabcdef);
        assert_eq!(read(&gpu, &source), original);
        // The same equality proof must not apply to masked or RGB-only writes.
        let mut masked = gpu.create_image(size, 0x80223344).unwrap();
        gpu.copy_rect(
            &mut masked,
            &source,
            size.rect(),
            0,
            0,
            clip,
            DrawFace::Opaque,
            true,
        )
        .unwrap();
        assert_eq!(gpu.pixel(&masked, 10, 10, false).unwrap(), 0x80998877);
        assert_eq!(gpu.pixel(&masked, 0, 0, false).unwrap(), 0x80223344);
    }
}

#[test]
fn equal_copies_on_solid_canvases_share_results_without_pinning_pixels() {
    use krkr_protocol::graphics::Fill;
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let source_size = Size {
        width: 12,
        height: 12,
    };
    let mut source = patterned(&gpu, source_size);
    let size = Size {
        width: 24,
        height: 20,
    };
    let blank = gpu.create_image(size, 0x12345678).unwrap();
    let mut first = blank.shared();
    let copy = |target: &mut Image, source: &Image| {
        gpu.copy_rect(
            target,
            source,
            source_size.rect(),
            4,
            3,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
    };
    copy(&mut first, &source);
    let original = read(&gpu, &first);
    gpu.collect().unwrap();
    let bytes = gpu.resident.used();
    let pressure = gpu.resident.reserve(gpu.resident.available()).unwrap();
    let mut copies = Vec::new();
    for _ in 0..12 {
        let mut image = blank.shared();
        copy(&mut image, &source);
        assert_eq!(read(&gpu, &image), original);
        copies.push(image);
    }
    drop(pressure);
    assert_eq!(gpu.resident.used(), bytes);
    drop(first);
    copies.clear();
    gpu.collect().unwrap();
    assert!(
        gpu.resident.used() < bytes,
        "weak cache must not retain copied textures"
    );
    let mut target = blank.shared();
    copy(&mut target, &source);
    let fill = |rectangle, color| Fill {
        rectangle,
        color,
        face: DrawFace::Alpha,
        hold_alpha: false,
    };
    gpu.fill(&mut source, &[fill(source_size.rect(), 0xff987654)])
        .unwrap();
    let mut changed_source = blank.shared();
    copy(&mut changed_source, &source);
    assert_eq!(gpu.pixel(&changed_source, 4, 3, false).unwrap(), 0xff987654);
    assert_eq!(read(&gpu, &target), original);
    // Mutating the sole result owner invalidates its remembered output version.
    gpu.fill(
        &mut changed_source,
        &[fill(
            Rect {
                left: 4,
                top: 3,
                width: 1,
                height: 1,
            },
            0xffabcdef,
        )],
    )
    .unwrap();
    let mut again = blank.shared();
    copy(&mut again, &source);
    assert_eq!(gpu.pixel(&again, 4, 3, false).unwrap(), 0xff987654);
}

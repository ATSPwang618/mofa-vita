#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{Blend, BlendOptions, DrawFace, Rect, Size},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render_gles2::{Config, Gpu, Image};
fn row(gpu: &Gpu, colors: &[u32]) -> Image {
    let size = Size {
        width: colors.len() as u32,
        height: 1,
    };
    let mut image = gpu.create_image(size, 0).unwrap();
    let fills: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(x, &color)| krkr_protocol::graphics::Fill {
            rectangle: Rect {
                left: x as i32,
                top: 0,
                width: 1,
                height: 1,
            },
            color,
            face: DrawFace::Alpha,
            hold_alpha: false,
        })
        .collect();
    gpu.fill(&mut image, &fills).unwrap();
    image
}
fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn stretch(width: i32, height: i32) -> Transform {
    Transform::Stretch(StretchRect {
        left: 0,
        top: 0,
        width,
        height,
    })
}
fn sampling(filter: Filter) -> Sampling {
    Sampling {
        filter,
        sharpness: -1.,
        no_clip: false,
    }
}
fn copy() -> ImageOperation {
    ImageOperation::Copy { hold_alpha: false }
}

#[test]
fn full_opacity_opaque_transform_preserves_faces_without_resolving_backdrop() {
    use krkr_protocol::graphics::Fill;
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
    let source = row(&gpu, &[0x33445566, 0x778899aa]);
    let size = Size {
        width: 64,
        height: 32,
    };
    for (face, hold_alpha) in [
        (DrawFace::Alpha, false),
        (DrawFace::AddAlpha, true),
        (DrawFace::Opaque, false),
        (DrawFace::Opaque, true),
    ] {
        let mut target = gpu.create_image(size, 0x80112233).unwrap();
        gpu.fill(
            &mut target,
            &[Fill {
                rectangle: Rect {
                    left: 32,
                    width: 32,
                    ..size.rect()
                },
                color: 0x99102030,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        gpu.flush().unwrap();
        traffic::reset();
        gpu.transform(
            &mut target,
            &source,
            source.size.rect(),
            stretch(64, 32),
            sampling(Filter::Nearest),
            ImageOperation::Blend(BlendOptions {
                mode: Blend::Opaque,
                face,
                opacity: 255,
                hold_alpha,
            }),
            size.rect(),
            None,
        )
        .unwrap();
        assert_eq!(traffic::stored_pixels(), 0, "{face:?}, hold={hold_alpha}");
        let alpha = |original, background| {
            if face != DrawFace::Opaque {
                255
            } else if hold_alpha {
                background
            } else {
                original
            }
        };
        assert_eq!(
            gpu.pixel(&target, 8, 8, false).unwrap(),
            alpha(0x33, 0x80) << 24 | 0x445566
        );
        assert_eq!(
            gpu.pixel(&target, 56, 8, false).unwrap(),
            alpha(0x77, 0x99) << 24 | 0x8899aa
        );
    }
}

#[test]
fn repeated_clipped_clear_does_not_fragment_a_rotating_canvas() {
    clipped_clear_does_not_fragment(true);
}

#[test]
fn affine_clear_alone_does_not_fragment_a_rotating_canvas() {
    clipped_clear_does_not_fragment(false);
}

fn clipped_clear_does_not_fragment(explicit_clear: bool) {
    use krkr_protocol::graphics::Fill;
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                small_canvas_edge: 0,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(Size {
        width: 1024,
        height: 576,
    });
    let size = Size {
        width: 1120,
        height: 672,
    };
    let clip = Rect {
        left: 47,
        top: 47,
        width: 1026,
        height: 578,
    };
    let source = row(&gpu, &[0x77334455, 0xa056789a, 0xff369abc]);
    let source = gpu
        .logical_image(
            source,
            Size {
                width: 459,
                height: 459,
            },
        )
        .unwrap();
    let mut image = gpu.create_image(size, 0).unwrap();
    let mut last = [[0.; 2]; 3];
    let mut max_draws = 0;
    for frame in 0..100 {
        let snapshot = image.shared();
        if explicit_clear {
            gpu.fill(
                &mut image,
                &[Fill {
                    rectangle: clip,
                    color: 0x00ffffff,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
        }
        let angle = frame as f64 * 0.053;
        let x = 150.25 + frame as f64 * 1.3;
        let y = 90.5 + frame as f64 * 0.7;
        last = [
            [x, y],
            [x + 330. * angle.cos(), y + 330. * angle.sin()],
            [x - 380. * angle.sin(), y + 380. * angle.cos()],
        ];
        gpu.resolve().unwrap();
        traffic::reset();
        gpu.transform(
            &mut image,
            &source,
            source.size.rect(),
            Transform::Affine(last),
            sampling(Filter::FastLinear),
            copy(),
            clip,
            Some(0x00ffffff),
        )
        .unwrap();
        gpu.resolve().unwrap();
        max_draws = max_draws.max(traffic::draw_calls());
        drop(snapshot);
    }
    assert!(max_draws < 40, "canvas fragments grew to {max_draws} draws");
    let mut fresh = gpu.create_image(size, 0).unwrap();
    gpu.fill(
        &mut fresh,
        &[Fill {
            rectangle: clip,
            color: 0x00ffffff,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    gpu.transform(
        &mut fresh,
        &source,
        source.size.rect(),
        Transform::Affine(last),
        sampling(Filter::FastLinear),
        copy(),
        clip,
        Some(0x00ffffff),
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &image), pixels(&gpu, &fresh));
}

#[test]
fn filtered_clear_skips_old_target_loads_and_preserves_shared_snapshots() {
    let mut reference = Vec::new();
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    tile_edge: 256,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 192,
            height: 128,
        };
        let source = row(&gpu, &[0x80305070, 0xf0123498, 0x12345678]);
        for (clip, hold_alpha) in [
            (size.rect(), false),
            (
                Rect {
                    left: 13,
                    top: 9,
                    width: 141,
                    height: 93,
                },
                false,
            ),
            (size.rect(), true),
        ] {
            let mut image = gpu.create_image(size, 0x91355779).unwrap();
            let snapshot = image.shared();
            gpu.resolve().unwrap();
            traffic::reset();
            gpu.transform(
                &mut image,
                &source,
                source.size.rect(),
                Transform::Affine([[21.25, 11.5], [164.75, -13.25], [30.5, 110.25]]),
                sampling(Filter::FastLinear),
                ImageOperation::Copy { hold_alpha },
                clip,
                Some(0x49332211),
            )
            .unwrap();
            gpu.resolve().unwrap();
            if work && !hold_alpha && clip == size.rect() {
                assert!(
                    traffic::loaded_pixels() <= 1,
                    "full affine clear loaded {} old pixels",
                    traffic::loaded_pixels()
                );
            }
            assert!(
                pixels(&gpu, &snapshot)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| *p == [0x35, 0x57, 0x79, 0x91])
            );
            let actual = pixels(&gpu, &image);
            for (i, pixel) in actual.as_chunks::<4>().0.iter().enumerate() {
                let point = Rect {
                    left: (i % size.width as usize) as i32,
                    top: (i / size.width as usize) as i32,
                    width: 1,
                    height: 1,
                };
                if point.intersection(clip).is_none() {
                    assert_eq!(*pixel, [0x35, 0x57, 0x79, 0x91]);
                } else if hold_alpha {
                    assert_eq!(pixel[3], 0x91);
                }
            }
            if work {
                assert_eq!(actual, reference.remove(0));
            } else {
                reference.push(actual);
            }
        }
    }
}

#[test]
fn tiled_affine_reuses_scratch_with_identical_filtered_pixels() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 35,
        height: 21,
    };
    let logical = Size {
        width: 1750,
        height: 1050,
    };
    let output = Size {
        width: 960,
        height: 544,
    };
    let mut raw = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in raw
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[
            (i * 31) as u8,
            (i * 73) as u8,
            (i * 19) as u8,
            (i * 11) as u8,
        ]);
    }
    let compact = gpu
        .assign_bitmap(
            None,
            &Pixels {
                size: stored,
                main: Some(raw),
                province: None,
            },
        )
        .unwrap();
    let compact = gpu.logical_image(compact, logical).unwrap();
    let expanded = gpu.readback(&compact, logical.rect(), false).unwrap();
    let tiled = gpu
        .assign_bitmap(
            None,
            &Pixels {
                size: logical,
                main: Some(expanded.data),
                province: None,
            },
        )
        .unwrap();
    for points in [
        [[170.25, 590.75], [516.25, -129.75], [602.25, 800.75]],
        [[110.5, 90.5], [870.5, 90.5], [110.5, 540.5]],
        [[870.5, 90.5], [110.5, 90.5], [870.5, 540.5]],
        [[110.5, 540.5], [870.5, 540.5], [110.5, 90.5]],
        [[870.5, 540.5], [110.5, 540.5], [870.5, 90.5]],
    ] {
        let mut actual = gpu.create_image(output, 0x79452317).unwrap();
        gpu.resolve().unwrap();
        traffic::reset();
        gpu.transform(
            &mut actual,
            &tiled,
            logical.rect(),
            Transform::Affine(points),
            sampling(Filter::FastLinear),
            copy(),
            output.rect(),
            Some(0x79452317),
        )
        .unwrap();
        gpu.resolve().unwrap();
        eprintln!(
            "rotated tiled: alloc={} clears={} draws={} stores={} stored_pixels={}",
            traffic::texture_allocations(),
            traffic::clear_calls(),
            traffic::draw_calls(),
            traffic::store_calls(),
            traffic::stored_pixels()
        );
        // Allocating every distinct seam neighborhood used ten textures for
        // the rotation. Reuse must not add clears, draws or pixel transfers.
        assert!(traffic::texture_allocations() <= 4);
        assert_eq!(traffic::clear_calls(), 0);
        assert!(traffic::draw_calls() <= 63);
        assert!(traffic::stored_pixels() <= 1_961_045);
        if points[0][1] == points[1][1] {
            // Axis-aligned flips should gather seam bands, not whole tiles.
            assert!(traffic::stored_pixels() <= 450_000);
        }
        let mut expected = gpu.create_image(output, 0x79452317).unwrap();
        gpu.transform(
            &mut expected,
            &compact,
            logical.rect(),
            Transform::Affine(points),
            sampling(Filter::FastLinear),
            copy(),
            output.rect(),
            Some(0x79452317),
        )
        .unwrap();
        assert_eq!(pixels(&gpu, &actual), pixels(&gpu, &expected));
    }
}

#[test]
fn repeated_seam_gathers_reuse_storage_and_release_it_on_collection() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    let context = support::Context::new();
    let source_size = Size {
        width: 320,
        height: 208,
    };
    let output = Size {
        width: 160,
        height: 104,
    };
    let mut reference = Vec::new();
    for work in [false, true] {
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    tile_edge: 128,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let sources: Vec<_> = [19, 53]
            .into_iter()
            .map(|seed| {
                let mut raw =
                    Bytes::zeroed(source_size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
                for (i, pixel) in raw
                    .as_mut_slice()
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .enumerate()
                {
                    pixel.copy_from_slice(&[
                        (i * seed) as u8,
                        (i * 17 + seed) as u8,
                        (i * 31) as u8,
                        (i * 7 + seed) as u8,
                    ]);
                }
                gpu.assign_bitmap(
                    None,
                    &Pixels {
                        size: source_size,
                        main: Some(raw),
                        province: None,
                    },
                )
                .unwrap()
            })
            .collect();
        let mut target = gpu.create_image(output, 0).unwrap();
        gpu.collect().unwrap();
        let scratch_before = gpu.scratch.used();
        for (frame, source) in sources.iter().cycle().take(4).enumerate() {
            traffic::reset();
            gpu.transform(
                &mut target,
                source,
                source_size.rect(),
                Transform::Affine([[0.25, 0.25], [160.25, 0.25], [0.25, 104.25]]),
                sampling(Filter::FastLinear),
                copy(),
                output.rect(),
                Some(0),
            )
            .unwrap();
            gpu.resolve().unwrap();
            if work && frame != 0 {
                assert_eq!(traffic::texture_allocations(), 0, "frame {frame}");
            }
            let actual = pixels(&gpu, &target);
            if work {
                assert_eq!(actual, reference[frame], "frame {frame}");
            } else {
                reference.push(actual);
            }
        }
        if work {
            assert!(gpu.scratch.used() > scratch_before);
        }
        gpu.collect().unwrap();
        assert_eq!(gpu.scratch.used(), scratch_before);
    }
}

#[test]
fn tiled_affine_clear_only_samples_the_sprite_rectangle() {
    let context = support::Context::new();
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
    let source_size = Size {
        width: 256,
        height: 192,
    };
    let source = gpu.create_image(source_size, 0x80406080).unwrap();
    let size = Size {
        width: 512,
        height: 384,
    };
    for hold_alpha in [false, true] {
        let mut target = gpu.create_image(size, 0x99775533).unwrap();
        gpu.resolve().unwrap();
        traffic::reset();
        gpu.transform(
            &mut target,
            &source,
            source_size.rect(),
            Transform::Affine([[219.5, 165.5], [251.5, 165.5], [219.5, 189.5]]),
            sampling(Filter::FastLinear),
            ImageOperation::Copy { hold_alpha },
            size.rect(),
            Some(0x40302010),
        )
        .unwrap();
        gpu.resolve().unwrap();
        let draws = traffic::draw_calls();
        // A large clear surrounding a tiny sprite must not dispatch filtered
        // source gathers for every background tile.
        assert!(
            draws < 130,
            "unnecessary background sampling: {draws} draws"
        );
        for (i, pixel) in pixels(&gpu, &target).as_chunks::<4>().0.iter().enumerate() {
            let x = i as u32 % size.width;
            let y = i as u32 / size.width;
            let inside = (220..252).contains(&x) && (166..190).contains(&y);
            let color = if inside {
                [0x40, 0x60, 0x80]
            } else {
                [0x30, 0x20, 0x10]
            };
            let alpha = if hold_alpha {
                0x99
            } else if inside {
                0x80
            } else {
                0x40
            };
            assert_eq!(pixel, &[color[0], color[1], color[2], alpha], "{x},{y}");
        }
    }
}

#[test]
fn tiled_affine_gathers_split_to_fit_remaining_shared_memory() {
    use krkr_protocol::{budget::Budget, graphics::Fill};
    let context = support::Context::new();
    let shared = Budget::new(8 * 1024 * 1024);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 256,
                resident: shared.child(shared.limit()),
                scratch: shared.child(shared.limit()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 384,
        height: 256,
    };
    let mut source = gpu.create_image(size, 0x80778899).unwrap();
    gpu.fill(
        &mut source,
        &[Fill {
            rectangle: Rect {
                left: 254,
                top: 1,
                width: 3,
                height: 254,
            },
            color: 0xff123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let output = Size {
        width: 160,
        height: 128,
    };
    let mut reference = None;
    for limited in [false, true] {
        let mut target = gpu.create_image(output, 0xff345678).unwrap();
        gpu.collect().unwrap();
        let pressure = limited.then(|| shared.reserve(shared.available() - 40_000).unwrap());
        gpu.transform(
            &mut target,
            &source,
            size.rect(),
            Transform::Affine([[-15.25, 9.75], [151.25, -11.75], [18.25, 122.25]]),
            sampling(Filter::FastLinear),
            copy(),
            output.rect(),
            Some(0xff987654),
        )
        .unwrap();
        drop(pressure);
        let result = pixels(&gpu, &target);
        if let Some(reference) = &reference {
            assert_eq!(&result, reference);
        } else {
            reference = Some(result);
        }
    }
}

#[test]
fn small_affine_clear_preserves_empty_margins_without_allocating_a_full_canvas() {
    use krkr_protocol::graphics::Fill;
    let context = support::Context::new();
    let size = Size {
        width: 512,
        height: 512,
    };
    let mut reference = None;
    for compact in [false, true] {
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: true,
                    canvas_limit: compact.then_some(size),
                    small_canvas_edge: 0,
                    tile_edge: 512,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(size);
        let source = row(&gpu, &[0x00ffffff, 0xff765432, 0x80887766]);
        let mut target = gpu.create_image(size, 0x00223344).unwrap();
        let snapshot = target.shared();
        gpu.collect().unwrap();
        let before = gpu.resident.used();
        let pressure = compact.then(|| {
            gpu.resident
                .reserve(gpu.resident.available() - 80_000)
                .unwrap()
        });
        gpu.transform(
            &mut target,
            &source,
            source.size.rect(),
            Transform::Affine([[201.25, 181.4], [281.5, 184.2], [198.1, 264.3]]),
            sampling(Filter::FastLinear),
            copy(),
            size.rect(),
            Some(0x00223344),
        )
        .unwrap();
        drop(pressure);
        gpu.collect().unwrap();
        if compact {
            assert!(gpu.resident.used() - before < 40_000);
            assert_eq!(pixels(&gpu, &target), *reference.as_ref().unwrap());
        } else {
            reference = Some(pixels(&gpu, &target));
        }
        assert_eq!(gpu.pixel(&snapshot, 220, 210, false).unwrap(), 0x00223344);
        // A later clear must erase the old sprite as well as drawing the next.
        // A changed outside pixel prevents incorrectly narrowing the clear.
        gpu.fill(
            &mut target,
            &[Fill {
                rectangle: Rect {
                    left: 3,
                    top: 4,
                    width: 2,
                    height: 2,
                },
                color: 0xff123456,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        gpu.transform(
            &mut target,
            &source,
            source.size.rect(),
            Transform::Affine([[301., 310.], [381., 310.], [301., 390.]]),
            sampling(Filter::FastLinear),
            copy(),
            size.rect(),
            Some(0x00223344),
        )
        .unwrap();
        assert_eq!(gpu.pixel(&target, 3, 4, false).unwrap(), 0x00223344);
        assert_eq!(gpu.pixel(&target, 220, 210, false).unwrap(), 0x00223344);
    }
}

#[test]
fn repeated_full_clear_transform_reuses_writable_canvas_storage() {
    let context = support::Context::new();
    let size = Size {
        width: 64,
        height: 64,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let source = row(&gpu, &[0xffee2211]);
    let mut target = gpu.create_image(size, 0xff000000).unwrap();
    gpu.transform(
        &mut target,
        &source,
        source.size.rect(),
        Transform::Affine([[8., 8.], [40., 8.], [8., 40.]]),
        sampling(Filter::Nearest),
        copy(),
        size.rect(),
        Some(0xff112233),
    )
    .unwrap();
    gpu.flush().unwrap();
    traffic::reset();
    gpu.transform(
        &mut target,
        &source,
        source.size.rect(),
        Transform::Affine([[16., 16.], [48., 16.], [16., 48.]]),
        sampling(Filter::Nearest),
        copy(),
        size.rect(),
        Some(0xff112233),
    )
    .unwrap();
    assert_eq!(traffic::texture_allocations(), 0);
    assert_eq!(gpu.pixel(&target, 20, 20, false).unwrap(), 0xffee2211);
    assert_eq!(gpu.pixel(&target, 10, 10, false).unwrap(), 0xff112233);
}

#[test]
fn compact_affine_direct_sampling_matches_gathered_pixels_without_source_copies() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: 32,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 9,
            height: 7,
        };
        let logical = Size {
            width: 67,
            height: 51,
        };
        let output = Size {
            width: 29,
            height: 23,
        };
        let mut raw = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, p) in raw
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            p.copy_from_slice(&[
                (i * 31) as u8,
                (i * 73) as u8,
                (i * 19) as u8,
                (i * 11) as u8,
            ]);
        }
        let source = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size: stored,
                    main: Some(raw),
                    province: None,
                },
            )
            .unwrap();
        let source = gpu.logical_image(source, logical).unwrap();
        let expanded = gpu.readback(&source, logical.rect(), false).unwrap();
        let reference = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size: logical,
                    main: Some(expanded.data),
                    province: None,
                },
            )
            .unwrap();
        for filter in [Filter::Nearest, Filter::FastLinear] {
            for clear in [None, Some(0x79452317)] {
                for points in [
                    [[1.2, 3.1], [26.3, -2.7], [6.6, 22.2]],
                    [[12.4, 10.6], [13.6, 10.3], [12.8, 11.7]],
                ] {
                    let mut direct = gpu.create_image(output, 0x89346576).unwrap();
                    gpu.flush().unwrap();
                    traffic::reset();
                    gpu.transform(
                        &mut direct,
                        &source,
                        logical.rect(),
                        Transform::Affine(points),
                        sampling(filter),
                        copy(),
                        output.rect(),
                        clear,
                    )
                    .unwrap();
                    let draws = traffic::draw_calls();
                    let copies = traffic::store_calls();
                    assert!(traffic::loaded_pixels() <= output.rgba_bytes().unwrap() / 4);
                    assert!(draws <= 2, "direct draws={draws}");
                    assert!(copies <= 1, "direct copies={copies}");
                    // Create the comparison canvas after measuring the direct
                    // draw: blank canvases now share storage, so creating both
                    // first would include an unrelated copy-on-write here.
                    let mut gathered = gpu.create_image(output, 0x89346576).unwrap();
                    gpu.transform(
                        &mut gathered,
                        &reference,
                        logical.rect(),
                        Transform::Affine(points),
                        sampling(filter),
                        copy(),
                        output.rect(),
                        clear,
                    )
                    .unwrap();
                    assert_eq!(
                        pixels(&gpu, &direct),
                        pixels(&gpu, &gathered),
                        "{filter:?} {clear:?} {points:?}"
                    );
                }
            }
        }
    }
}
#[test]
fn compact_tiled_affine_gathers_stored_texels_with_identical_pixels() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: 16,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 29,
            height: 23,
        };
        let logical = Size {
            width: 67,
            height: 51,
        };
        let output = Size {
            width: 29,
            height: 23,
        };
        let mut raw = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, p) in raw
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            p.copy_from_slice(&[
                (i * 31) as u8,
                (i * 73) as u8,
                (i * 19) as u8,
                (i * 11) as u8,
            ]);
        }
        let source = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size: stored,
                    main: Some(raw),
                    province: None,
                },
            )
            .unwrap();
        let source = gpu.logical_image(source, logical).unwrap();
        let expanded = gpu.readback(&source, logical.rect(), false).unwrap();
        let reference = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size: logical,
                    main: Some(expanded.data),
                    province: None,
                },
            )
            .unwrap();
        for filter in [Filter::Nearest, Filter::FastLinear] {
            for clear in [None, Some(0x79452317)] {
                for points in [
                    [[1.2, 3.1], [26.3, -2.7], [6.6, 22.2]],
                    [[12.4, 10.6], [13.6, 10.3], [12.8, 11.7]],
                ] {
                    let mut direct = gpu.create_image(output, 0x89346576).unwrap();
                    gpu.flush().unwrap();
                    traffic::reset();
                    gpu.transform(
                        &mut direct,
                        &source,
                        logical.rect(),
                        Transform::Affine(points),
                        sampling(filter),
                        copy(),
                        output.rect(),
                        clear,
                    )
                    .unwrap();
                    // Expanding seam gathers to logical pixels needed up to
                    // 136 draws and 2821 copied pixels for the same transform.
                    assert!(traffic::draw_calls() <= 60);
                    assert!(traffic::stored_pixels() <= 1600);
                    // Allocate after measuring so shared blank storage does
                    // not introduce an unrelated copy-on-write.
                    let mut gathered = gpu.create_image(output, 0x89346576).unwrap();
                    gpu.transform(
                        &mut gathered,
                        &reference,
                        logical.rect(),
                        Transform::Affine(points),
                        sampling(filter),
                        copy(),
                        output.rect(),
                        clear,
                    )
                    .unwrap();
                    assert_eq!(
                        pixels(&gpu, &direct),
                        pixels(&gpu, &gathered),
                        "{filter:?} {clear:?} {points:?}"
                    );
                }
            }
        }
    }
}
#[test]
fn equal_size_stretch_clips_source_like_plain_copy() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer,
                    tile_edge: 2,
                    ..Config::default()
                },
            )
            .unwrap()
        };
        let source = row(&gpu, &[0x40702010, 0x80201070, 0xc0207030]);
        let size = Size {
            width: 7,
            height: 3,
        };
        // Legacy StretchBlt dispatches unscaled, fully destination-clipped
        // rectangles to Blt before validating affine source bounds.
        for rectangle in [
            Rect {
                left: -1,
                top: -1,
                width: 5,
                height: 3,
            },
            Rect {
                left: 0,
                top: 0,
                width: 5,
                height: 3,
            },
            Rect {
                left: 12,
                top: 0,
                width: 5,
                height: 3,
            },
        ] {
            for filter in [Filter::Nearest, Filter::FastLinear, Filter::Lanczos3] {
                for hold_alpha in [false, true] {
                    let mut target = gpu.create_image(size, 0x91708090).unwrap();
                    let mut expected = target.shared();
                    gpu.copy_rect(
                        &mut expected,
                        &source,
                        rectangle,
                        1,
                        0,
                        size.rect(),
                        if hold_alpha {
                            DrawFace::Opaque
                        } else {
                            DrawFace::Alpha
                        },
                        hold_alpha,
                    )
                    .unwrap();
                    gpu.transform(
                        &mut target,
                        &source,
                        rectangle,
                        Transform::Stretch(StretchRect {
                            left: 1,
                            top: 0,
                            width: 5,
                            height: 3,
                        }),
                        sampling(filter),
                        ImageOperation::Copy { hold_alpha },
                        size.rect(),
                        None,
                    )
                    .unwrap();
                    assert_eq!(pixels(&gpu, &target), pixels(&gpu, &expected));
                }
            }
        }
        // Actual rescaling still requires a valid source rectangle.
        let mut target = gpu.create_image(size, 0x91708090).unwrap();
        let before = pixels(&gpu, &target);
        assert!(
            gpu.transform(
                &mut target,
                &source,
                Rect {
                    width: 5,
                    height: 3,
                    ..Default::default()
                },
                stretch(4, 2),
                sampling(Filter::FastLinear),
                copy(),
                size.rect(),
                None
            )
            .is_err()
        );
        assert_eq!(pixels(&gpu, &target), before);
    }
}
#[test]
fn wide_filters_accumulate_across_tap_batches_and_texture_tiles() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 31,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let colors: Vec<u32> = (0..256)
        .map(|i| if i < 128 { 0x40202020 } else { 0xc0e0e0e0 })
        .collect();
    let source = row(&gpu, &colors);
    let size = Size {
        width: 1,
        height: 1,
    };
    let mut output = gpu.create_image(size, 0).unwrap();
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        stretch(1, 1),
        sampling(Filter::Area),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &output), [128; 4]);
    let size = Size {
        width: 2,
        height: 1,
    };
    let mut output = gpu.create_image(size, 0).unwrap();
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        Transform::Stretch(StretchRect {
            left: 2,
            top: 0,
            width: -2,
            height: 1,
        }),
        sampling(Filter::Area),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &output), [224, 224, 224, 192, 32, 32, 32, 64]);
    let source = gpu
        .create_image(
            Size {
                width: 2,
                height: 257,
            },
            0x97cb672f,
        )
        .unwrap();
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        stretch(2, 1),
        sampling(Filter::Lanczos3),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    for pixel in pixels(&gpu, &output).as_chunks::<4>().0 {
        for (actual, expected) in pixel.iter().zip([203_i32, 103, 47, 151]) {
            assert!((i32::from(*actual) - expected).abs() <= 1, "{pixel:?}");
        }
    }
}

#[test]
fn rejected_filter_budget_preserves_shared_image_contents() {
    let context = support::Context::new();
    let mut gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let mut image = row(&gpu, &[0x40202020, 0x80505050, 0xc0808080, 0xffb0b0b0]);
    let saved = image.shared();
    let before = pixels(&gpu, &image);
    gpu.collect().unwrap();
    let original = std::mem::replace(&mut gpu.scratch, krkr_protocol::budget::Budget::new(3));
    assert!(
        gpu.transform(
            &mut image,
            &saved,
            saved.size.rect(),
            stretch(2, 1),
            sampling(Filter::Area),
            copy(),
            saved.size.rect(),
            None
        )
        .is_err()
    );
    gpu.scratch = original;
    assert_eq!(pixels(&gpu, &image), before);
    assert_eq!(pixels(&gpu, &saved), before);
}
// Fixed legacy fixtures shared with the WGPU backend's transformation tests.
#[test]
fn nearest_stretch_flip_and_affine_pixel_centers() {
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
    let source = row(&gpu, &[0x40112233, 0x80556677]);
    let size = Size {
        width: 4,
        height: 2,
    };
    let mut output = gpu.create_image(size, 0x12345678).unwrap();
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        stretch(4, 2),
        sampling(Filter::Nearest),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    let forward = [
        17, 34, 51, 64, 17, 34, 51, 64, 85, 102, 119, 128, 85, 102, 119, 128,
    ]
    .repeat(2);
    assert_eq!(pixels(&gpu, &output), forward);
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        Transform::Stretch(StretchRect {
            left: 4,
            top: 2,
            width: -4,
            height: -2,
        }),
        sampling(Filter::Nearest),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &output),
        [
            85, 102, 119, 128, 85, 102, 119, 128, 17, 34, 51, 64, 17, 34, 51, 64
        ]
        .repeat(2)
    );
    // 90-degree rotation: (x,y) -> (1-y,x), source edges at half pixels.
    let points = [[1.5, -0.5], [1.5, 1.5], [0.5, -0.5]];
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        Transform::Affine(points),
        sampling(Filter::Nearest),
        copy(),
        size.rect(),
        Some(0),
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &output),
        [
            0, 0, 0, 0, 17, 34, 51, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 85, 102, 119, 128, 0,
            0, 0, 0, 0, 0, 0, 0
        ]
    );
    // Clipping a stretch must advance its source mapping, not rescale the clip.
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        stretch(4, 2),
        sampling(Filter::Nearest),
        copy(),
        Rect {
            left: 2,
            top: 0,
            width: 1,
            height: 1,
        },
        None,
    )
    .unwrap();
    assert_eq!(&pixels(&gpu, &output)[8..12], &[85, 102, 119, 128]);
}

#[test]

fn linear_borders_hold_alpha_and_affine_clear() {
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
    let source = row(&gpu, &[0xff000000, 0xffffffff, 0xff000000]);
    let rect = Rect {
        left: 1,
        top: 0,
        width: 1,
        height: 1,
    };
    let size = Size {
        width: 2,
        height: 1,
    };
    let mut output = gpu.create_image(size, 0x40000000).unwrap();
    gpu.transform(
        &mut output,
        &source,
        rect,
        stretch(2, 1),
        sampling(Filter::FastLinear),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &output), [255; 8]);
    let mut filter = sampling(Filter::FastLinear);
    filter.no_clip = true;
    gpu.transform(
        &mut output,
        &source,
        rect,
        stretch(2, 1),
        filter,
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &output),
        [192, 192, 192, 255, 191, 191, 191, 255]
    );
    // HDA copies follow the native nearest affine path, even with stCubic.
    let source = row(&gpu, &[0x64112233, 0x80445566]);
    let points = [[-0.5, -0.5], [1.5, -0.5], [-0.5, 0.5]];
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        Transform::Affine(points),
        sampling(Filter::Cubic),
        ImageOperation::Copy { hold_alpha: true },
        size.rect(),
        Some(0),
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &output), [17, 34, 51, 255, 68, 85, 102, 255]);
    let outside = [[10.0, 10.0], [11.0, 10.0], [10.0, 11.0]];
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        Transform::Affine(outside),
        filter,
        ImageOperation::Copy { hold_alpha: true },
        size.rect(),
        Some(0x12010203),
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &output), [1, 2, 3, 255, 1, 2, 3, 255]);
}

#[test]
fn every_stretch_filter_uses_its_kernel_and_then_blends() {
    stretch_filter_fixtures(false);
}
#[test]
fn work_surface_filters_use_native_rows_with_the_same_kernel_results() {
    stretch_filter_fixtures(true);
}
fn stretch_filter_fixtures(work_framebuffer: bool) {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                work_framebuffer,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let source = row(&gpu, &[0xff000000, 0xff505050, 0xffa0a0a0, 0xfff0f0f0]);
    let size = Size {
        width: 2,
        height: 1,
    };
    // Independent evaluations of krkrz WeightFunctor.h on a 4 -> 2 ramp.
    // Fast names share the generic floating kernel; SIMD rounding may differ.
    let expected = [
        [80, 240],
        [40, 200],
        [50, 190],
        [36, 203],
        [50, 190],
        [36, 203],
        [41, 198],
        [41, 198],
        [37, 202],
        [37, 202],
        [41, 198],
        [41, 198],
        [38, 201],
        [38, 201],
        [40, 200],
        [40, 200],
        [53, 186],
        [53, 186],
        [38, 201],
        [38, 201],
    ];
    for (legacy, expected) in expected.into_iter().enumerate() {
        let mut output = gpu.create_image(size, 0).unwrap();
        let filter = sampling(Filter::from_legacy(legacy as i32).unwrap());
        gpu.transform(
            &mut output,
            &source,
            source.size.rect(),
            stretch(2, 1),
            filter,
            copy(),
            size.rect(),
            None,
        )
        .unwrap();
        for (pixel, want) in pixels(&gpu, &output)
            .as_chunks::<4>()
            .0
            .iter()
            .zip(expected)
        {
            for channel in &pixel[..3] {
                assert!(
                    (i32::from(*channel) - want).abs() <= 1,
                    "filter {legacy}: {pixel:?}, expected {want}"
                );
            }
            assert!(pixel[3] >= 254, "filter {legacy} alpha");
        }
    }
    let mut output = gpu.create_image(size, 0x64646464).unwrap();
    let operation = ImageOperation::Blend(BlendOptions {
        mode: Blend::Additive,
        face: DrawFace::Opaque,
        opacity: 255,
        hold_alpha: true,
    });
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        stretch(2, 1),
        sampling(Filter::Area),
        operation,
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &output),
        [140, 140, 140, 100, 255, 255, 255, 100]
    );
    // The stock low-order affine dispatch doesn't implement Additive.
    gpu.transform(
        &mut output,
        &source,
        source.size.rect(),
        stretch(2, 1),
        sampling(Filter::Nearest),
        operation,
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &output),
        [140, 140, 140, 100, 255, 255, 255, 100]
    );
}

#[test]
fn native_filter_rows_keep_compact_source_coordinates_flips_and_clips() {
    let context = support::Context::new();
    let mut results = Vec::new();
    for work_framebuffer in [false, true] {
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer,
                    tile_edge: 4,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut source = gpu
            .create_image(
                Size {
                    width: 7,
                    height: 5,
                },
                0x40204060,
            )
            .unwrap();
        gpu.fill(
            &mut source,
            &[krkr_protocol::graphics::Fill {
                rectangle: Rect {
                    left: 3,
                    top: 1,
                    width: 3,
                    height: 3,
                },
                color: 0xc0d09050,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let source = gpu
            .logical_image(
                source,
                Size {
                    width: 14,
                    height: 10,
                },
            )
            .unwrap();
        let size = Size {
            width: 9,
            height: 7,
        };
        let mut output = gpu.create_image(size, 0x90554433).unwrap();
        gpu.transform(
            &mut output,
            &source,
            Rect {
                left: 2,
                top: 2,
                width: 10,
                height: 7,
            },
            Transform::Stretch(StretchRect {
                left: 8,
                top: 6,
                width: -7,
                height: -5,
            }),
            sampling(Filter::Lanczos3),
            copy(),
            Rect {
                left: 2,
                top: 1,
                width: 5,
                height: 4,
            },
            None,
        )
        .unwrap();
        results.push(pixels(&gpu, &output));
    }
    for (a, b) in results[0].iter().zip(&results[1]) {
        assert!((i16::from(*a) - i16::from(*b)).abs() <= 1);
    }
}

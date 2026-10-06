#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{Adjustment, DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};

fn upload(gpu: &Gpu, size: Size, raw: &[u8]) -> Image {
    let mut main = Bytes::zeroed(raw.len(), &gpu.staging).unwrap();
    main.as_mut_slice().copy_from_slice(raw);
    gpu.assign_bitmap(
        None,
        &Pixels {
            size,
            main: Some(main),
            province: None,
        },
    )
    .unwrap()
}
fn data(size: Size) -> Vec<u8> {
    (0..size.width * size.height)
        .flat_map(|i| {
            [
                (i * 31 + 73) as u8,
                (i * 43 + 151) as u8,
                (i * 79 + 251) as u8,
                [0, 1, 63, 127, 128, 254, 255][i as usize % 7],
            ]
        })
        .collect()
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn small_blur_uses_one_draw_when_the_source_and_output_fit_one_texture() {
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
    let size = Size {
        width: 768,
        height: 512,
    };
    let pixels = data(size);
    for alpha in [false, true] {
        let mut image = upload(&gpu, size, &pixels);
        traffic::reset();
        gpu.adjust(
            &mut image,
            size.rect(),
            &Adjustment::BoxBlur {
                radius: [1, 1],
                alpha,
            },
        )
        .unwrap();
        let draws = traffic::draw_calls() - traffic::load_calls();
        println!("small blur kernel draws={draws} alpha={alpha}");
        assert_eq!(
            draws, 1,
            "one source texture needs no 256-pixel subdivisions"
        );
        compare(&gpu, &image, &pixels, size.rect(), [1, 1], alpha);
    }
}
fn average(raw: &[u8], size: Size, x: u32, y: u32, radius: [u32; 2], alpha: bool) -> [u8; 4] {
    let reciprocal = (u64::from(radius[0]) * 2 + 1) * (u64::from(radius[1]) * 2 + 1) < 256;
    let radius = [
        radius[0].min(size.width - 1),
        radius[1].min(size.height - 1),
    ];
    let left = x.saturating_sub(radius[0]);
    let right = x
        .saturating_add(radius[0])
        .saturating_add(1)
        .min(size.width);
    let top = y.saturating_sub(radius[1]);
    let bottom = y
        .saturating_add(radius[1])
        .saturating_add(1)
        .min(size.height);
    let count = (right - left) * (bottom - top);
    let mut sum = [0u32; 4];
    for sy in top..bottom {
        for sx in left..right {
            let i = ((sy * size.width + sx) * 4) as usize;
            let mut p: [u32; 4] = std::array::from_fn(|c| u32::from(raw[i + c]));
            if alpha {
                let a = p[3] + (p[3] >> 7);
                for c in &mut p[..3] {
                    *c = (*c * a) >> 8;
                }
            }
            for c in 0..4 {
                sum[c] += p[c];
            }
        }
    }
    let mut value = sum.map(|s| {
        if reciprocal {
            ((s + count / 2) * (65536 / count)) >> 16
        } else {
            (s + count / 2) / count
        }
    });
    if alpha {
        let a = value[3];
        for c in &mut value[..3] {
            *c = (*c * 255).checked_div(a).unwrap_or(0).min(255);
        }
    }
    value.map(|c| c as u8)
}

#[test]
fn tall_compact_canvas_streams_across_band_and_texture_edges() {
    let context = support::Context::new();
    let logical = Size {
        width: 385,
        height: 801,
    };
    let stored = Size {
        width: 361,
        height: 750,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                canvas_limit: Some(logical),
                small_canvas_edge: 0,
                tile_edge: 512,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(logical);
    for spare in [300_000, 1_048_576] {
        for radius in [[1, 1], [10, 4]] {
            let physical = upload(&gpu, stored, &data(stored));
            let mut target = gpu.logical_image(physical, logical).unwrap();
            let before = read(&gpu, &target);
            gpu.collect().unwrap();
            let pressure = gpu
                .resident
                .reserve(gpu.resident.available() - spare)
                .unwrap();
            traffic::reset();
            gpu.adjust(
                &mut target,
                logical.rect(),
                &Adjustment::BoxBlur {
                    radius,
                    alpha: true,
                },
            )
            .unwrap();
            gpu.resolve().unwrap();
            eprintln!(
                "tall blur spare={spare} radius={radius:?}: draws={} stores={}",
                traffic::draw_calls(),
                traffic::store_calls()
            );
            if spare == 1_048_576 {
                assert!(traffic::draw_calls() < 100, "excessive strip preparation");
                assert!(traffic::store_calls() <= 30, "excessive strip transfers");
            }
            let got = read(&gpu, &target);
            let nearest = |x: u32, from: u32, to: u32| ((2 * x + 1) * to) / (2 * from);
            for y in 0..logical.height {
                for x in 0..logical.width {
                    let sx = nearest(
                        nearest(x, logical.width, stored.width),
                        stored.width,
                        logical.width,
                    );
                    let sy = nearest(
                        nearest(y, logical.height, stored.height),
                        stored.height,
                        logical.height,
                    );
                    let at = ((y * logical.width + x) * 4) as usize;
                    assert_eq!(
                        &got[at..at + 4],
                        &average(&before, logical, sx, sy, radius, true),
                        "spare={spare}, radius={radius:?}, at={x},{y}"
                    );
                }
            }
            drop((pressure, target));
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn unique_canvas_blur_streams_under_pressure_without_corrupting_later_halos() {
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    for stored in [
        size,
        Size {
            width: 320,
            height: 213,
        },
    ] {
        for alpha in [false, true] {
            let physical = upload(&gpu, stored, &data(stored));
            let mut image = gpu.logical_image(physical, size).unwrap();
            gpu.collect().unwrap();
            let pressure = gpu
                .resident
                .reserve(gpu.resident.available() - 200_000)
                .unwrap();
            for radius in [[2, 2], [1, 1], [3, 0], [5, 5], [9, 9]] {
                let before = read(&gpu, &image);
                gpu.adjust(
                    &mut image,
                    size.rect(),
                    &Adjustment::BoxBlur { radius, alpha },
                )
                .unwrap();
                assert_eq!(image.stored_size(), Some(stored));
                let got = read(&gpu, &image);
                let nearest = |x: u32, from: u32, to: u32| ((2 * x + 1) * to) / (2 * from);
                for y in 0..size.height {
                    for x in 0..size.width {
                        let sx = nearest(
                            nearest(x, size.width, stored.width),
                            stored.width,
                            size.width,
                        );
                        let sy = nearest(
                            nearest(y, size.height, stored.height),
                            stored.height,
                            size.height,
                        );
                        let at = ((y * size.width + x) * 4) as usize;
                        assert_eq!(
                            &got[at..at + 4],
                            &average(&before, size, sx, sy, radius, alpha),
                            "stored={stored:?}, radius={radius:?}, alpha={alpha}, at={x},{y}"
                        );
                    }
                }
                gpu.collect().unwrap();
            }
            drop((pressure, image));
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn partly_shared_canvas_streams_blur_without_mutating_snapshot() {
    let context = support::Context::new();
    let size = Size {
        width: 768,
        height: 256,
    };
    let shared = Budget::new(8 * 1024 * 1024);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                work_framebuffer: true,
                resident: shared.child(shared.limit()),
                scratch: shared.child(shared.limit()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut image = gpu
        .logical_image(upload(&gpu, size, &data(size)), size)
        .unwrap();
    let snapshot = image.shared();
    let original = read(&gpu, &snapshot);
    let patch_size = Size {
        width: 256,
        height: 256,
    };
    let patch = upload(&gpu, patch_size, &data(patch_size));
    gpu.copy_rect(
        &mut image,
        &patch,
        patch_size.rect(),
        0,
        0,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    drop(patch);
    let before = read(&gpu, &image);
    gpu.collect().unwrap();
    let _pressure = shared.reserve(shared.available() - 700_000).unwrap();
    assert!(
        gpu.adjust_write_bytes(
            &image,
            size.rect(),
            &Adjustment::BoxBlur {
                radius: [2, 2],
                alpha: true
            }
        ) < size.rgba_bytes().unwrap()
    );
    gpu.adjust(
        &mut image,
        size.rect(),
        &Adjustment::BoxBlur {
            radius: [2, 2],
            alpha: true,
        },
    )
    .unwrap();
    compare(&gpu, &image, &before, size.rect(), [2, 2], true);
    assert_eq!(read(&gpu, &snapshot), original);
}

#[test]
fn streamed_blur_keeps_zero_margins_outside_the_kernel_virtual() {
    use krkr_protocol::graphics::{DrawFace, Fill};
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let shared = Budget::new(8 * 1024 * 1024);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                resident: shared.child(shared.limit()),
                scratch: shared.child(shared.limit()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut image = gpu.create_image(size, 0).unwrap();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 32,
                top: 80,
                width: 320,
                height: 96,
            },
            color: 0x80654321,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let old = read(&gpu, &image);
    gpu.collect().unwrap();
    let baseline = gpu.resident.used();
    let pressure = shared.reserve(shared.available() - 110_000).unwrap();
    gpu.adjust(
        &mut image,
        size.rect(),
        &Adjustment::BoxBlur {
            radius: [2, 2],
            alpha: true,
        },
    )
    .unwrap();
    drop(pressure);
    gpu.collect().unwrap();
    assert!(
        gpu.resident.used() < baseline + 30_000,
        "far zero margins must stay virtual"
    );
    compare(&gpu, &image, &old, size.rect(), [2, 2], true);
}

#[test]
fn shared_canvas_blur_allocates_only_the_nonzero_footprint() {
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let shared = Budget::new(8 * 1024 * 1024);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                resident: shared.child(shared.limit()),
                scratch: shared.child(shared.limit()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let patch_size = Size {
        width: 96,
        height: 40,
    };
    let patch = upload(&gpu, patch_size, &data(patch_size));
    let mut original = gpu.create_image(size, 0).unwrap();
    gpu.copy_rect(
        &mut original,
        &patch,
        patch_size.rect(),
        127,
        97,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    let before = read(&gpu, &original);
    for radius in [[2, 1], [5, 5], [65, 0]] {
        for alpha in [false, true] {
            let mut image = original.shared();
            gpu.collect().unwrap();
            let operation = Adjustment::BoxBlur { radius, alpha };
            assert!(gpu.adjust_write_bytes(&image, size.rect(), &operation) < 90_000);
            let pressure = shared.reserve(shared.available() - 190_000).unwrap();
            gpu.adjust(&mut image, size.rect(), &operation).unwrap();
            drop(pressure);
            compare(&gpu, &image, &before, size.rect(), radius, alpha);
            assert_eq!(
                read(&gpu, &original),
                before,
                "blur changed a live source alias"
            );
            assert!(
                image.resident_bytes() < 90_000,
                "transparent margins became dense RGBA"
            );
        }
    }
}
fn compare(gpu: &Gpu, target: &Image, old: &[u8], area: Rect, radius: [u32; 2], alpha: bool) {
    let size = target.size;
    let got = read(gpu, target);
    for y in 0..size.height {
        for x in 0..size.width {
            let i = ((y * size.width + x) * 4) as usize;
            let expected = if radius != [0, 0]
                && (x as i32) >= area.left
                && (y as i32) >= area.top
                && (x as i32) < area.left + area.width as i32
                && (y as i32) < area.top + area.height as i32
            {
                average(old, size, x, y, radius, alpha)
            } else {
                old[i..i + 4].try_into().unwrap()
            };
            assert_eq!(
                &got[i..i + 4],
                &expected,
                "at ({x},{y}), radius {radius:?}, alpha {alpha}"
            );
        }
    }
}

#[test]
fn canvas_blur_keeps_display_density_but_averages_the_original_logical_kernel() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(Size {
                    width: 30,
                    height: 25,
                }),
                small_canvas_edge: 0,
                tile_edge: 11,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    for size in [
        Size {
            width: 37,
            height: 31,
        },
        Size {
            width: 73,
            height: 5,
        },
    ] {
        gpu.set_canvas_size(size);
        let stored = Size {
            width: 30,
            height: (f64::from(size.height) * 30.0 / f64::from(size.width)).round() as u32,
        };
        let physical = upload(&gpu, stored, &data(stored));
        let source = gpu.logical_image(physical, size).unwrap();
        let old = read(&gpu, &source);
        for radius in [[1, 2], [5, 7], [10, 8], [65, 0]] {
            for alpha in [false, true] {
                let mut target = source.shared();
                gpu.adjust(
                    &mut target,
                    size.rect(),
                    &Adjustment::BoxBlur { radius, alpha },
                )
                .unwrap();
                let actual = target.stored_size().unwrap();
                assert!(actual.width < size.width && actual.height < size.height);
                assert_eq!(target.resident_bytes(), actual.rgba_bytes().unwrap());
                let got = read(&gpu, &target);
                let nearest = |x: u32, from: u32, to: u32| ((2 * x + 1) * to) / (2 * from);
                for y in 0..size.height {
                    for x in 0..size.width {
                        let sx = nearest(
                            nearest(x, size.width, actual.width),
                            actual.width,
                            size.width,
                        );
                        let sy = nearest(
                            nearest(y, size.height, actual.height),
                            actual.height,
                            size.height,
                        );
                        let expected = average(&old, size, sx, sy, radius, alpha);
                        let at = ((y * size.width + x) * 4) as usize;
                        assert_eq!(
                            &got[at..at + 4],
                            &expected,
                            "{radius:?} alpha={alpha} at {x},{y}"
                        );
                    }
                }
                assert_eq!(read(&gpu, &source), old);
            }
        }
    }
}

#[test]
fn box_blur_averages_once_and_preserves_clips_compact_sources_provinces_and_aliases() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 5,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 17,
        height: 13,
    };
    let clip = Rect {
        left: 2,
        top: 1,
        width: 13,
        height: 10,
    };
    for stored in [
        size,
        Size {
            width: 9,
            height: 7,
        },
    ] {
        let raw = data(stored);
        let physical = upload(&gpu, stored, &raw);
        let mut source = gpu.logical_image(physical, size).unwrap();
        gpu.fill(
            &mut source,
            &[Fill {
                rectangle: size.rect(),
                color: 53,
                face: DrawFace::Province,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let old = read(&gpu, &source);
        for radius in [
            [0, 0],
            [1, 1],
            [0, 3],
            [4, 0],
            [3, 2],
            [4, 4],
            [40, 0],
            [0, 40],
            [8, 7],
            [8, 8],
            [32, 1],
            [1, 32],
            [31, 31],
        ] {
            for alpha in [false, true] {
                let mut target = source.shared();
                gpu.adjust(&mut target, clip, &Adjustment::BoxBlur { radius, alpha })
                    .unwrap();
                compare(&gpu, &target, &old, clip, radius, alpha);
                assert_eq!(read(&gpu, &source), old);
                assert!(
                    gpu.readback(&target, size.rect(), true)
                        .unwrap()
                        .data
                        .as_slice()
                        .iter()
                        .all(|&p| p == 53)
                );
                drop(target);
                gpu.collect().unwrap();
            }
        }
    }
}

#[test]
fn narrow_budgets_split_blocks_and_wide_kernels_cross_tap_batches() {
    let context = support::Context::new();
    let scratch = Budget::new(3000);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                scratch: scratch.clone(),
                tile_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 73,
        height: 41,
    };
    let area = Rect {
        left: 3,
        top: 2,
        width: 67,
        height: 37,
    };
    let raw = data(size);
    for radius in [[4, 4], [36, 2], [2, 36]] {
        for alpha in [false, true] {
            let mut image = upload(&gpu, size, &raw);
            gpu.adjust(&mut image, area, &Adjustment::BoxBlur { radius, alpha })
                .unwrap();
            assert!(scratch.used() <= 3000);
            compare(&gpu, &image, &raw, area, radius, alpha);
            drop(image);
            gpu.collect().unwrap();
            assert_eq!(scratch.used(), 0);
        }
    }
}

#[test]
fn integer_sums_beyond_float_precision_keep_the_final_rounding() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 128,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 263,
        height: 263,
    };
    let raw: Vec<u8> = (0..size.width * size.height)
        .flat_map(|i| {
            [
                255 - (i % 2) as u8,
                254 + (i % 3 == 0) as u8,
                255,
                254 + (i % 5 == 0) as u8,
            ]
        })
        .collect();
    let area = Rect {
        left: 131,
        top: 131,
        width: 1,
        height: 1,
    };
    for radius in [[64, 64], [65, 64], [131, 131]] {
        for alpha in [false, true] {
            let mut image = upload(&gpu, size, &raw);
            gpu.adjust(&mut image, area, &Adjustment::BoxBlur { radius, alpha })
                .unwrap();
            let expected = average(&raw, size, 131, 131, radius, alpha);
            let actual = gpu.pixel(&image, 131, 131, false).unwrap();
            assert_eq!(
                actual,
                u32::from_be_bytes([expected[3], expected[0], expected[1], expected[2]])
            );
            assert_eq!(
                gpu.pixel(&image, 130, 131, false).unwrap(),
                u32::from_be_bytes({
                    let i = ((131 * size.width + 130) * 4) as usize;
                    [raw[i + 3], raw[i], raw[i + 1], raw[i + 2]]
                })
            );
            drop(image);
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn invalid_kernels_and_failed_budget_leave_both_planes_unchanged() {
    let context = support::Context::new();
    let mut gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 13,
        height: 9,
    };
    let mut image = upload(&gpu, size, &data(size));
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: size.rect(),
            color: 77,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let old = read(&gpu, &image);
    assert!(
        gpu.adjust(
            &mut image,
            size.rect(),
            &Adjustment::BoxBlur {
                radius: [u32::MAX, 1],
                alpha: true
            }
        )
        .is_err()
    );
    gpu.scratch = Budget::new(0);
    // Packed sums still require scratch even when the source can be sampled directly.
    assert!(
        gpu.adjust(
            &mut image,
            size.rect(),
            &Adjustment::BoxBlur {
                radius: [5, 4],
                alpha: true
            }
        )
        .is_err()
    );
    assert_eq!(read(&gpu, &image), old);
    assert!(
        gpu.readback(&image, size.rect(), true)
            .unwrap()
            .data
            .as_slice()
            .iter()
            .all(|&p| p == 77)
    );
}

#[test]
fn work_surface_blur_limits_transfers_and_preserves_compact_sources() {
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
    let size = Size {
        width: 320,
        height: 180,
    };
    let stored = Size {
        width: 160,
        height: 90,
    };
    let area = Rect {
        left: 13,
        top: 11,
        width: 289,
        height: 153,
    };
    let source = gpu
        .logical_image(upload(&gpu, stored, &data(stored)), size)
        .unwrap();
    let old = read(&gpu, &source);
    for radius in [[5, 3], [9, 9]] {
        for alpha in [false, true] {
            let mut image = source.shared();
            gpu.resolve().unwrap();
            traffic::reset();
            gpu.adjust(&mut image, area, &Adjustment::BoxBlur { radius, alpha })
                .unwrap();
            gpu.resolve().unwrap();
            let loaded = traffic::loaded_pixels();
            let stored = traffic::stored_pixels();
            assert!(
                stored * 4 < 700_000,
                "separable blur should transfer only gathered halos and output"
            );
            assert!(
                traffic::store_calls() < 20,
                "separable blur must not accumulate per channel"
            );
            eprintln!(
                "work blur {radius:?} alpha={alpha}: load={} bytes store={} bytes",
                loaded * 4,
                stored * 4
            );
            compare(&gpu, &image, &old, area, radius, alpha);
            assert_eq!(read(&gpu, &source), old);
            drop(image);
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn compact_blur_samples_logical_neighbors_without_expanding_a_gather() {
    for radius in [[1, 1], [5, 4]] {
        let context = support::Context::new();
        let mut gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let logical = Size {
            width: 67,
            height: 51,
        };
        let stored = Size {
            width: 29,
            height: 23,
        };
        let source = gpu
            .logical_image(upload(&gpu, stored, &data(stored)), logical)
            .unwrap();
        let original = read(&gpu, &source);
        // Small kernels need no scratch; packed kernels need only their two
        // horizontal sum surfaces. No room remains for an expanded input.
        let read_budget = gpu.scratch.clone();
        let draw_budget = Budget::new(if radius == [1, 1] {
            0
        } else {
            logical.rgba_bytes().unwrap() * 2
        });
        for alpha in [false, true] {
            let mut target = source.shared();
            gpu.resolve().unwrap();
            gpu.scratch = draw_budget.clone();
            traffic::reset();
            gpu.adjust(
                &mut target,
                logical.rect(),
                &Adjustment::BoxBlur { radius, alpha },
            )
            .unwrap();
            gpu.resolve().unwrap();
            assert_eq!(traffic::read_calls(), 0);
            assert_eq!(
                traffic::stored_pixels(),
                (logical.width * logical.height) as usize
            );
            gpu.scratch = read_budget.clone();
            compare(&gpu, &target, &original, logical.rect(), radius, alpha);
            assert_eq!(read(&gpu, &source), original);
        }
    }
}

#[test]
fn whole_small_blur_never_loads_discarded_output_pixels() {
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 64,
        height: 32,
    };
    let raw = data(size);
    let source = upload(&gpu, size, &raw);
    let mut image = source.shared();
    gpu.resolve().unwrap();
    // A direct small kernel no longer needs a gather or a smaller block.
    gpu.scratch = Budget::new(0);
    traffic::reset();
    gpu.adjust(
        &mut image,
        size.rect(),
        &Adjustment::BoxBlur {
            radius: [1, 1],
            alpha: true,
        },
    )
    .unwrap();
    gpu.resolve().unwrap();
    assert!(
        traffic::loaded_pixels() <= 2,
        "only allocation warm-up texels, never the discarded output image"
    );
    assert_eq!(traffic::read_calls(), 0);
    assert_eq!(
        traffic::texture_allocations(),
        1,
        "a blur inside one source tile needs only its output texture"
    );
    compare(&gpu, &image, &raw, size.rect(), [1, 1], true);
    assert_eq!(read(&gpu, &source), raw);
}

#[test]
fn shorter_packed_blur_reuses_sum_attachments_without_a_gather() {
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
    let size = Size {
        width: 320,
        height: 180,
    };
    let raw = data(size);
    let source = upload(&gpu, size, &raw);
    for (height, radius) in [(153, [9, 9]), (127, [5, 4])] {
        let area = Rect {
            left: 13,
            top: 11,
            width: 289,
            height,
        };
        let mut image = source.shared();
        gpu.resolve().unwrap();
        traffic::reset();
        gpu.adjust(
            &mut image,
            area,
            &Adjustment::BoxBlur {
                radius,
                alpha: true,
            },
        )
        .unwrap();
        gpu.resolve().unwrap();
        eprintln!(
            "packed blur height={height}: allocations={} stores={} bytes",
            traffic::texture_allocations(),
            traffic::stored_pixels() * 4
        );
        if height == 127 {
            assert!(
                traffic::texture_allocations() <= 1,
                "a smaller blur recreated sum attachments or a redundant gather"
            );
        }
        compare(&gpu, &image, &raw, area, radius, true);
        assert_eq!(read(&gpu, &source), raw);
    }
}

#[test]
fn scratch_block_planning_leaves_room_for_output_in_a_shared_graphics_budget() {
    for radius in [[1, 1], [9, 9]] {
        let context = support::Context::new();
        let shared = Budget::new(1024 * 1024);
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    resident: shared.child(1024 * 1024),
                    scratch: shared.child(1024 * 1024),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 64,
            height: 64,
        };
        let raw = data(size);
        let source = upload(&gpu, size, &raw);
        let mut image = source.shared();
        gpu.collect().unwrap();
        let _pressure = shared
            .reserve(shared.available() - size.rgba_bytes().unwrap() - 5000)
            .unwrap();
        gpu.adjust(
            &mut image,
            size.rect(),
            &Adjustment::BoxBlur {
                radius,
                alpha: true,
            },
        )
        .unwrap();
        assert!(gpu.scratch.used() <= 5000);
        compare(&gpu, &image, &raw, size.rect(), radius, true);
        assert_eq!(read(&gpu, &source), raw);
    }
}

#[test]
fn wide_compact_stream_keeps_its_seam_gather_between_strips() {
    let context = support::Context::new();
    let logical = Size {
        width: 1216,
        height: 1088,
    };
    let stored = Size {
        width: 1140,
        height: 1020,
    };
    let shared = Budget::new(16 * 1024 * 1024);
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(logical),
                small_canvas_edge: 0,
                tile_edge: 1024,
                resident: shared.child(shared.limit()),
                scratch: shared.child(shared.limit()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(logical);
    let mut image = gpu
        .logical_image(upload(&gpu, stored, &data(stored)), logical)
        .unwrap();
    let before = read(&gpu, &image);
    gpu.collect().unwrap();
    let pressure = shared.reserve(shared.available() - 300_000).unwrap();
    traffic::reset();
    gpu.adjust(
        &mut image,
        logical.rect(),
        &Adjustment::BoxBlur {
            radius: [1, 1],
            alpha: true,
        },
    )
    .unwrap();
    gpu.resolve().unwrap();
    eprintln!(
        "wide stream: finishes={} allocations={} stores={}",
        traffic::finish_calls(),
        traffic::texture_allocations(),
        traffic::store_calls()
    );
    assert!(
        traffic::finish_calls() <= 1,
        "a strip must not retire its seam gather"
    );
    assert!(
        traffic::texture_allocations() <= 5,
        "two window tiles, two history tiles and one seam gather suffice"
    );
    drop(pressure);
    let got = read(&gpu, &image);
    let nearest = |x: u32, from: u32, to: u32| ((2 * x + 1) * to) / (2 * from);
    for y in 0..logical.height {
        for x in 0..logical.width {
            let sx = nearest(
                nearest(x, logical.width, stored.width),
                stored.width,
                logical.width,
            );
            let sy = nearest(
                nearest(y, logical.height, stored.height),
                stored.height,
                logical.height,
            );
            let at = ((y * logical.width + x) * 4) as usize;
            assert_eq!(
                &got[at..at + 4],
                &average(&before, logical, sx, sy, [1, 1], true),
                "at={x},{y}"
            );
        }
    }
}

#[test]
fn sparse_stream_copies_only_active_columns_and_their_halo() {
    let context = support::Context::new();
    let size = Size {
        width: 1536,
        height: 384,
    };
    let shared = Budget::new(16 * 1024 * 1024);
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                resident: shared.child(shared.limit()),
                scratch: shared.child(shared.limit()),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut image = gpu.create_image(size, 0).unwrap();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 640,
                top: 48,
                width: 128,
                height: 256,
            },
            color: 0x80654321,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let before = read(&gpu, &image);
    gpu.collect().unwrap();
    let pressure = shared.reserve(shared.available() - 300_000).unwrap();
    traffic::reset();
    gpu.adjust(
        &mut image,
        size.rect(),
        &Adjustment::BoxBlur {
            radius: [2, 2],
            alpha: true,
        },
    )
    .unwrap();
    gpu.resolve().unwrap();
    eprintln!("sparse stream: stored pixels={}", traffic::stored_pixels());
    assert!(
        traffic::stored_pixels() < (size.width * size.height) as usize,
        "transparent side margins must not be copied for every strip"
    );
    drop(pressure);
    compare(&gpu, &image, &before, size.rect(), [2, 2], true);
}

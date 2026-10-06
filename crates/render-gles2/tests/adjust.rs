#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{Adjustment, Blend, BlendOptions, DrawFace, Rect, Size},
    pixels::{Bytes, Pixels},
    transform::{Filter, ImageOperation, Sampling, Transform},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::sync::Arc;

fn pixels(gpu: &Gpu, size: Size, data: &[u8], province: bool) -> Image {
    let mut main = Bytes::zeroed(data.len(), &gpu.staging).unwrap();
    main.as_mut_slice().copy_from_slice(data);
    let province = province.then(|| {
        let mut p = Bytes::zeroed((size.width * size.height) as usize, &gpu.staging).unwrap();
        for (i, b) in p.as_mut_slice().iter_mut().enumerate() {
            *b = (i * 31 + 9) as u8;
        }
        p
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
fn read(gpu: &Gpu, image: &Image, province: bool) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), province)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn data(size: Size) -> Vec<u8> {
    (0..size.width * size.height)
        .flat_map(|i| {
            [
                (i * 31 + 13) as u8,
                (i * 43 + 25) as u8,
                (i * 71 + 47) as u8,
                [0, 1, 2, 63, 127, 128, 254, 255][i as usize % 8],
            ]
        })
        .collect()
}

#[test]
fn tall_gamma_preserves_continuous_non_solid_pixels() {
    let context = support::Context::new();
    for size in [
        Size {
            width: 1009,
            height: 3033,
        },
        Size {
            width: 1750,
            height: 1050,
        },
    ] {
        let raw: Vec<u8> = (0..size.width * size.height)
            .flat_map(|i| {
                let x = i % size.width;
                let y = i / size.width;
                [
                    (x * 255 / size.width) as u8,
                    (y * 255 / size.height) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]
            })
            .collect();
        let table = Arc::new(std::array::from_fn(|i| {
            [i as u32 / 2 + 15, 255 - i as u32, i as u32 * 3 / 4, 0]
        }));
        let mut expected = raw.clone();
        for p in expected.as_chunks_mut::<4>().0 {
            for c in 0..3 {
                p[c] = table[p[c] as usize][c] as u8;
            }
        }
        for work_framebuffer in [false, true] {
            for tile_edge in [512, 1024, 2048, 4096] {
                let gpu = unsafe {
                    Gpu::new(
                        context.gl(),
                        Config {
                            work_framebuffer,
                            render_target_cache_entries: 8,
                            render_target_cache_bytes: 8 * 1024 * 1024,
                            tile_edge,
                            scratch: Budget::new(128 * 1024 * 1024),
                            ..Default::default()
                        },
                    )
                    .unwrap()
                };
                let source = pixels(&gpu, size, &raw, false);
                let mut image = gpu.create_image(size, 0).unwrap();
                gpu.copy_rect(
                    &mut image,
                    &source,
                    size.rect(),
                    0,
                    0,
                    size.rect(),
                    DrawFace::Alpha,
                    false,
                )
                .unwrap();
                assert_eq!(
                    read(&gpu, &image, false),
                    raw,
                    "initial canvas copy work={work_framebuffer} tile={tile_edge}"
                );
                gpu.adjust(
                    &mut image,
                    size.rect(),
                    &Adjustment::Gamma {
                        table: table.clone(),
                        additive: false,
                    },
                )
                .unwrap();
                let actual = read(&gpu, &image, false);
                let differences: Vec<_> = actual
                    .iter()
                    .zip(&expected)
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .take(20)
                    .map(|(i, (a, b))| {
                        (
                            i / 4 % size.width as usize,
                            i / 4 / size.width as usize,
                            i % 4,
                            *a,
                            *b,
                        )
                    })
                    .collect();
                assert!(
                    differences.is_empty(),
                    "work={work_framebuffer} tile={tile_edge} mismatched (x,y,channel,actual,expected): {differences:?}"
                );
            }
        }
    }
}

#[test]
fn shared_point_edits_write_only_their_result_and_leave_the_source_unchanged() {
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
        width: 67,
        height: 37,
    };
    let raw = data(size);
    let original = pixels(&gpu, size, &raw, false);
    let table = Arc::new(std::array::from_fn(|i| {
        [255 - i as u32, (i * i / 255) as u32, (i / 2 + 27) as u32, 0]
    }));
    for operation in [
        Adjustment::GrayScale,
        Adjustment::Gamma {
            table: table.clone(),
            additive: false,
        },
        Adjustment::Gamma {
            table: table.clone(),
            additive: true,
        },
    ] {
        let mut target = original.shared();
        gpu.resolve().unwrap();
        traffic::reset();
        gpu.adjust(&mut target, size.rect(), &operation).unwrap();
        gpu.resolve().unwrap();
        assert_eq!(traffic::read_calls(), 0);
        // PVR needs one residency sample for each output tile before the
        // framebuffer-to-texture transfer. No image-sized loads are needed.
        assert_eq!(
            traffic::loaded_pixels(),
            (size.width.div_ceil(32) * size.height.div_ceil(32)) as usize,
            "{operation:?}"
        );
        assert_eq!(
            traffic::stored_pixels(),
            (size.width * size.height) as usize,
            "only the resulting pixels need storing: {operation:?}"
        );
        let mut expected = raw.clone();
        for pixel in expected.as_chunks_mut::<4>().0 {
            match &operation {
                Adjustment::GrayScale => {
                    let gray = (u32::from(pixel[0]) * 54
                        + u32::from(pixel[1]) * 183
                        + u32::from(pixel[2]) * 19)
                        >> 8;
                    pixel[..3].fill(gray as u8);
                }
                Adjustment::Gamma { table, additive } => {
                    let adjusted = expected_gamma(pixel, table, *additive);
                    pixel.copy_from_slice(&adjusted);
                }
                _ => unreachable!(),
            }
        }
        assert_eq!(read(&gpu, &target, false), expected, "{operation:?}");
        assert_eq!(read(&gpu, &original, false), raw);
        drop(target);
        gpu.collect().unwrap();
    }
}
fn compact(gpu: &Gpu, size: Size, stored: Size, bytes: &[u8]) -> Image {
    let physical = pixels(gpu, stored, bytes, false);
    gpu.logical_image(physical, size).unwrap()
}

#[test]
fn constant_point_margins_stay_sparse_and_match_dense_gpu_pixels_under_pressure() {
    use krkr_protocol::graphics::{DrawFace, Fill};
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let dense = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let sparse = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 512,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    sparse.set_canvas_size(size);
    let mut table = [[0; 4]; 256];
    for (i, row) in table.iter_mut().enumerate() {
        *row = [(255 - i) as u32, (i * 3) as u32, (i * 7 / 9) as u32, 0];
    }
    // Exercise the shader's 32-bit word product and output saturation, too.
    table[255] = [u32::MAX, 0x0100_0000, 0x1020_3040, 0];
    let table = Arc::new(table);
    for alpha in [0, 1, 63, 128, 254, 255] {
        let color = alpha << 24 | 0x0096a1b2;
        for operation in [
            Adjustment::GrayScale,
            Adjustment::Gamma {
                table: table.clone(),
                additive: false,
            },
            Adjustment::Gamma {
                table: table.clone(),
                additive: true,
            },
        ] {
            let mut actual = sparse.create_image(size, color).unwrap();
            let mut expected = dense.create_image(size, color).unwrap();
            let patch_size = Size {
                width: 96,
                height: 64,
            };
            let raw = data(patch_size);
            for (gpu, image) in [(&sparse, &mut actual), (&dense, &mut expected)] {
                let patch = pixels(gpu, patch_size, &raw, false);
                gpu.copy_rect(
                    image,
                    &patch,
                    patch_size.rect(),
                    141,
                    87,
                    size.rect(),
                    DrawFace::Alpha,
                    false,
                )
                .unwrap();
            }
            let original = read(&sparse, &actual, false);
            let snapshot = actual.shared();
            sparse.collect().unwrap();
            assert!(sparse.adjust_write_bytes(&actual, size.rect(), &operation) < 32 * 1024);
            let pressure = sparse
                .resident
                .reserve(sparse.resident.available() - 64 * 1024)
                .unwrap();
            sparse.adjust(&mut actual, size.rect(), &operation).unwrap();
            drop(pressure);
            dense
                .adjust(&mut expected, size.rect(), &operation)
                .unwrap();
            assert_eq!(
                read(&sparse, &actual, false),
                read(&dense, &expected, false),
                "alpha={alpha}, operation={operation:?}"
            );
            assert_eq!(read(&sparse, &snapshot, false), original);
            assert!(actual.resident_bytes() < 32 * 1024);
            // A later write into a virtual margin must detach and expand just
            // that region, including transparent RGB and the original alias.
            sparse
                .fill(
                    &mut actual,
                    &[Fill {
                        rectangle: Rect {
                            left: 8,
                            top: 6,
                            width: 3,
                            height: 5,
                        },
                        color: 0x815522aa,
                        face: DrawFace::Alpha,
                        hold_alpha: false,
                    }],
                )
                .unwrap();
            assert_eq!(sparse.pixel(&actual, 9, 7, false).unwrap(), 0x815522aa);
            assert_eq!(sparse.pixel(&snapshot, 9, 7, false).unwrap(), color);
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn full_canvas_flips_and_point_functions_keep_one_display_density() {
    let context = support::Context::new();
    let logical = Size {
        width: 39,
        height: 31,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(Size {
                    width: 30,
                    height: 25,
                }),
                small_canvas_edge: 0,
                tile_edge: 16,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(logical);
    for stored in [
        logical,
        Size {
            width: 64,
            height: 16,
        },
        Size {
            width: 13,
            height: 11,
        },
    ] {
        let raw = data(stored);
        let source = compact(&gpu, logical, stored, &raw);
        let expected_size = Size {
            width: (39.0 * (f64::from(stored.width) / 39.).min(30. / 39.)).round() as u32,
            height: (31.0 * (f64::from(stored.height) / 31.).min(30. / 39.)).round() as u32,
        };
        for operation in [
            Adjustment::Flip { horizontal: true },
            Adjustment::Flip { horizontal: false },
            Adjustment::GrayScale,
        ] {
            let mut target = source.shared();
            gpu.adjust(&mut target, logical.rect(), &operation).unwrap();
            assert_eq!(target.size, logical);
            assert_eq!(target.stored_size(), Some(expected_size));
            let mut physical = target.shared();
            physical.size = expected_size;
            let got = read(&gpu, &physical, false);
            let mut expected = Vec::new();
            for y in 0..expected_size.height {
                for x in 0..expected_size.width {
                    let mut sx = (f64::from(x) + 0.5) * f64::from(stored.width)
                        / f64::from(expected_size.width);
                    let mut sy = (f64::from(y) + 0.5) * f64::from(stored.height)
                        / f64::from(expected_size.height);
                    if let Adjustment::Flip { horizontal } = operation {
                        if horizontal {
                            sx = f64::from(stored.width) - sx;
                        } else {
                            sy = f64::from(stored.height) - sy;
                        }
                    }
                    let i = ((sy.floor() as u32 * stored.width + sx.floor() as u32) * 4) as usize;
                    let mut pixel: [u8; 4] = raw[i..i + 4].try_into().unwrap();
                    if matches!(operation, Adjustment::GrayScale) {
                        let gray = (u32::from(pixel[0]) * 54
                            + u32::from(pixel[1]) * 183
                            + u32::from(pixel[2]) * 19)
                            >> 8;
                        pixel[..3].fill(gray as u8);
                    }
                    expected.extend(pixel);
                }
            }
            for (i, (got, expected)) in got
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.as_chunks::<4>().0.iter())
                .enumerate()
            {
                assert_eq!(
                    got, expected,
                    "stored={stored:?} op={operation:?} pixel={i}"
                );
            }
            assert_eq!(
                read(&gpu, &source, false),
                read(&gpu, &compact(&gpu, logical, stored, &raw), false)
            );
            if matches!(operation, Adjustment::GrayScale) {
                gpu.adjust(&mut target, logical.rect(), &operation).unwrap();
                assert_eq!(target.stored_size(), Some(expected_size));
            }
        }
    }
}

#[test]
fn work_point_kernels_need_no_tile_snapshot_and_preserve_pending_writes() {
    use krkr_protocol::filter::{Filter, Kind};
    let context = support::Context::new();
    let size = Size {
        width: 67,
        height: 37,
    };
    let mut reference = None;
    for work in [false, true] {
        let scratch = Budget::new(if work {
            32 * 32 * 4 + 32 * 4 * 4
        } else {
            65536
        });
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 32,
                    scratch: scratch.clone(),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let original = pixels(&gpu, size, &data(size), false);
        let mut target = original.shared();
        let make_filter = |kind, words: &[u32]| {
            let mut bytes = Bytes::zeroed(words.len() * 4, &gpu.staging).unwrap();
            for (out, word) in bytes
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(words)
            {
                out.copy_from_slice(&word.to_le_bytes());
            }
            Adjustment::Filter(Filter {
                kind,
                table: Arc::new(bytes),
            })
        };
        let operations = [
            Adjustment::GrayScale,
            Adjustment::Gamma {
                table: Arc::new(std::array::from_fn(|i| {
                    [255 - i as u32, i as u32 / 2, i as u32, 0]
                })),
                additive: false,
            },
            make_filter(
                Kind::Lookup,
                &(0..256)
                    .map(|i| (255 - i) | i << 8 | (i / 2) << 16 | 255 << 24)
                    .collect::<Vec<_>>(),
            ),
            make_filter(
                Kind::Modulate {
                    hue: 0.2,
                    saturation: 0.6,
                    luminance: 0.3,
                },
                &[],
            ),
            make_filter(Kind::Xor { color: 0x729384 }, &[]),
            make_filter(
                Kind::Noise {
                    seed: 98,
                    level: Some(23),
                },
                &[],
            ),
            Adjustment::Gradient {
                bounds: size.rect(),
                from: 0x8298aa28,
                to: 0x49bbbb38,
                vertical: true,
                blend: true,
            },
        ];
        // Overlap earlier pending writes, straddle tile edges, and preserve
        // unmodified pixels outside a clipped operation and a live alias.
        for (i, operation) in operations.iter().enumerate() {
            let area = if i % 2 == 0 {
                size.rect()
            } else {
                Rect {
                    left: 5,
                    top: 3,
                    width: 54,
                    height: 29,
                }
            };
            gpu.adjust(&mut target, area, operation).unwrap();
        }
        assert_eq!(read(&gpu, &original, false), data(size));
        let result = read(&gpu, &target, false);
        if let Some(reference) = &reference {
            let mismatch = result.iter().zip(reference).position(|(a, b)| a != b);
            assert_eq!(mismatch, None, "first differing channel");
        } else {
            reference = Some(result);
        }
        assert!(scratch.used() <= scratch.limit());
    }
}

#[test]
fn full_point_adjustments_keep_compact_storage_and_snapshot_pixels() {
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
        width: 39,
        height: 31,
    };
    for stored in [
        Size {
            width: 13,
            height: 11,
        },
        Size {
            width: 64,
            height: 32,
        },
        Size {
            width: 64,
            height: 16,
        },
    ] {
        let expected_storage = if stored.width <= size.width && stored.height <= size.height {
            stored
        } else {
            size
        };
        let original = compact(&gpu, size, stored, &data(stored));
        let before = read(&gpu, &original, false);
        let gamma = Arc::new(std::array::from_fn(|i| {
            [255 - i as u32, i as u32 / 2, (i * i / 255) as u32, 0]
        }));
        for operation in [
            Adjustment::GrayScale,
            Adjustment::Gamma {
                table: gamma.clone(),
                additive: false,
            },
            Adjustment::Gamma {
                table: gamma.clone(),
                additive: true,
            },
        ] {
            let mut target = original.shared();
            assert_eq!(
                gpu.adjust_write_bytes(&target, size.rect(), &operation),
                expected_storage.rgba_bytes().unwrap()
            );
            gpu.adjust(&mut target, size.rect(), &operation).unwrap();
            assert_eq!(target.size, size);
            assert_eq!(target.stored_size(), Some(expected_storage));
            let mut reference = pixels(&gpu, size, &before, false);
            gpu.adjust(&mut reference, size.rect(), &operation).unwrap();
            assert_eq!(read(&gpu, &target, false), read(&gpu, &reference, false));
            assert_eq!(read(&gpu, &original, false), before);
            drop((target, reference));
            gpu.collect().unwrap();
        }
    }
}
fn inside(rect: Rect, x: u32, y: u32) -> bool {
    i64::from(x) >= i64::from(rect.left)
        && i64::from(y) >= i64::from(rect.top)
        && i64::from(x) < i64::from(rect.left) + i64::from(rect.width)
        && i64::from(y) < i64::from(rect.top) + i64::from(rect.height)
}
fn expected_gamma(p: &[u8], table: &[[u32; 4]; 256], additive: bool) -> [u8; 4] {
    let mut out: [u8; 4] = p.try_into().unwrap();
    let alpha = u32::from(p[3]);
    if !additive && alpha == 0 {
        return out;
    }
    for c in 0..3 {
        let color = u32::from(p[c]);
        let result = if !additive || alpha == 255 {
            table[color as usize][c]
        } else {
            let adjusted = alpha + (alpha >> 7);
            if color > alpha {
                (table[255][c].wrapping_mul(adjusted) >> 8) + color - alpha
            } else {
                let reciprocal = (65536 / alpha.max(1)).min(65535);
                let index = ((reciprocal * color) >> 8).min(255) as usize;
                table[index][c].wrapping_mul(adjusted) >> 8
            }
        };
        out[c] = result.min(255) as u8;
    }
    out
}

#[test]
fn grayscale_and_both_gamma_modes_preserve_alpha_clips_compact_sources_and_aliases() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 4,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 13,
        height: 11,
    };
    let area = Rect {
        left: 2,
        top: 1,
        width: 9,
        height: 7,
    };
    let normal: Arc<[[u32; 4]; 256]> = Arc::new(std::array::from_fn(|i| {
        [255 - i as u32, (i * i / 255) as u32, (i / 2 + 27) as u32, 0]
    }));
    let wrapped = Arc::new(std::array::from_fn(|i| {
        [
            0x8000_0001u32.wrapping_add(i as u32),
            0xffff_ffffu32.wrapping_sub(i as u32),
            0x0100_0101u32.wrapping_mul(i as u32),
            0,
        ]
    }));
    for stored in [
        size,
        Size {
            width: 7,
            height: 5,
        },
    ] {
        let raw = data(stored);
        let original = compact(&gpu, size, stored, &raw);
        let before = read(&gpu, &original, false);
        for operation in [
            Adjustment::GrayScale,
            Adjustment::Gamma {
                table: normal.clone(),
                additive: false,
            },
            Adjustment::Gamma {
                table: normal.clone(),
                additive: true,
            },
            Adjustment::Gamma {
                table: wrapped.clone(),
                additive: false,
            },
            Adjustment::Gamma {
                table: wrapped.clone(),
                additive: true,
            },
        ] {
            let mut target = original.shared();
            gpu.adjust(&mut target, area, &operation).unwrap();
            let mut expected = before.clone();
            for y in 0..size.height {
                for x in 0..size.width {
                    if !inside(area, x, y) {
                        continue;
                    }
                    let i = ((y * size.width + x) * 4) as usize;
                    match &operation {
                        Adjustment::GrayScale => {
                            let p = &before[i..i + 4];
                            let gray = (u32::from(p[0]) * 54
                                + u32::from(p[1]) * 183
                                + u32::from(p[2]) * 19)
                                >> 8;
                            expected[i..i + 3].fill(gray as u8);
                        }
                        Adjustment::Gamma { table, additive } => expected[i..i + 4]
                            .copy_from_slice(&expected_gamma(&before[i..i + 4], table, *additive)),
                        _ => unreachable!(),
                    }
                }
            }
            assert_eq!(
                read(&gpu, &target, false),
                expected,
                "{operation:?}, stored {stored:?}"
            );
            assert_eq!(read(&gpu, &original, false), before);
            drop(target);
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn flips_use_full_image_coordinates_for_clipped_main_and_province_without_scratch() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 4,
                scratch: Budget::new(0),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 9,
        height: 7,
    };
    let area = Rect {
        left: 1,
        top: 2,
        width: 7,
        height: 4,
    };
    for province in [false, true] {
        let original = pixels(&gpu, size, &data(size), province);
        let before = read(&gpu, &original, false);
        let regions = province.then(|| read(&gpu, &original, true));
        for horizontal in [true, false] {
            let mut target = original.shared();
            gpu.adjust(&mut target, area, &Adjustment::Flip { horizontal })
                .unwrap();
            for (is_province, source, stride) in
                [(false, Some(&before), 4), (true, regions.as_ref(), 1)]
            {
                let Some(source) = source else { continue };
                let mut expected = source.clone();
                for y in 0..size.height {
                    for x in 0..size.width {
                        if !inside(area, x, y) {
                            continue;
                        }
                        let (sx, sy) = if horizontal {
                            (size.width - 1 - x, y)
                        } else {
                            (x, size.height - 1 - y)
                        };
                        let from = (sy * size.width + sx) as usize * stride;
                        let to = (y * size.width + x) as usize * stride;
                        expected[to..to + stride].copy_from_slice(&source[from..from + stride]);
                    }
                }
                assert_eq!(read(&gpu, &target, is_province), expected);
                assert_eq!(read(&gpu, &original, is_province), *source);
            }
            assert_eq!(gpu.scratch.used(), 0);
        }
    }
}

#[cfg(target_os = "linux")]
fn rgba(c: u32) -> [u32; 4] {
    [c >> 16 & 255, c >> 8 & 255, c & 255, c >> 24]
}
#[cfg(target_os = "linux")]
fn gradient(a: u32, b: u32, length: u32, index: u32) -> [u32; 4] {
    let a = rgba(a);
    let b = rgba(b);
    let denominator = length.wrapping_sub(1).max(1);
    let x = index.min(denominator);
    std::array::from_fn(|c| {
        a[c].wrapping_mul(denominator - x)
            .wrapping_add(b[c].wrapping_mul(x))
            / denominator
    })
}

#[cfg(target_os = "linux")]
#[test]
fn gradients_keep_integer_interpolation_sse_alpha_groups_and_large_bounds() {
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
        width: 19,
        height: 13,
    };
    let area = Rect {
        left: 1,
        top: 1,
        width: 16,
        height: 11,
    };
    let bytes = data(size);
    for bounds in [
        size.rect(),
        Rect {
            left: -3,
            top: -2,
            width: 19,
            height: 15,
        },
        Rect {
            left: i32::MIN,
            top: i32::MIN,
            width: u32::MAX,
            height: 0x8000_0001,
        },
        Rect {
            left: 3,
            top: 3,
            width: 0,
            height: 1,
        },
    ] {
        for (from, to) in [
            (0x10e9b517, 0xff1f5783),
            (0xfee8a71d, 0xff26498a),
            (0xffd0c070, 0xff152d43),
        ] {
            for vertical in [false, true] {
                for blend in [false, true] {
                    let mut target = pixels(&gpu, size, &bytes, false);
                    let operation = Adjustment::Gradient {
                        bounds,
                        from,
                        to,
                        vertical,
                        blend,
                    };
                    gpu.adjust(&mut target, area, &operation).unwrap();
                    let mut expected = bytes.clone();
                    for y in 0..size.height {
                        for x in 0..size.width {
                            if !inside(area, x, y) {
                                continue;
                            }
                            let index = if vertical {
                                (y as i32).wrapping_sub(bounds.top)
                            } else {
                                (x as i32).wrapping_sub(bounds.left)
                            } as u32;
                            let length = if vertical {
                                bounds.height
                            } else {
                                bounds.width
                            };
                            let mut color = gradient(from, to, length, index);
                            let at = ((y * size.width + x) * 4) as usize;
                            let old = &bytes[at..at + 4];
                            if blend {
                                if vertical {
                                    for c in 0..3 {
                                        color[c] = (u32::from(old[c]) * (255 - color[3])
                                            + color[c] * color[3])
                                            >> 8;
                                    }
                                    color[3] = u32::from(old[3]);
                                } else {
                                    let mut opaque = color[3] == 255;
                                    let first = (area.left as u32 + 3) & !3;
                                    let end = (area.left as u32 + area.width) & !3;
                                    if opaque && x >= first && x < end {
                                        let group = (x & !3).wrapping_sub(bounds.left as u32);
                                        opaque = gradient(from, to, length, group)[3] == 255
                                            && gradient(from, to, length, group + 3)[3] == 255;
                                    }
                                    if !opaque {
                                        let a = color[3] as i32;
                                        for c in 0..4 {
                                            color[c] = (i32::from(old[c])
                                                + (((color[c] as i32 - i32::from(old[c])) * a)
                                                    >> 8))
                                                as u32;
                                        }
                                    }
                                }
                            }
                            expected[at..at + 4].copy_from_slice(&color.map(|v| v as u8));
                        }
                    }
                    assert_eq!(read(&gpu, &target, false), expected, "{operation:?}");
                    drop(target);
                    gpu.collect().unwrap();
                }
            }
        }
    }
}

#[test]
fn point_adjustment_reuses_a_single_pixel_snapshot_and_releases_gamma_tables() {
    let context = support::Context::new();
    let scratch = Budget::new(4);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                scratch: scratch.clone(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 16,
        height: 16,
    };
    let mut target = gpu.create_image(size, 0x80804020).unwrap();
    let before = gpu.resident.used();
    let operation = Adjustment::Gamma {
        table: Arc::new(std::array::from_fn(|i| [255 - i as u32; 4])),
        additive: false,
    };
    let area = Rect {
        left: 5,
        top: 6,
        width: 1,
        height: 1,
    };
    assert_eq!(gpu.adjust_upload_bytes(&operation), 3072);
    gpu.adjust(&mut target, area, &operation).unwrap();
    assert_eq!(scratch.used(), 4);
    assert_eq!(gpu.adjust_upload_bytes(&operation), 0);
    assert_eq!(gpu.pixel(&target, 5, 6, false).unwrap(), 0x807fbfdf);
    assert_eq!(gpu.pixel(&target, 4, 6, false).unwrap(), 0x80804020);
    let same = Adjustment::Gamma {
        table: Arc::new(std::array::from_fn(|i| [255 - i as u32; 4])),
        additive: false,
    };
    assert_eq!(gpu.adjust_upload_bytes(&same), 0);
    gpu.adjust(&mut target, area, &same).unwrap();
    assert_eq!(gpu.pixel(&target, 5, 6, false).unwrap(), 0x80804020);
    drop(operation);
    assert_eq!(gpu.adjust_upload_bytes(&same), 0);
    drop(same);
    gpu.collect_adjustment_tables();
    gpu.collect().unwrap();
    assert_eq!(scratch.used(), 0);
    assert_eq!(gpu.resident.used(), before);
    let failed = Gpu::adjust(&gpu, &mut target, size.rect(), &Adjustment::GrayScale);
    assert!(failed.is_err());
    assert_eq!(gpu.pixel(&target, 4, 6, false).unwrap(), 0x80804020);
}

#[test]
fn clipped_point_passes_reuse_one_pixel_across_four_tiles() {
    use krkr_protocol::filter::{Filter, Kind};
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                tile_edge: 32,
                scratch: Budget::new(4),
                work_framebuffer: false,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 67,
        height: 37,
    };
    let area = Rect {
        left: 31,
        top: 31,
        width: 2,
        height: 2,
    };
    let raw = data(size);
    let mut target = pixels(&gpu, size, &raw, false);
    traffic::reset();
    gpu.adjust(&mut target, area, &Adjustment::GrayScale)
        .unwrap();
    assert_eq!(traffic::clear_calls(), 0);
    assert_eq!(gpu.scratch.used(), 4);
    let mut expected = raw;
    for y in 31..33 {
        for x in 31..33 {
            let p = &mut expected[((y * size.width + x) * 4) as usize..][..4];
            let gray = (u32::from(p[0]) * 54 + u32::from(p[1]) * 183 + u32::from(p[2]) * 19) >> 8;
            p[..3].fill(gray as u8);
        }
    }
    assert_eq!(read(&gpu, &target, false), expected);
    gpu.collect().unwrap();
    let make_table = || {
        let mut bytes = Bytes::zeroed(1024, &gpu.staging).unwrap();
        for (i, p) in bytes
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            p.copy_from_slice(&(255 - i as u32).to_le_bytes());
        }
        Adjustment::Filter(Filter {
            kind: Kind::Lookup,
            table: Arc::new(bytes),
        })
    };
    let operation = make_table();
    traffic::reset();
    gpu.adjust(&mut target, area, &operation).unwrap();
    assert_eq!(traffic::clear_calls(), 0);
    for y in 31..33 {
        for x in 31..33 {
            let p = &mut expected[((y * size.width + x) * 4) as usize..][..4];
            for c in &mut p[..3] {
                *c = 255 - *c;
            }
        }
    }
    assert_eq!(read(&gpu, &target, false), expected);
    let same = make_table();
    assert_eq!(gpu.adjust_upload_bytes(&same), 0);
    traffic::reset();
    gpu.adjust(&mut target, area, &same).unwrap();
    // Only the one-pixel snapshot is allocated; the lookup table is reused.
    assert_eq!(traffic::texture_allocations(), 1);
    drop(operation);
    assert_eq!(gpu.adjust_upload_bytes(&same), 0);
}

#[test]
fn rgb_and_hsv_fields_keep_global_axes_constants_and_opaque_output() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 5,
                scratch: Budget::new(0),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 19,
        height: 13,
    };
    let area = Rect {
        left: 2,
        top: 1,
        width: 17,
        height: 12,
    };
    for hsv in [false, true] {
        for (axes, values) in [
            ([1, 2, 0], [0., 0., 67.]),
            ([1, 0, 2], [0., 100., 0.]),
            ([0, 2, 1], [240., 0., 0.]),
            ([0, 0, 0], [360., 100., 100.]),
            ([0, 0, 0], [-60., 130., -27.]),
            ([0, 0, 0], [-1e18, 0., 1e18]),
        ] {
            let mut target = gpu.create_image(size, 0x127593ab).unwrap();
            let operation = Adjustment::ColorField {
                size,
                hsv,
                axes,
                values,
            };
            gpu.adjust(&mut target, area, &operation).unwrap();
            let got = read(&gpu, &target, false);
            for y in 0..size.height {
                for x in 0..size.width {
                    let index = ((y * size.width + x) * 4) as usize;
                    let expected = if !inside(area, x, y) {
                        [117, 147, 171, 18]
                    } else if !hsv {
                        let rgb: [u8; 3] = std::array::from_fn(|c| match axes[c] {
                            1 => (255 * x / (size.width - 1)) as u8,
                            2 => (255 * (size.height - 1 - y) / (size.height - 1)) as u8,
                            _ => values[c] as i32 as u8,
                        });
                        [rgb[0], rgb[1], rgb[2], 255]
                    } else {
                        let mut v: [f64; 3] = std::array::from_fn(|c| {
                            let limit = if c == 0 { 360. } else { 100. };
                            match axes[c] {
                                1 => limit * f64::from(x) / f64::from(size.width - 1),
                                2 => {
                                    limit * f64::from(size.height - 1 - y)
                                        / f64::from(size.height - 1)
                                }
                                _ => f64::from(values[c] as f32),
                            }
                        });
                        if v[0] == 360. {
                            v[0] = 0.;
                        }
                        let (s, b) = (v[1] / 100., v[2] / 100.);
                        let rgb = if s == 0. {
                            [b; 3]
                        } else {
                            let sector = (v[0] / 60.).floor();
                            let f = v[0] / 60. - sector;
                            let p = b * (1. - s);
                            let q = b * (1. - f * s);
                            let t = b * (1. - (1. - f) * s);
                            match (sector as i32) % 6 {
                                0 => [b, t, p],
                                1 => [q, b, p],
                                2 => [p, b, t],
                                3 => [p, q, b],
                                4 => [t, p, b],
                                _ => [b, p, q],
                            }
                        }
                        .map(|v| (v * 255.) as i32 as u8);
                        [rgb[0], rgb[1], rgb[2], 255]
                    };
                    for c in 0..4 {
                        let error = got[index + c].abs_diff(expected[c]);
                        assert!(
                            error <= u8::from(hsv && c < 3),
                            "{operation:?}, ({x},{y}), got {:?}, expected {expected:?}",
                            &got[index..index + 4]
                        );
                    }
                }
            }
            assert_eq!(gpu.scratch.used(), 0);
        }
    }
}

#[test]
fn compact_flip_and_failed_second_plane_allocation_keep_original_versions() {
    let context = support::Context::new();
    let resident = Budget::new(256 * 321 * 4 + 9 * 7 * 4 * 3);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 4,
                resident,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 9,
        height: 7,
    };
    let mut image = pixels(&gpu, size, &data(size), true);
    let before = read(&gpu, &image, false);
    let province = read(&gpu, &image, true);
    assert!(
        gpu.adjust(
            &mut image,
            size.rect(),
            &Adjustment::Flip { horizontal: true }
        )
        .is_err()
    );
    assert_eq!(read(&gpu, &image, false), before);
    assert_eq!(read(&gpu, &image, true), province);
    drop(image);
    gpu.collect().unwrap();
    let stored = Size {
        width: 5,
        height: 3,
    };
    let original = compact(&gpu, size, stored, &data(stored));
    let before = read(&gpu, &original, false);
    let mut flipped = original.shared();
    gpu.adjust(
        &mut flipped,
        size.rect(),
        &Adjustment::Flip { horizontal: true },
    )
    .unwrap();
    let expected: Vec<u8> = before
        .chunks_exact(size.width as usize * 4)
        .flat_map(|row| row.as_chunks::<4>().0.iter().rev().flatten().copied())
        .collect();
    assert_eq!(read(&gpu, &flipped, false), expected);
    assert_eq!(read(&gpu, &original, false), before);
}

#[test]
fn native_multitile_solid_blend_covers_every_canvas_pixel() {
    let context = support::Context::new();
    let size = Size {
        width: 1120,
        height: 672,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 512,
                work_framebuffer: true,
                render_target_cache_entries: 8,
                render_target_cache_bytes: 8 * 1024 * 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let white = gpu.create_image(size, 0xff446688).unwrap();
    let actual = read(&gpu, &white, false);
    assert!(
        actual
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [0x44, 0x66, 0x88, 255]),
        "solid canvas has unfilled tiles"
    );
    let raw: Vec<_> = (0..size.width * size.height)
        .flat_map(|i| {
            [
                ((i % size.width) * 255 / size.width) as u8,
                ((i / size.width) * 255 / size.height) as u8,
                71,
                255,
            ]
        })
        .collect();
    let mut target = pixels(&gpu, size, &raw, false);
    let white = gpu.create_image(size, 0xffffffff).unwrap();
    gpu.operate(
        &mut target,
        &white,
        size.rect(),
        0,
        0,
        size.rect(),
        BlendOptions {
            mode: Blend::Alpha,
            face: DrawFace::Alpha,
            opacity: 255,
            hold_alpha: false,
        },
    )
    .unwrap();
    let input = pixels(&gpu, size, &raw, false);
    let output_size = Size {
        width: 1123,
        height: 675,
    };
    let mut output = gpu.create_image(output_size, 0).unwrap();
    gpu.transform(
        &mut output,
        &input,
        size.rect(),
        Transform::Affine([[1., 1.], [1121., 1.], [1., 673.]]),
        Sampling {
            filter: Filter::Nearest,
            sharpness: 0.,
            no_clip: false,
        },
        ImageOperation::Copy { hold_alpha: false },
        output_size.rect(),
        None,
    )
    .unwrap();
    let output_bytes = read(&gpu, &output, false);
    for y in 0..output_size.height {
        for x in 0..output_size.width {
            let at = ((y * output_size.width + x) * 4) as usize;
            let expected = if x >= 1 && x <= size.width && y >= 1 && y <= size.height {
                let i = (((y - 1) * size.width + x - 1) * 4) as usize;
                &raw[i..i + 4]
            } else {
                &[0; 4]
            };
            assert_eq!(&output_bytes[at..at + 4], expected, "affine pixel {x},{y}");
        }
    }
    let actual = read(&gpu, &target, false);
    assert!(
        actual
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [254, 254, 254, 255]),
        "opaque-alpha blend must cover all source tiles with legacy integer rounding"
    );
}

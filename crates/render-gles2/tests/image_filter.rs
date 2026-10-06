#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
use krkr_protocol::{
    budget::Budget,
    filter::{Filter, Kind},
    graphics::{Adjustment, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::sync::Arc;

fn filter(gpu: &Gpu, kind: Kind, table: &[u32]) -> Adjustment {
    let mut bytes = Bytes::zeroed(table.len() * 4, &gpu.staging).unwrap();
    for (out, n) in bytes
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(table)
    {
        out.copy_from_slice(&n.to_le_bytes());
    }
    Adjustment::Filter(Filter {
        kind,
        table: Arc::new(bytes),
    })
}
fn upload(gpu: &Gpu, size: Size, raw: &[u8]) -> Image {
    let mut bytes = Bytes::zeroed(raw.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(raw);
    gpu.assign_bitmap(
        None,
        &Pixels {
            size,
            main: Some(bytes),
            province: None,
        },
    )
    .unwrap()
}
fn data(size: Size) -> Vec<u8> {
    (0..size.width * size.height)
        .flat_map(|i| {
            [
                (i * 31 + 19) as u8,
                (i * 71 + 37) as u8,
                (i * 43 + 251) as u8,
                (i * 11) as u8,
            ]
        })
        .collect()
}

#[test]
fn whole_image_color_filters_do_not_expand_converted_assets() {
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
    let stored = Size {
        width: 13,
        height: 11,
    };
    let original = gpu
        .logical_image(upload(&gpu, stored, &data(stored)), size)
        .unwrap();
    let before = read(&gpu, &original);
    let table: Vec<u32> = (0..256).map(|i| 255 - i).collect();
    for operation in [
        filter(&gpu, Kind::Lookup, &table),
        filter(
            &gpu,
            Kind::Modulate {
                hue: 0.13,
                saturation: -0.25,
                luminance: 0.1,
            },
            &[],
        ),
    ] {
        let mut target = original.shared();
        let mut reference = upload(&gpu, size, &before);
        gpu.adjust(&mut target, size.rect(), &operation).unwrap();
        gpu.adjust(&mut reference, size.rect(), &operation).unwrap();
        assert_eq!(target.stored_size(), Some(stored));
        assert_eq!(read(&gpu, &target), read(&gpu, &reference));
        assert_eq!(read(&gpu, &original), before);
    }
}

#[test]
fn repeated_filters_share_live_results_and_invalidate_changed_pixels() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 32,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 31,
        height: 29,
    };
    let original = upload(&gpu, size, &data(size));
    let before = read(&gpu, &original);
    let table: Vec<u32> = (0..256).map(|i| 255 - i).collect();
    let mut first = original.shared();
    gpu.adjust(&mut first, size.rect(), &filter(&gpu, Kind::Lookup, &table))
        .unwrap();
    gpu.collect().unwrap();
    let used = gpu.resident.used();
    let expected = read(&gpu, &first);
    let mut results = Vec::new();
    for _ in 0..8 {
        let mut next = original.shared();
        // Equal values in separately allocated tables must still share.
        assert_eq!(
            gpu.adjust_write_bytes(&next, size.rect(), &filter(&gpu, Kind::Lookup, &table)),
            0
        );
        gpu.adjust(&mut next, size.rect(), &filter(&gpu, Kind::Lookup, &table))
            .unwrap();
        results.push(next);
    }
    gpu.collect().unwrap();
    assert_eq!(
        gpu.resident.used(),
        used,
        "repeated color edits allocated more image planes"
    );
    for result in &results {
        assert_eq!(read(&gpu, result), expected);
    }
    // Editing one result must preserve the other users and the cache entry.
    gpu.adjust(
        &mut first,
        size.rect(),
        &filter(&gpu, Kind::Xor { color: 0x00ffffff }, &[]),
    )
    .unwrap();
    assert_eq!(read(&gpu, &results[0]), expected);
    assert_eq!(read(&gpu, &original), before);
    drop(results);
    // Once only one result remains, overwriting its texture must also make
    // the weak version stale, even if the storage address is reused.
    let mut result = original.shared();
    gpu.adjust(
        &mut result,
        size.rect(),
        &filter(&gpu, Kind::Lookup, &table),
    )
    .unwrap();
    let zeros = upload(&gpu, size, &vec![0; size.rgba_bytes().unwrap()]);
    gpu.copy_rect(
        &mut result,
        &zeros,
        size.rect(),
        0,
        0,
        size.rect(),
        krkr_protocol::graphics::DrawFace::Alpha,
        false,
    )
    .unwrap();
    let mut next = original.shared();
    gpu.adjust(&mut next, size.rect(), &filter(&gpu, Kind::Lookup, &table))
        .unwrap();
    assert_eq!(read(&gpu, &next), expected);

    let mut clipped = original.shared();
    let area = Rect {
        left: 3,
        top: 4,
        width: 15,
        height: 12,
    };
    gpu.adjust(&mut clipped, area, &filter(&gpu, Kind::Lookup, &table))
        .unwrap();
    let clipped_expected = change(&before, size, area, |p, _, _| {
        [255 - p[0], 255 - p[1], 255 - p[2], p[3]]
    });
    assert_eq!(read(&gpu, &clipped), clipped_expected);
    let mut second = original.shared();
    let operation = filter(&gpu, Kind::Lookup, &table);
    assert_eq!(gpu.adjust_write_bytes(&second, area, &operation), 0);
    gpu.adjust(&mut second, area, &operation).unwrap();
    assert_eq!(read(&gpu, &second), clipped_expected);

    let mut changed = original;
    gpu.upload(
        &mut changed,
        &Pixels {
            size,
            main: Some(Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap()),
            province: None,
        },
    )
    .unwrap();
    gpu.adjust(&mut changed, size.rect(), &operation).unwrap();
    assert_eq!(
        read(&gpu, &changed),
        [255, 255, 255, 0].repeat((size.width * size.height) as usize)
    );
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn change(
    raw: &[u8],
    size: Size,
    area: Rect,
    mut apply: impl FnMut([u8; 4], u32, u32) -> [u8; 4],
) -> Vec<u8> {
    let mut expected = raw.to_vec();
    for y in 0..area.height {
        for x in 0..area.width {
            let i = (((area.top as u32 + y) * size.width + area.left as u32 + x) * 4) as usize;
            expected[i..i + 4].copy_from_slice(&apply(raw[i..i + 4].try_into().unwrap(), x, y));
        }
    }
    expected
}
fn unpack(c: u32) -> [u32; 4] {
    [c >> 16 & 255, c >> 8 & 255, c & 255, c >> 24]
}
fn pack(c: [u8; 4]) -> u32 {
    u32::from_be_bytes([c[3], c[0], c[1], c[2]])
}

#[test]
fn lookup_tint_xor_and_dither_preserve_packed_integer_rules_and_clipping() {
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
        left: 3,
        top: 2,
        width: 14,
        height: 9,
    };
    let raw = data(size);
    let source = upload(&gpu, size, &raw);
    let lookup: Vec<u32> = (0..256)
        .map(|i| if i % 31 == 0 { u32::MAX } else { i ^ 85 })
        .collect();
    let tint: Vec<u32> = (0..256u32)
        .map(|i| (i.wrapping_mul(19) << 24) | ((255 - i) << 16) | (i << 8) | ((i * 71) & 255))
        .collect();
    let mut operations = vec![
        (Kind::Lookup, lookup.as_slice()),
        (Kind::Xor { color: 0xa73cd159 }, &[][..]),
    ];
    for amount in [0, 1, 128, 256, 400, 65535] {
        operations.push((Kind::Colorize { amount }, tint.as_slice()));
    }
    for width in [18, 19, u32::MAX] {
        for height in [12, 13] {
            operations.push((Kind::Dither { width, height }, &[]));
        }
    }
    for (kind, table) in operations {
        let mut target = source.shared();
        let operation = filter(&gpu, kind, table);
        gpu.adjust(&mut target, area, &operation).unwrap();
        let expected = change(&raw, size, area, |mut p, x, y| {
            match kind {
                Kind::Lookup => {
                    for c in &mut p[..3] {
                        *c = table[*c as usize].min(255) as u8;
                    }
                }
                Kind::Colorize { amount } => {
                    let light = (u32::from(*p[..3].iter().max().unwrap())
                        + u32::from(*p[..3].iter().min().unwrap()))
                    .div_ceil(2);
                    let tinted = unpack(table[light as usize]);
                    let amount = u32::from(amount);
                    for c in 0..3 {
                        p[c] = (tinted[c].wrapping_mul(amount).wrapping_add(
                            u32::from(p[c]).wrapping_mul(256u32.wrapping_sub(amount)),
                        ) >> 8)
                            .min(255) as u8;
                    }
                }
                Kind::Xor { color } => {
                    let value = unpack(pack(p) ^ color);
                    p = value.map(|c| c as u8);
                }
                Kind::Dither { width, height } => {
                    let c = pack(p);
                    let t = [0x010101u32, 0x040404, 0x030303, 0x020202][((width.wrapping_sub(x)
                        & 1)
                        | ((height.wrapping_sub(y) & 1) << 1))
                        as usize];
                    let v = (c & 0xfcfcfc)
                        .wrapping_add(((c & 0xffff0707 | 0xfffc0404).wrapping_sub(t)) & 0x040404);
                    let overflow = v & 0x01010100;
                    let rgb = unpack(overflow.wrapping_sub(overflow >> 8) | (v & 0xfcfcfc));
                    for c in 0..3 {
                        p[c] = rgb[c] as u8;
                    }
                }
                _ => unreachable!(),
            }
            p
        });
        assert_eq!(read(&gpu, &target), expected, "{kind:?}");
        assert_eq!(read(&gpu, &source), raw);
        drop(target);
        drop(operation);
        gpu.collect_adjustment_tables();
        gpu.collect().unwrap();
    }
}

#[test]
fn msvc_noise_streams_use_clipped_row_order_and_blue_first_channels() {
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
        width: 17,
        height: 13,
    };
    let area = Rect {
        left: 3,
        top: 2,
        width: 11,
        height: 8,
    };
    for stored in [
        size,
        Size {
            width: 9,
            height: 7,
        },
    ] {
        let physical = upload(&gpu, stored, &data(stored));
        let original = gpu.logical_image(physical, size).unwrap();
        let raw = read(&gpu, &original);
        for seed in [0, 0x81234567, u32::MAX] {
            for level in [None, Some(0), Some(97), Some(-61), Some(i32::MIN)] {
                let mut target = original.shared();
                gpu.adjust(
                    &mut target,
                    area,
                    &filter(&gpu, Kind::Noise { seed, level }, &[]),
                )
                .unwrap();
                let mut state = seed;
                let mut next = || {
                    state = state.wrapping_mul(214013).wrapping_add(2531011);
                    (state >> 16) & 32767
                };
                let expected = change(&raw, size, area, |mut p, _, _| {
                    if let Some(level) = level {
                        for c in [2, 1, 0] {
                            let value = (next() as f32 / 32767. - 0.5) * level as f32;
                            let offset = value as i32;
                            p[c] = (i32::from(p[c]) + offset).clamp(0, 255) as u8;
                        }
                    } else {
                        let gray = (next() / 128) as u8;
                        p[..3].fill(gray);
                    }
                    p
                });
                assert_eq!(
                    read(&gpu, &target),
                    expected,
                    "seed {seed:x}, level {level:?}, stored {stored:?}"
                );
                assert_eq!(read(&gpu, &original), raw);
                drop(target);
                gpu.collect().unwrap();
            }
        }
    }
}

fn modulate(p: [u8; 4], hue: f32, saturation: f32, luminance: f32) -> [u8; 4] {
    let rgb = [p[0], p[1], p[2]].map(|c| f32::from(c) / 255.);
    let hi = rgb.into_iter().fold(f32::NEG_INFINITY, f32::max);
    let lo = rgb.into_iter().fold(f32::INFINITY, f32::min);
    let delta = hi - lo;
    let add = hi + lo;
    let mut l = add / 2.;
    let (mut s, mut h) = (0., 0.);
    if delta != 0. {
        s = delta / if l < 0.5 { add } else { 2. - add };
        h = if rgb[0] == hi {
            (rgb[1] - rgb[2]) / delta
        } else if rgb[1] == hi {
            2. + (rgb[2] - rgb[0]) / delta
        } else {
            4. + (rgb[0] - rgb[1]) / delta
        };
        h /= 6.;
    }
    h += hue;
    if h < 0. {
        h += (-h).ceil();
    } else if h > 1. {
        h -= (h - 1.).ceil();
    }
    s += if saturation > 0. { 1. - s } else { s } * saturation;
    l += if luminance > 0. { 1. - l } else { l } * luminance;
    let rgb = if s == 0. {
        [(l * 255.) as i32 as u8; 3]
    } else {
        let m2 = if l <= 0.5 {
            l * (1. + s)
        } else {
            l + s - l * s
        };
        let m1 = 2. * l - m2;
        [h + 1. / 3., h, h - 1. / 3.].map(|mut h| {
            if h < 0. {
                h += 1.;
            } else if h > 1. {
                h -= 1.;
            }
            let c = if h < 1. / 6. {
                m1 + (m2 - m1) * h * 6.
            } else if h < 0.5 {
                m2
            } else if h < 2. / 3. {
                m1 + (m2 - m1) * (2. / 3. - h) * 6.
            } else {
                m1
            };
            (c * 255.) as i32 as u8
        })
    };
    [rgb[0], rgb[1], rgb[2], p[3]]
}
#[test]
fn hsl_modulation_wraps_hue_and_preserves_alpha() {
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
        height: 11,
    };
    let raw = data(size);
    for (hue, saturation, luminance) in [
        (0., 0., 0.),
        (1., 0., 0.),
        (-3.125, 0.7, -0.4),
        (2.75, -0.8, 0.3),
        (0., -1., 0.),
        (0., 0., 1.),
        (0.5, 1.3, -1.2),
    ] {
        let mut image = upload(&gpu, size, &raw);
        gpu.adjust(
            &mut image,
            size.rect(),
            &filter(
                &gpu,
                Kind::Modulate {
                    hue,
                    saturation,
                    luminance,
                },
                &[],
            ),
        )
        .unwrap();
        let got = read(&gpu, &image);
        for (i, p) in raw.as_chunks::<4>().0.iter().enumerate() {
            let expected = modulate(*p, hue, saturation, luminance);
            for c in 0..4 {
                let a = got[i * 4 + c];
                let b = expected[c];
                let error = a.wrapping_sub(b).min(b.wrapping_sub(a));
                assert!(
                    error <= u8::from(c < 3),
                    "{hue},{saturation},{luminance}, pixel {i} channel {c}: {a} != {b}"
                );
            }
        }
        drop(image);
        gpu.collect().unwrap();
    }
}

fn advance(seed: u32, count: u32, a: u32, c: u32) -> u32 {
    // Ordinary fixtures walk the generator directly. The wide-coordinate
    // cases use a separate homogeneous-matrix exponentiation oracle.
    if count < 65536 {
        let mut state = seed;
        for _ in 0..count {
            state = state.wrapping_mul(a).wrapping_add(c);
        }
        return state;
    }
    let mut matrix = [[u64::from(a), u64::from(c)], [0, 1]];
    let mut vector = [u64::from(seed), 1];
    let mut count = count;
    while count != 0 {
        if count & 1 != 0 {
            vector = std::array::from_fn(|r| {
                (matrix[r][0] * vector[0] + matrix[r][1] * vector[1]) & 0xffff_ffff
            });
        }
        matrix = std::array::from_fn(|r| {
            std::array::from_fn(|c| {
                (matrix[r][0] * matrix[0][c] + matrix[r][1] * matrix[1][c]) & 0xffff_ffff
            })
        });
        count >>= 1;
    }
    vector[0] as u32
}
fn random_pixel(original: [u8; 4], x: i32, y: i32, kind: Kind) -> [u8; 4] {
    let Kind::RandomFill {
        legacy,
        seed,
        under,
        range,
        monochrome: mono,
        hold_alpha: hold,
        rectangle,
    } = kind
    else {
        unreachable!()
    };
    let a = if legacy { 0x5d588b65 } else { 0x7d2b89dd };
    let x = x.wrapping_sub(rectangle.left) as u32;
    let y = y.wrapping_sub(rectangle.top) as u32;
    let width = rectangle.width;
    let full = range == 255;
    let mut steps = width;
    let mut index = x;
    if mono {
        if full {
            steps = width / 3 + 1;
            index = x / 3;
        } else {
            steps = width.wrapping_add(1) / 2;
            index = x / 2;
        }
    } else if full {
        if hold {
            steps = steps.wrapping_add(width / 4);
            index = index.wrapping_add(x / 4);
        }
        if legacy && width >= 4 {
            let tail = width % 4;
            if x >= width - tail {
                return original;
            }
            if x < tail {
                index = (width - tail).wrapping_add(x);
            }
        }
    } else {
        steps = (width / 2).wrapping_mul(3).wrapping_add((width % 2) * 2);
        index = (x / 2).wrapping_mul(3).wrapping_add(x % 2);
    }
    let count = y.wrapping_mul(steps).wrapping_add(index);
    let previous = advance(seed, count, a, 1);
    let state = previous.wrapping_mul(a).wrapping_add(1);
    let scale =
        |bits: u32| ((bits.wrapping_mul(range as u32) as i32) >> 16).wrapping_add(under) as u8;
    let mut alpha = if hold { original[3] } else { 255 };
    let rgb = if mono {
        let gray = if full {
            let mut part = x % 3;
            if legacy {
                if x >= width - width % 3 {
                    part = if x == width.wrapping_sub(1) { 2 } else { 1 };
                } else {
                    alpha = ((u64::from(previous) * u64::from(a)) >> 56) as u8;
                }
            } else if x == width.wrapping_sub(1) && width % 3 == 1 {
                part = 1;
            }
            (state >> (24 - part * 8)) as u8
        } else {
            let high = !x.is_multiple_of(2) || x == width.wrapping_sub(1);
            scale((state >> if high { 16 } else { 0 }) & 65535)
        };
        [gray; 3]
    } else if full {
        [(state >> 24) as u8, (state >> 16) as u8, (state >> 8) as u8]
    } else {
        let next = state.wrapping_mul(a).wrapping_add(1);
        if x.is_multiple_of(2) {
            [
                scale(state & 65535),
                scale(state >> 16),
                scale(next & 65535),
            ]
        } else {
            [scale(state >> 16), scale(next & 65535), scale(next >> 16)]
        }
    };
    [rgb[0], rgb[1], rgb[2], alpha]
}

#[test]
fn random_fill_keeps_both_lcgs_packed_tails_and_legacy_alpha() {
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
    for width in [1, 2, 3, 4, 5, 7, 8, 9, 17] {
        let size = Size {
            width: width + 4,
            height: 9,
        };
        let raw = data(size);
        let area = Rect {
            left: 2,
            top: 1,
            width,
            height: 7,
        };
        for legacy in [false, true] {
            for monochrome in [false, true] {
                for hold_alpha in [false, true] {
                    for (range, under) in [
                        (255, -37),
                        (137, 51),
                        (-65537, i32::MIN),
                        (i32::MIN, i32::MAX),
                    ] {
                        let kind = Kind::RandomFill {
                            legacy,
                            seed: 0xd37b91a5,
                            under,
                            range,
                            monochrome,
                            hold_alpha,
                            rectangle: area,
                        };
                        let mut image = upload(&gpu, size, &raw);
                        gpu.adjust(&mut image, area, &filter(&gpu, kind, &[]))
                            .unwrap();
                        let expected = change(&raw, size, area, |p, x, y| {
                            random_pixel(p, x as i32 + area.left, y as i32 + area.top, kind)
                        });
                        assert_eq!(read(&gpu, &image), expected, "width {width}, {kind:?}");
                        drop(image);
                        gpu.collect().unwrap();
                    }
                }
            }
        }
    }
}

#[test]
fn random_fill_preserves_wrapped_offsets_and_large_row_strides() {
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
        height: 9,
    };
    let raw = data(size);
    let area = Rect {
        left: 1,
        top: 1,
        width: 11,
        height: 7,
    };
    for rectangle in [
        Rect {
            left: -3,
            top: -7,
            width: 17,
            height: 19,
        },
        Rect {
            left: i32::MIN + 3,
            top: i32::MIN + 11,
            width: u32::MAX,
            height: u32::MAX,
        },
    ] {
        for legacy in [false, true] {
            for monochrome in [false, true] {
                for hold_alpha in [false, true] {
                    for range in [255, 137] {
                        let kind = Kind::RandomFill {
                            legacy,
                            monochrome,
                            hold_alpha,
                            range,
                            seed: u32::MAX,
                            under: -691,
                            rectangle,
                        };
                        let mut image = upload(&gpu, size, &raw);
                        let original = image.shared();
                        gpu.adjust(&mut image, area, &filter(&gpu, kind, &[]))
                            .unwrap();
                        let expected = change(&raw, size, area, |p, x, y| {
                            random_pixel(p, x as i32 + area.left, y as i32 + area.top, kind)
                        });
                        assert_eq!(read(&gpu, &image), expected, "{kind:?}");
                        assert_eq!(read(&gpu, &original), raw);
                        drop(image);
                        drop(original);
                        gpu.collect().unwrap();
                    }
                }
            }
        }
    }
}

#[test]
fn point_filter_budgets_charge_parameter_tables_and_release_dead_lookups() {
    let context = support::Context::new();
    let mut gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 16,
        height: 16,
    };
    let mut image = gpu.create_image(size, 0x7193a5c7).unwrap();
    let area = Rect {
        left: 3,
        top: 2,
        width: 1,
        height: 1,
    };
    let lookup = filter(
        &gpu,
        Kind::Lookup,
        &(0..256).map(|v| 255 - v).collect::<Vec<_>>(),
    );
    gpu.scratch = Budget::new(4);
    let baseline = gpu.resident.used();
    assert_eq!(gpu.adjust_upload_bytes(&lookup), 1024);
    gpu.adjust(&mut image, area, &lookup).unwrap();
    assert_eq!(gpu.scratch.used(), 4);
    assert_eq!(gpu.adjust_upload_bytes(&lookup), 0);
    assert_eq!(gpu.pixel(&image, 3, 2, false).unwrap(), 0x716c5a38);
    drop(lookup);
    gpu.collect_adjustment_tables();
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), baseline);
    let kind = Kind::RandomFill {
        legacy: false,
        monochrome: false,
        hold_alpha: false,
        range: 255,
        under: 0,
        seed: 917,
        rectangle: size.rect(),
    };
    let operation = filter(&gpu, kind, &[]);
    gpu.scratch = Budget::new(16);
    gpu.adjust(&mut image, area, &operation).unwrap();
    assert_eq!(gpu.scratch.used(), 16);
    let p = random_pixel([0; 4], 3, 2, kind);
    assert_eq!(gpu.pixel(&image, 3, 2, false).unwrap(), pack(p));
    gpu.collect().unwrap();
    assert_eq!(gpu.scratch.used(), 0);
    let before = gpu.pixel(&image, 3, 2, false).unwrap();
    assert!(
        gpu.adjust(&mut image, area, &filter(&gpu, Kind::Lookup, &[0]))
            .is_err()
    );
    assert_eq!(gpu.pixel(&image, 3, 2, false).unwrap(), before);
}

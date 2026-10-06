#![cfg(target_os = "linux")]
mod support;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
    warp::Warp,
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::{collections::HashMap, sync::Arc};

fn raw(gpu: &Gpu, size: Size, data: &[u8]) -> Pixels {
    let mut bytes = Bytes::zeroed(data.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(data);
    Pixels {
        size,
        main: Some(bytes),
        province: None,
    }
}
fn upload(gpu: &Gpu, size: Size, data: &[u8]) -> Image {
    gpu.assign_bitmap(None, &raw(gpu, size, data)).unwrap()
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
                (i * 43 + 17) as u8,
                (i * 71 + 23) as u8,
                (i * 19 + 5) as u8,
                (i * 11 + 91) as u8,
            ]
        })
        .collect()
}
fn lens_table(gpu: &Gpu, constant: Option<i32>) -> Arc<Bytes> {
    let mut bytes = Bytes::zeroed(8192 * 4, &gpu.staging).unwrap();
    for (i, b) in bytes
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let value =
            constant.unwrap_or_else(|| (((i as f64 / 8191.).asin() * 0.5).tan() * 65535.) as i32);
        b.copy_from_slice(&value.to_le_bytes());
    }
    Arc::new(bytes)
}
fn pixel(data: &[u8], stored: Size, logical: Size, q: [i32; 2]) -> [u32; 4] {
    let x =
        ((q[0] as u64 * 2 + 1) * u64::from(stored.width) / (2 * u64::from(logical.width))) as usize;
    let y = ((q[1] as u64 * 2 + 1) * u64::from(stored.height) / (2 * u64::from(logical.height)))
        as usize;
    let at = (y * stored.width as usize + x) * 4;
    std::array::from_fn(|i| u32::from(data[at + i]))
}
fn valid(q: [i32; 2], size: Size, next: bool) -> bool {
    q[0] >= 0
        && q[1] >= 0
        && i64::from(q[0]) + i64::from(next) < i64::from(size.width)
        && i64::from(q[1]) + i64::from(next) < i64::from(size.height)
}
fn sum(data: &[u8], stored: Size, logical: Size, q: [i32; 2], w: [u32; 4]) -> [u32; 4] {
    let taps = [
        [q[0], q[1]],
        [q[0] + 1, q[1]],
        [q[0], q[1] + 1],
        [q[0] + 1, q[1] + 1],
    ];
    let mut result = [0u32; 4];
    for (p, weight) in taps.into_iter().zip(w) {
        for (out, input) in result.iter_mut().zip(pixel(data, stored, logical, p)) {
            *out = out.wrapping_add(input.wrapping_mul(weight));
        }
    }
    result
}
// Test-only integer oracle: ordinary Rust words reproduce the original packed
// kernel independently of the shader's byte carries and four texture samplers.
fn reference(
    data: &[u8],
    stored: Size,
    logical: Size,
    size: Size,
    background: &[u8],
    effect: &Warp,
) -> Vec<u8> {
    let mut output = background.to_vec();
    for y in 0..size.height as i32 {
        for x in 0..size.width as i32 {
            let at = [x, y];
            let index = (y as usize * size.width as usize + x as usize) * 4;
            let old: [u32; 4] = std::array::from_fn(|i| u32::from(background[index + i]));
            let result = match effect {
                Warp::Stretch {
                    source,
                    destination,
                    opacity,
                } => {
                    let delta = [
                        x.wrapping_sub(destination.left),
                        y.wrapping_sub(destination.top),
                    ];
                    if delta[0] < 0
                        || delta[1] < 0
                        || delta[0] >= destination.width as i32
                        || delta[1] >= destination.height as i32
                    {
                        continue;
                    }
                    let step = [
                        (source.width as i32).wrapping_shl(8) / destination.width as i32,
                        (source.height as i32).wrapping_shl(8) / destination.height as i32,
                    ];
                    let fixed = [
                        source
                            .left
                            .wrapping_shl(8)
                            .wrapping_add(delta[0].wrapping_mul(step[0])),
                        source
                            .top
                            .wrapping_shl(8)
                            .wrapping_add(delta[1].wrapping_mul(step[1])),
                    ];
                    let q = fixed.map(|i| i >> 8);
                    let linear = step.iter().all(|&n| n <= 256);
                    if !valid(q, logical, linear) {
                        continue;
                    }
                    let mut out = if linear {
                        let f = fixed.map(|n| n as u32 & 255);
                        let a = if *opacity < 255 { *opacity as u32 } else { 256 };
                        let upper = (255 - f[1]).wrapping_mul(a);
                        let lower = f[1].wrapping_mul(a);
                        let ab = (255 - f[0]).wrapping_mul(upper);
                        let cd = (255 - f[0]).wrapping_mul(lower);
                        sum(
                            data,
                            stored,
                            logical,
                            q,
                            [
                                ab >> 16,
                                upper.wrapping_sub(ab >> 8) >> 8,
                                cd >> 16,
                                lower.wrapping_sub(cd >> 8) >> 8,
                            ],
                        )
                    } else {
                        pixel(data, stored, logical, q).map(|p| {
                            if *opacity < 255 {
                                p.wrapping_mul(*opacity as u32)
                            } else {
                                p
                            }
                        })
                    };
                    if *opacity < 255 {
                        for (out, old) in out.iter_mut().zip(old) {
                            *out = out.wrapping_add(
                                old.wrapping_mul(255i32.wrapping_sub(*opacity) as u32),
                            );
                        }
                    }
                    out.map(|n| {
                        if linear || *opacity < 255 {
                            (n >> 8) & 255
                        } else {
                            n
                        }
                    })
                }
                Warp::Lens {
                    radius,
                    power,
                    table,
                } => {
                    let center = [(size.width / 2) as i32, (size.height / 2) as i32];
                    let offset = [x - center[0], y - center[1]];
                    let distance = ((offset[0] as f32).powi(2) + (offset[1] as f32).powi(2)).sqrt();
                    if distance >= *radius {
                        [0, 0, 0, 255]
                    } else {
                        let t = ((distance / radius * 8191.) as usize).min(8191) * 4;
                        let radial =
                            (u32::from_le_bytes(table.as_slice()[t..t + 4].try_into().unwrap())
                                as f32
                                * radius) as i32;
                        if radial == 0 {
                            if !valid(at, logical, false) {
                                continue;
                            }
                            pixel(data, stored, logical, at)
                        } else {
                            let mut scale = (radial as f32 / distance) as i32;
                            let mut done = 0u32;
                            let count = power.saturating_sub(1);
                            let mut seen = HashMap::new();
                            while done < count {
                                if let Some(before) = seen.insert(scale, done) {
                                    let cycle = done - before;
                                    let skip = (count - done) / cycle * cycle;
                                    if skip != 0 {
                                        done += skip;
                                        continue;
                                    }
                                }
                                scale = (scale >> 1).wrapping_mul(scale >> 1) >> 14;
                                done += 1;
                            }
                            let fixed = offset.map(|n| n.wrapping_mul(scale));
                            let q = [(fixed[0] >> 16) + center[0], (fixed[1] >> 16) + center[1]];
                            if !valid(q, logical, true) {
                                continue;
                            }
                            let f: [u32; 2] = std::array::from_fn(|i| {
                                let n = (fixed[i] as u32 >> 8) & 255;
                                if offset[i] < 0 { 255 - n } else { n }
                            });
                            let both = (f[0] * f[1]) >> 8;
                            let onlyx = f[0] - both;
                            let onlyy = f[1] - both;
                            let mut w = [256 - both - onlyx - onlyy, onlyx, onlyy, both];
                            if offset[0] < 0 {
                                w = [w[1], w[0], w[3], w[2]];
                            }
                            if offset[1] < 0 {
                                w = [w[2], w[3], w[0], w[1]];
                            }
                            sum(data, stored, logical, q, w).map(|n| n >> 8)
                        }
                    }
                }
                Warp::Vortex { radians } => {
                    if x >= logical.width as i32 - 1 || y >= logical.height as i32 - 1 {
                        continue;
                    }
                    let width = logical.width as i32;
                    let height = logical.height as i32;
                    let center = [width / 2, height / 2];
                    let offset = [x - center[0], y - center[1]];
                    let radius = center[0].max(center[1]);
                    let radius2 = radius.wrapping_mul(radius);
                    let shorter = width.min(height);
                    let distance = offset[0].wrapping_mul(offset[0]).wrapping_add(
                        (offset[1].wrapping_mul(offset[1]) as f32
                            * width.wrapping_mul(width) as f32
                            / shorter.wrapping_mul(shorter) as f32) as i32,
                    );
                    if radius2 == 0 || distance >= radius2 {
                        continue;
                    }
                    let strength = 1. - distance as f32 / radius2 as f32;
                    let theta = strength * strength * strength * radians;
                    let (s, c) = theta.sin_cos();
                    let [dx, dy] = offset.map(|n| n as f32);
                    let fixed = [
                        ((dx * c + dy * s + center[0] as f32) * 32767.) as i32,
                        ((dy * c - dx * s + center[1] as f32) * 32767.) as i32,
                    ];
                    let q = fixed.map(|n| n >> 15);
                    if !valid(q, logical, true) {
                        continue;
                    }
                    let f = fixed.map(|n| n as u32 & 32767);
                    let inv = f.map(|n| 32767 - n);
                    sum(
                        data,
                        stored,
                        logical,
                        q,
                        [
                            (inv[0] * inv[1]) >> 22,
                            (f[0] * inv[1]) >> 22,
                            (inv[0] * f[1]) >> 22,
                            (f[0] * f[1]) >> 22,
                        ],
                    )
                    .map(|n| n >> 8)
                }
            };
            output[index..index + 4].copy_from_slice(&result.map(|n| n as u8));
        }
    }
    output
}
fn compare(actual: &[u8], expected: &[u8], tolerance: u8, label: &str) {
    assert_eq!(actual.len(), expected.len());
    for (i, (&a, &e)) in actual.iter().zip(expected).enumerate() {
        assert!(a.abs_diff(e) <= tolerance, "{label}, byte {i}: {a} != {e}");
    }
}

#[test]
fn special_stretch_keeps_wrapping_weights_offsets_and_both_sampling_modes() {
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
    for compact in [false, true] {
        let stored = Size {
            width: 5,
            height: 4,
        };
        let logical = if compact {
            Size {
                width: 10,
                height: 8,
            }
        } else {
            stored
        };
        let bytes = data(stored);
        let source = gpu
            .logical_image(upload(&gpu, stored, &bytes), logical)
            .unwrap();
        let size = Size {
            width: 11,
            height: 9,
        };
        let background = [71, 113, 157, 199].repeat(99);
        let r = |left, top, width, height| Rect {
            left,
            top,
            width,
            height,
        };
        for (source_rect, destination) in [
            (r(1, 1, 3, 2), r(1, 2, 8, 6)),
            (r(0, 0, 9, 7), r(-2, 1, 4, 3)),
            (r(i32::MIN + 1, 0, 4, 3), r(-2, -1, 11, 9)),
            (r(0, 0, 0x01000003, 3), r(0, 0, 7, 8)),
            (r(1, 1, 4, 3), r(i32::MIN + 3, 1, i32::MAX as u32, 6)),
        ] {
            for opacity in [i32::MIN, -1000000001, -65537, -1, 0, 1, 128, 254, 255, 999] {
                let effect = Warp::Stretch {
                    source: source_rect,
                    destination,
                    opacity,
                };
                let mut target = upload(&gpu, size, &background);
                gpu.warp(&mut target, &source, &effect).unwrap();
                compare(
                    &read(&gpu, &target),
                    &reference(&bytes, stored, logical, size, &background, &effect),
                    0,
                    &format!("stretch compact={compact} opacity={opacity} {destination:?}"),
                );
            }
        }
    }
}

#[test]
fn lens_samples_quadrants_and_caps_large_powers_without_losing_wrapped_cycles() {
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
    let stored = Size {
        width: 7,
        height: 5,
    };
    let logical = Size {
        width: 14,
        height: 10,
    };
    let size = Size {
        width: 13,
        height: 11,
    };
    let bytes = data(stored);
    let source = gpu
        .logical_image(upload(&gpu, stored, &bytes), logical)
        .unwrap();
    let background = [71, 113, 157, 199].repeat(143);
    for constant in [None, Some(90000)] {
        let table = lens_table(&gpu, constant);
        for radius in [-1., 0., 6.5, 14.25] {
            for power in [1, 2, 5, 45, u32::MAX] {
                let effect = Warp::Lens {
                    radius,
                    power,
                    table: table.clone(),
                };
                let mut target = upload(&gpu, size, &background);
                gpu.warp(&mut target, &source, &effect).unwrap();
                compare(
                    &read(&gpu, &target),
                    &reference(&bytes, stored, logical, size, &background, &effect),
                    0,
                    &format!("lens table={constant:?} radius={radius} power={power}"),
                );
            }
        }
    }
}

#[test]
fn vortex_preserves_legacy_edges_aspect_ratio_and_zero_angle_rounding() {
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
    for logical in [
        Size {
            width: 9,
            height: 7,
        },
        Size {
            width: 7,
            height: 11,
        },
    ] {
        let stored = Size {
            width: 5,
            height: 4,
        };
        let bytes = data(stored);
        let source = gpu
            .logical_image(upload(&gpu, stored, &bytes), logical)
            .unwrap();
        let size = Size {
            width: 13,
            height: 12,
        };
        let background = [71, 113, 157, 199].repeat(156);
        for radians in [0., 0.6, -1.8, 4.7] {
            let effect = Warp::Vortex { radians };
            let mut target = upload(&gpu, size, &background);
            gpu.warp(&mut target, &source, &effect).unwrap();
            compare(
                &read(&gpu, &target),
                &reference(&bytes, stored, logical, size, &background, &effect),
                u8::from(radians != 0.),
                &format!("vortex {logical:?} radians={radians}"),
            );
        }
    }
}

#[test]
fn warps_keep_source_snapshots_provinces_and_lens_table_lifetimes() {
    let context = support::Context::new();
    let mut gpu = unsafe {
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
        width: 9,
        height: 7,
    };
    let bytes = data(size);
    let mut target = upload(&gpu, size, &bytes);
    gpu.fill(
        &mut target,
        &[Fill {
            rectangle: size.rect(),
            color: 53,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let source = target.shared();
    let table = lens_table(&gpu, None);
    let effect = Warp::Lens {
        radius: 4.5,
        power: 2,
        table: table.clone(),
    };
    let before = gpu.resident.used();
    assert_eq!(gpu.warp_upload_bytes(&effect), 32768);
    gpu.warp(&mut target, &source, &effect).unwrap();
    assert_eq!(gpu.warp_upload_bytes(&effect), 0);
    compare(
        &read(&gpu, &target),
        &reference(&bytes, size, size, size, &bytes, &effect),
        0,
        "alias",
    );
    assert_eq!(read(&gpu, &source), bytes);
    assert_eq!(
        gpu.readback(&target, size.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        &[53; 63]
    );
    let baseline = read(&gpu, &target);
    let invalid = Warp::Lens {
        radius: 2.,
        power: 1,
        table: Arc::new(Bytes::zeroed(4, &gpu.staging).unwrap()),
    };
    assert!(gpu.warp(&mut target, &source, &invalid).is_err());
    assert!(
        gpu.warp(&mut target, &source, &Warp::Vortex { radians: f32::NAN })
            .is_err()
    );
    let resident = gpu.resident.clone();
    gpu.resident = Budget::new(0);
    let cold = Warp::Lens {
        radius: 4.,
        power: 1,
        table: lens_table(&gpu, Some(1)),
    };
    assert!(gpu.warp(&mut target, &source, &cold).is_err());
    gpu.resident = resident;
    assert_eq!(read(&gpu, &target), baseline);
    drop(effect);
    drop(table);
    gpu.collect_warp_tables();
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), before + size.rgba_bytes().unwrap());
}

#[test]
fn small_stretch_snapshots_only_the_changed_region_under_a_tight_budget() {
    let context = support::Context::new();
    let mut gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 16,
        height: 16,
    };
    let source_size = Size {
        width: 5,
        height: 4,
    };
    let source = upload(&gpu, source_size, &data(source_size));
    let mut target = gpu.create_image(size, 0xc747719d).unwrap();
    gpu.scratch = Budget::new(4);
    gpu.warp(
        &mut target,
        &source,
        &Warp::Stretch {
            source: Rect {
                left: 0,
                top: 0,
                width: 2,
                height: 2,
            },
            destination: Rect {
                left: 3,
                top: 2,
                width: 1,
                height: 1,
            },
            opacity: 128,
        },
    )
    .unwrap();
    assert_eq!(gpu.scratch.used(), 4);
    assert_eq!(gpu.pixel(&target, 3, 2, false).unwrap(), 0x902b4350);
    assert_eq!(gpu.pixel(&target, 4, 2, false).unwrap(), 0xc747719d);
    gpu.collect().unwrap();
    assert_eq!(gpu.scratch.used(), 0);
}

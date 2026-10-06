#![cfg(target_os = "linux")]
mod support;
use krkr_protocol::{
    budget::Budget,
    filter::{Filter, Kind},
    graphics::{Adjustment, DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::sync::Arc;

fn operation(gpu: &Gpu, kind: Kind, weights: &[f32]) -> Adjustment {
    let mut bytes = Bytes::zeroed(weights.len() * 4, &gpu.staging).unwrap();
    for (out, n) in bytes
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(weights)
    {
        out.copy_from_slice(&n.to_le_bytes());
    }
    Adjustment::Filter(Filter {
        kind,
        table: Arc::new(bytes),
    })
}
fn upload(gpu: &Gpu, size: Size) -> Image {
    let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in bytes
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[
            (i * 31 + 19) as u8,
            (i * 71 + 37) as u8,
            (i * 43 + 251) as u8,
            [0, 1, 127, 128, 254, 255][i % 6],
        ]);
    }
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
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

// Direct scalar oracle for the legacy filter kernels. It deliberately works
// on the whole clipped region and has no texture, tile or tap-batch concepts.
fn reference(raw: &[u8], size: Size, area: Rect, weights: Option<&[f32]>, passes: u32) -> Vec<u8> {
    let mut result = raw.to_vec();
    for pass in 0..passes {
        let source = result.clone();
        for y in 0..area.height as i32 {
            for x in 0..area.width as i32 {
                let pixel = |x: i32, y: i32| -> &[u8] {
                    let i = (((y + area.top) as u32 * size.width + (x + area.left) as u32) * 4)
                        as usize;
                    &source[i..i + 4]
                };
                let value = if let Some(kernel) = weights {
                    let length = kernel.len() as i32;
                    let middle = length / 2;
                    let vertical = pass != 0;
                    let extent = if vertical { area.height } else { area.width } as i32;
                    let row = if vertical { y } else { x };
                    let mut sum = [0f32; 4];
                    let mut scale = 0f32;
                    for j in (middle - row).max(0)..length.min(extent - row + middle) {
                        let index = row + j - middle;
                        let rgba = if vertical {
                            pixel(x, index)
                        } else {
                            pixel(index, y)
                        };
                        let weight = kernel[if length > extent { index } else { j } as usize];
                        for c in 0..4 {
                            sum[c] += f32::from(rgba[c]) * weight;
                        }
                        scale += kernel[j as usize];
                    }
                    if row < middle || row >= extent - middle || length > extent {
                        for v in &mut sum {
                            *v /= scale;
                        }
                    }
                    sum.map(|v| (v + 0.5) as u32 as u8)
                } else {
                    let mut sum = [0u32; 4];
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            if dx == 0 && dy == 0 {
                                continue;
                            }
                            let (nx, ny) = (x + dx, y + dy);
                            let rgba = if nx < 0
                                || ny < 0
                                || nx >= area.width as i32
                                || ny >= area.height as i32
                            {
                                pixel(x, y)
                            } else {
                                pixel(nx, ny)
                            };
                            for c in 0..4 {
                                sum[c] += u32::from(rgba[c]);
                            }
                        }
                    }
                    sum.map(|v| (v >> 3) as u8)
                };
                let i =
                    (((y + area.top) as u32 * size.width + (x + area.left) as u32) * 4) as usize;
                result[i..i + 4].copy_from_slice(&value);
            }
        }
    }
    result
}

#[test]
fn clipped_neighborhoods_preserve_compact_sources_snapshots_and_provinces() {
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
    let area = Rect {
        left: 3,
        top: 2,
        width: 11,
        height: 9,
    };
    for stored in [
        size,
        Size {
            width: 9,
            height: 7,
        },
    ] {
        let mut source = gpu.logical_image(upload(&gpu, stored), size).unwrap();
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
        let raw = read(&gpu, &source);
        for (weights, passes) in [
            (None, 0),
            (None, 1),
            (None, 2),
            (None, 7),
            (Some(&[0.25, 0.5, 0.25][..]), 2),
            (Some(&[0.125, 0.125, 0.5, 0.125, 0.125][..]), 2),
            (Some(&[0.25, 1., 0.25][..]), 2),
        ] {
            let kind = if weights.is_some() {
                Kind::Gaussian
            } else {
                Kind::Smudge { passes }
            };
            let mut target = source.shared();
            gpu.adjust(
                &mut target,
                area,
                &operation(&gpu, kind, weights.unwrap_or(&[])),
            )
            .unwrap();
            assert_eq!(
                read(&gpu, &target),
                reference(&raw, size, area, weights, passes),
                "{kind:?}, stored {stored:?}"
            );
            assert_eq!(read(&gpu, &source), raw);
            assert!(
                gpu.readback(&target, size.rect(), true)
                    .unwrap()
                    .data
                    .as_slice()
                    .iter()
                    .all(|&v| v == 53)
            );
            drop(target);
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn gaussian_short_images_and_large_kernel_tables_retain_legacy_weight_indices() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 67,
                ..Default::default()
            },
        )
        .unwrap()
    };
    for (size, count) in [
        (
            Size {
                width: 1,
                height: 1,
            },
            5,
        ),
        (
            Size {
                width: 4,
                height: 7,
            },
            9,
        ),
        (
            Size {
                width: 143,
                height: 11,
            },
            65,
        ),
        (
            Size {
                width: 277,
                height: 3,
            },
            257,
        ),
    ] {
        // Binary fractions make the accumulation exact even across batches;
        // asymmetric coefficients expose the distinct small-image numerator.
        let weights: Vec<f32> = (0..count).map(|i| ((i % 7) + 1) as f32 / 2048.).collect();
        let mut target = upload(&gpu, size);
        let raw = read(&gpu, &target);
        gpu.adjust(
            &mut target,
            size.rect(),
            &operation(&gpu, Kind::Gaussian, &weights),
        )
        .unwrap();
        assert_eq!(
            read(&gpu, &target),
            reference(&raw, size, size.rect(), Some(&weights), 2),
            "size {size:?}, {count} taps"
        );
        drop(target);
        gpu.collect().unwrap();
    }
}

#[test]
fn repeated_smudge_and_gaussian_reuse_scratch_and_reject_unfunded_work_before_writes() {
    let context = support::Context::new();
    let mut gpu = unsafe { Gpu::new(context.gl(), Default::default()).unwrap() };
    let size = Size {
        width: 9,
        height: 7,
    };
    let source = upload(&gpu, size);
    let raw = read(&gpu, &source);
    let base = size.rgba_bytes().unwrap() * 2;
    // Only 64 bytes beyond the two region surfaces forces adaptive sub-blocks.
    gpu.scratch = Budget::new(base + 64);
    for (kind, weights) in [
        (Kind::Smudge { passes: 100 }, &[][..]),
        (Kind::Gaussian, &[0.25, 0.5, 0.25][..]),
    ] {
        let mut target = source.shared();
        gpu.adjust(&mut target, size.rect(), &operation(&gpu, kind, weights))
            .unwrap();
        let (kernel, passes) = match kind {
            Kind::Smudge { passes } => (None, passes),
            _ => (Some(weights), 2),
        };
        assert_eq!(
            read(&gpu, &target),
            reference(&raw, size, size.rect(), kernel, passes)
        );
        drop(target);
        gpu.collect().unwrap();
        assert_eq!(gpu.scratch.used(), 0);
    }
    gpu.scratch = Budget::new(0);
    let mut target = source.shared();
    gpu.adjust(
        &mut target,
        size.rect(),
        &operation(&gpu, Kind::Smudge { passes: 0 }, &[]),
    )
    .unwrap();
    for (kind, weights) in [
        (Kind::Smudge { passes: 1 }, &[][..]),
        (Kind::Gaussian, &[0.25, 0.5, 0.25][..]),
        (Kind::Gaussian, &[f32::NAN][..]),
        (Kind::Gaussian, &[-1.][..]),
        (Kind::Gaussian, &[0.][..]),
    ] {
        assert!(
            gpu.adjust(&mut target, size.rect(), &operation(&gpu, kind, weights))
                .is_err()
        );
        assert_eq!(read(&gpu, &target), raw);
        gpu.collect().unwrap();
    }
    assert_eq!(read(&gpu, &source), raw);
}

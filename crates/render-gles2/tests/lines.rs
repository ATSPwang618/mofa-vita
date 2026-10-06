#![cfg(any(target_os = "linux", feature = "windows-gles-tests"))]
mod support;
use krkr_protocol::{
    budget::Budget,
    graphics::{Adjustment, DrawFace, Fill, Rect, Size},
    lines::{Lines, TILE_SIZE},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::sync::Arc;

fn lines(gpu: &Gpu, rectangle: Rect, records: &[[u32; 8]]) -> Lines {
    let columns = rectangle.width.div_ceil(TILE_SIZE);
    let rows = rectangle.height.div_ceil(TILE_SIZE);
    let tiles = columns as usize * rows as usize;
    let start = 8 + tiles * 2;
    let references = start + records.len() * 8;
    // Every test tile intentionally references all records: the renderer must
    // still test coverage and preserve input order across texture/batch borders.
    let count = references + records.len() * tiles;
    let permit = gpu.staging.reserve(count * 4).unwrap();
    let mut words = vec![0; count];
    words[..8].copy_from_slice(&[
        rectangle.left as u32,
        rectangle.top as u32,
        columns,
        rows,
        start as u32,
        references as u32,
        rectangle.left as u32,
        rectangle.top as u32,
    ]);
    for tile in 0..tiles {
        words[8 + tile * 2] = (references + tile * records.len()) as u32;
        words[9 + tile * 2] = records.len() as u32;
        for i in 0..records.len() {
            words[references + tile * records.len() + i] = i as u32;
        }
    }
    for (i, record) in records.iter().enumerate() {
        words[start + i * 8..start + (i + 1) * 8].copy_from_slice(record);
    }
    Lines {
        rectangle,
        words,
        _permit: permit,
    }
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u32> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
        .collect()
}
fn rgba(c: u32) -> [u32; 4] {
    [c >> 16 & 255, c >> 8 & 255, c & 255, c >> 24]
}
fn pack(c: [u32; 4]) -> u32 {
    (c[3] << 24) | (c[0] << 16) | (c[1] << 8) | c[2]
}
fn reference(mut old: u32, x: i32, y: i32, records: &[[u32; 8]]) -> u32 {
    for r in records {
        let dx = (r[2] as i32).wrapping_sub(r[0] as i32);
        let dy = (r[3] as i32).wrapping_sub(r[1] as i32);
        let horizontal = dx.unsigned_abs() >= dy.unsigned_abs();
        let (major, minor, start, start_minor, pixel_major, pixel_minor) = if horizontal {
            (dx, dy, r[0] as i32, r[1] as i32, x, y)
        } else {
            (dy, dx, r[1] as i32, r[0] as i32, y, x)
        };
        let distance = major.abs().max(i32::from(r[5] != 0));
        let index = (pixel_major - start) * if major >= 0 { 1 } else { -1 };
        if index < 0 || index > distance {
            continue;
        }
        let mut ink = r[4];
        if r[5] == 0 {
            if distance == 0 {
                continue;
            }
            let first = index <= (distance + 1) / 2;
            let step = if first { index } else { distance - index };
            let shift = step
                .wrapping_mul(minor.abs())
                .wrapping_mul(2)
                .wrapping_add(distance)
                / distance.wrapping_mul(2).max(1);
            let direction = if minor >= 0 { 1 } else { -1 };
            let at = if first {
                start_minor + shift * direction
            } else {
                start_minor + minor - shift * direction
            };
            if pixel_minor == at {
                old = ink;
            }
            continue;
        }
        let step = minor.wrapping_mul(65536) / distance.max(1);
        let fixed = start_minor
            .wrapping_mul(65536)
            .wrapping_add(index.wrapping_mul(step));
        let low = fixed >> 16;
        let fraction = fixed as u32 & 65535;
        let weight = if pixel_minor == low {
            fraction
        } else if pixel_minor == low + 1 {
            65535 - fraction
        } else {
            continue;
        };
        if let Some(alpha) = (ink & 0xff000000).checked_div(r[6]) {
            let alpha = alpha.wrapping_mul((index as u32 + 1).min((distance as u32).min(r[6])));
            ink = ink & 0xffffff | alpha & 0xff000000;
        }
        let src = rgba(ink);
        let dst = rgba(old);
        old = pack(std::array::from_fn(|c| {
            src[c].wrapping_add(dst[c].wrapping_sub(src[c]).wrapping_mul(weight) >> 16)
        }));
    }
    old
}

#[test]
fn ordered_lines_keep_aa_fades_bresenham_ties_and_multiple_gpu_batches() {
    check_ordered_lines(false);
    check_ordered_lines(true);
}

fn check_ordered_lines(work_framebuffer: bool) {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 17,
                work_framebuffer,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 73,
        height: 57,
    };
    let rectangle = Rect {
        left: 3,
        top: 2,
        width: 67,
        height: 53,
    };
    let clip = Rect {
        left: 7,
        top: 5,
        width: 59,
        height: 47,
    };
    let mut records = vec![
        [3, 2, 69, 54, 0xffc347e1, 0, 0, 0],
        [69, 2, 3, 54, 0x01e579ad, 0, 0, 0],
        [30, 20, 30, 20, 0x107b63a1, 0, 0, 0],
        [30, 20, 30, 20, 0x807b63a1, 1, 1, 0],
    ];
    for i in 0..39u32 {
        records.push([
            3 + i * 11 % 67,
            2 + i * 17 % 53,
            3 + i * 31 % 67,
            2 + i * 23 % 53,
            0x9741d357u32.wrapping_mul(i + 1),
            i % 3,
            u32::from(i % 4 != 0) * [0, 1, 9, 100, u32::MAX][i as usize % 5],
            0,
        ]);
    }
    let mut target = gpu.create_image(size, 0x407593b1).unwrap();
    gpu.fill(
        &mut target,
        &[Fill {
            rectangle: size.rect(),
            color: 71,
            face: DrawFace::Province,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let original = target.shared();
    let operation = Adjustment::Lines(Arc::new(lines(&gpu, rectangle, &records)));
    gpu.adjust(&mut target, clip, &operation).unwrap();
    let expected: Vec<u32> = (0..size.height as i32)
        .flat_map(|y| {
            let records = &records;
            (0..size.width as i32).map(move |x| {
                if x >= clip.left
                    && y >= clip.top
                    && x < clip.left + clip.width as i32
                    && y < clip.top + clip.height as i32
                {
                    reference(0x407593b1, x, y, records)
                } else {
                    0x407593b1
                }
            })
        })
        .collect();
    let got = read(&gpu, &target);
    for (i, (&a, &b)) in got.iter().zip(&expected).enumerate() {
        assert_eq!(
            a,
            b,
            "at ({},{}): got {a:08x}, expected {b:08x}",
            i % size.width as usize,
            i / size.width as usize
        );
    }
    assert_eq!(read(&gpu, &original), vec![0x407593b1; expected.len()]);
    assert_eq!(
        gpu.readback(&target, size.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        vec![71; expected.len()]
    );
}

#[test]
fn line_batches_bound_scratch_and_reject_invalid_indexes_before_writes() {
    let context = support::Context::new();
    let scratch = Budget::new(16 * 10 * 4 + 32 * 32 * 4);
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
        width: 96,
        height: 64,
    };
    let mut target = gpu.create_image(size, 0x7193b557).unwrap();
    let records = (0..64)
        .map(|y| [0, y, 95, 63 - y, 0xfff3c741, 1, 27, 0])
        .collect::<Vec<_>>();
    let operation = Adjustment::Lines(Arc::new(lines(&gpu, size.rect(), &records)));
    gpu.adjust(&mut target, size.rect(), &operation).unwrap();
    assert_eq!(scratch.used(), scratch.limit());
    gpu.collect().unwrap();
    let before = read(&gpu, &target);
    let mut invalid = lines(&gpu, size.rect(), &records);
    invalid.words[8] = u32::MAX;
    assert!(
        gpu.adjust(
            &mut target,
            size.rect(),
            &Adjustment::Lines(Arc::new(invalid))
        )
        .is_err()
    );
    assert_eq!(read(&gpu, &target), before);
    assert_eq!(scratch.used(), 0);
}

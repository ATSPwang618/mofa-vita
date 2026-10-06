#![cfg(target_os = "linux")]
mod support;
use krkr_protocol::{
    budget::Budget,
    graphics::{Rect, Size},
    pixels::{Bytes, Pixels},
    transform::Perspective,
};
use krkr_render_gles2::{Config, Gpu, Image};

fn upload(gpu: &Gpu, size: Size, data: &[u8]) -> Image {
    let mut bytes = Bytes::zeroed(data.len(), &gpu.staging).unwrap();
    bytes.as_mut_slice().copy_from_slice(data);
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    gpu.upload(
        &mut image,
        &Pixels {
            size,
            main: Some(bytes),
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
fn pattern(size: Size) -> Vec<u8> {
    (0..size.width * size.height)
        .flat_map(|n| {
            [
                (n * 43 + 20) as u8,
                (n * 13 + 160) as u8,
                (n * 73 + 70) as u8,
                (n * 19 + 60) as u8,
            ]
        })
        .collect()
}
fn sample(data: &[u8], stored: Size, logical: Size, point: [f64; 2]) -> [f64; 4] {
    let p = [
        (point[0] - 0.5).clamp(0., f64::from(logical.width - 1)),
        (point[1] - 0.5).clamp(0., f64::from(logical.height - 1)),
    ];
    let [x, y] = p.map(f64::floor);
    let [fx, fy] = [p[0] - x, p[1] - y];
    let mut sum = [0.; 4];
    for (dx, dy, w) in [
        (0., 0., (1. - fx) * (1. - fy)),
        (1., 0., fx * (1. - fy)),
        (0., 1., (1. - fx) * fy),
        (1., 1., fx * fy),
    ] {
        let sx = (((x + dx).min(f64::from(logical.width - 1)) + 0.5) * f64::from(stored.width)
            / f64::from(logical.width))
        .floor() as usize;
        let sy = (((y + dy).min(f64::from(logical.height - 1)) + 0.5) * f64::from(stored.height)
            / f64::from(logical.height))
        .floor() as usize;
        for c in 0..4 {
            sum[c] += f64::from(data[(sy * stored.width as usize + sx) * 4 + c]) * w;
        }
    }
    sum
}
fn over(source: [f64; 4], dest: [u8; 4]) -> [u8; 4] {
    let a = source[3] / 255.;
    std::array::from_fn(|i| {
        (if i == 3 { source[3] } else { source[i] * a } + f64::from(dest[i]) * (1. - a)).round()
            as u8
    })
}
fn close(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len());
    for (i, (&a, &e)) in actual.iter().zip(expected).enumerate() {
        assert!(a.abs_diff(e) <= 1, "byte {i}: actual {a}, expected {e}");
    }
}

#[test]
fn perspective_samples_shared_inset_margins_like_a_dense_image() {
    use krkr_protocol::graphics::{DrawFace, Fill};
    let context = support::Context::new();
    let size = Size {
        width: 384,
        height: 256,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                tile_edge: 256,
                small_canvas_edge: 0,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let mut source = gpu.create_image(size, 0x80603010).unwrap();
    gpu.fill(
        &mut source,
        &[Fill {
            rectangle: Rect {
                left: 32,
                top: 32,
                width: 320,
                height: 192,
            },
            color: 0xc03588bd,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let bytes = read(&gpu, &source);
    let dense = upload(&gpu, size, &bytes);
    let mapping = Perspective {
        source: [0., 0., 384., 256.],
        destination: [[1.25, 1.5], [380.75, 3.25], [47.5, 252.5], [320.75, 250.25]],
    };
    let mut a = gpu.create_image(size, 0x78486090).unwrap();
    let mut b = a.shared();
    gpu.perspective(&mut a, &source, mapping, size.rect())
        .unwrap();
    gpu.perspective(&mut b, &dense, mapping, size.rect())
        .unwrap();
    close(&read(&gpu, &a), &read(&gpu, &b));
    assert_eq!(read(&gpu, &source), bytes);
}

#[test]
fn perspective_interpolates_across_four_tiles_without_materializing_compact_images() {
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
    let size = Size {
        width: 11,
        height: 9,
    };
    let background = [72, 96, 144, 120];
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
        let data = pattern(stored);
        let source = gpu
            .logical_image(upload(&gpu, stored, &data), logical)
            .unwrap();
        let mut target = gpu.create_image(size, 0x78486090).unwrap();
        let mapping = Perspective {
            source: [
                -1.,
                -0.5,
                f64::from(logical.width) + 1.,
                f64::from(logical.height) + 0.5,
            ],
            destination: [[1.1, 1.2], [9.3, 1.2], [3.15, 7.7], [7.25, 7.7]],
        };
        let clip = Rect {
            left: 2,
            top: 2,
            width: 8,
            height: 6,
        };
        let scratch = gpu.scratch.clone();
        gpu.scratch = Budget::new(0);
        gpu.perspective(&mut target, &source, mapping, clip)
            .unwrap();
        assert_eq!(gpu.scratch.used(), 0);
        assert_eq!(source.stored_size(), Some(stored));
        gpu.scratch = scratch;
        let mut expected = background.repeat((size.width * size.height) as usize);
        for y in 2..8 {
            for x in 2..10 {
                let t = f64::from(y) + 0.5 - 1.2;
                let left = 1.1 + 2.05 * t / 6.5;
                let width = 8.2 - 4.1 * t / 6.5;
                if !(0. ..6.5).contains(&t)
                    || f64::from(x) + 0.5 < left
                    || f64::from(x) + 0.5 >= left + width
                {
                    continue;
                }
                let u = (f64::from(x) + 0.5 - left) / width;
                let v = t / (13. - t);
                let p = [
                    -1. + u * (f64::from(logical.width) + 2.),
                    -0.5 + v * (f64::from(logical.height) + 1.),
                ];
                let color = over(sample(&data, stored, logical, p), background);
                let at = (y * size.width + x) as usize * 4;
                expected[at..at + 4].copy_from_slice(&color);
            }
        }
        close(&read(&gpu, &target), &expected);
    }
}

#[test]
fn perspective_aliases_reverse_edges_preserve_province_and_reject_singular_mapping() {
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
        width: 6,
        height: 4,
    };
    let data = pattern(size);
    let mut target = upload(&gpu, size, &data);
    let mut province = Bytes::zeroed(24, &gpu.staging).unwrap();
    province.as_mut_slice().fill(73);
    gpu.patch_pixels(
        &mut target,
        &Pixels {
            size,
            main: None,
            province: Some(province),
        },
    )
    .unwrap();
    let before = target.shared();
    gpu.perspective(
        &mut target,
        &before,
        Perspective {
            source: [6., 4., 0., 0.],
            destination: [[0., 0.], [6., 0.], [0., 4.], [6., 4.]],
        },
        size.rect(),
    )
    .unwrap();
    let expected: Vec<_> = (0..24)
        .flat_map(|i| {
            over(
                data[(23 - i) * 4..(24 - i) * 4]
                    .try_into()
                    .map(|a: [u8; 4]| a.map(f64::from))
                    .unwrap(),
                data[i * 4..i * 4 + 4].try_into().unwrap(),
            )
        })
        .collect();
    close(&read(&gpu, &target), &expected);
    assert_eq!(read(&gpu, &before), data);
    assert_eq!(
        gpu.readback(&target, size.rect(), true)
            .unwrap()
            .data
            .as_slice(),
        &[73; 24]
    );
    let valid = read(&gpu, &target);
    assert!(
        gpu.perspective(
            &mut target,
            &before,
            Perspective {
                source: [0., 0., 6., 4.],
                destination: [[1., 1.]; 4],
            },
            size.rect()
        )
        .is_err()
    );
    assert_eq!(read(&gpu, &target), valid);
}

#[test]
fn perspective_handles_source_storage_larger_than_logical_size() {
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
        width: 7,
        height: 5,
    };
    let size = Size {
        width: 3,
        height: 2,
    };
    let data = pattern(stored);
    let source = gpu
        .logical_image(upload(&gpu, stored, &data), size)
        .unwrap();
    let mut target = gpu.create_image(size, 0x78486090).unwrap();
    gpu.perspective(
        &mut target,
        &source,
        Perspective {
            source: [0., 0., 3., 2.],
            destination: [[0., 0.], [3., 0.], [0., 2.], [3., 2.]],
        },
        size.rect(),
    )
    .unwrap();
    let expected: Vec<_> = (0..2)
        .flat_map(|y| (0..3).map(move |x| [f64::from(x) + 0.5, f64::from(y) + 0.5]))
        .flat_map(|p| over(sample(&data, stored, size, p), [72, 96, 144, 120]))
        .collect();
    close(&read(&gpu, &target), &expected);
}

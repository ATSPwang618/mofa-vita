#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{Adjustment, DrawFace, Fill, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn small_shared_page_writes_copy_bands_and_preserve_other_rows() {
    use krkr_protocol::graphics::{Blend, BlendOptions};
    let context = support::Context::new();
    let size = Size {
        width: 960,
        height: 576,
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
    let original: Vec<u8> = (0..size.width * size.height)
        .flat_map(|i| [i as u8, (i / size.width) as u8, 75, 255])
        .collect();
    let source = pixels(&gpu, size, original.clone());
    let mut page = source.shared();
    let stamp_size = Size {
        width: 32,
        height: 32,
    };
    let stamp = gpu.create_image(stamp_size, 0xff17a542).unwrap();
    let mut expected = original.clone();
    for (index, (x, y)) in [
        (145, 83),
        (220, 83),
        (145, 163),
        (90, 259),
        (190, 430),
        (142, 540),
    ]
    .into_iter()
    .enumerate()
    {
        gpu.resolve().unwrap();
        let estimated = gpu.canvas_blend_write_bytes(
            &page,
            Rect {
                left: x,
                top: y,
                ..stamp_size.rect()
            },
            false,
        );
        let pressure = (index == 0).then(|| {
            gpu.resident
                .reserve(gpu.resident.available() - estimated)
                .unwrap()
        });
        let before = gpu.resident.used();
        traffic::reset();
        gpu.operate(
            &mut page,
            &stamp,
            stamp_size.rect(),
            x,
            y,
            size.rect(),
            BlendOptions {
                mode: Blend::Opaque,
                face: DrawFace::Opaque,
                opacity: 255,
                hold_alpha: false,
            },
        )
        .unwrap();
        gpu.resolve().unwrap();
        assert!(gpu.resident.used().saturating_sub(before) <= estimated);
        if index == 0 {
            assert!(gpu.resident.used() - before < size.rgba_bytes().unwrap() / 4);
            assert!(traffic::stored_pixels() < (size.width * size.height / 4) as usize);
            assert_eq!(traffic::read_calls(), 0);
            assert!(traffic::loaded_pixels() < (size.width * size.height / 4) as usize);
        }
        drop(pressure);
        for row in y..y + 32 {
            for col in x..x + 32 {
                let at = ((row as u32 * size.width + col as u32) * 4) as usize;
                expected[at..at + 4].copy_from_slice(&[0x17, 0xa5, 0x42, 255]);
            }
        }
        for (image, expected) in [(&page, &expected), (&source, &original)] {
            let actual = gpu.readback(image, size.rect(), false).unwrap();
            let difference = actual
                .data
                .as_slice()
                .iter()
                .zip(expected)
                .enumerate()
                .find(|(_, (a, b))| a != b);
            assert_eq!(difference, None, "write {index}");
        }
    }
}

fn pixels(gpu: &Gpu, size: Size, data: Vec<u8>) -> krkr_render_gles2::Image {
    let mut image = gpu.reserve_upload(size, true, false).unwrap();
    let permit = gpu.staging.reserve(data.len()).unwrap();
    gpu.upload(
        &mut image,
        &Pixels {
            size,
            main: Some(Bytes::with_permit(data, permit)),
            province: None,
        },
    )
    .unwrap();
    gpu.logical_image(image, size).unwrap()
}

#[test]
fn clipped_rgba_views_preserve_reads_snapshots_materialization_and_neighbour_kernels() {
    let context = support::Context::new();
    let size = Size {
        width: 512,
        height: 384,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                small_canvas_edge: 0,
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let original: Vec<u8> = (0..size.height)
        .flat_map(|y| {
            (0..size.width).flat_map(move |x| {
                [
                    (x * 13 + y * 7) as u8,
                    (x * 3 + y * 17) as u8,
                    (x ^ y) as u8,
                    (x + y) as u8,
                ]
            })
        })
        .collect();
    let mut image = pixels(&gpu, size, original.clone());
    let snapshot = image.shared();
    let fills = [
        Fill {
            rectangle: Rect {
                left: 0,
                top: 0,
                width: 512,
                height: 32,
            },
            color: 0,
            face: DrawFace::Alpha,
            hold_alpha: false,
        },
        Fill {
            rectangle: Rect {
                left: 0,
                top: 32,
                width: 16,
                height: 352,
            },
            color: 0,
            face: DrawFace::Alpha,
            hold_alpha: false,
        },
    ];
    gpu.collect().unwrap();
    let before = gpu.resident.used();
    let pressure = gpu
        .resident
        .reserve(gpu.resident.available() - 4096)
        .unwrap();
    for fill in &fills {
        assert!(gpu.fill_write_bytes(&image, std::slice::from_ref(fill)) <= 4);
        gpu.fill(&mut image, std::slice::from_ref(fill)).unwrap();
    }
    drop(pressure);
    assert!(gpu.resident.used() - before <= 8);
    let mut expected = original.clone();
    for y in 0..size.height {
        for x in 0..size.width {
            if y < 32 || x < 16 {
                let i = ((y * size.width + x) * 4) as usize;
                expected[i..i + 4].fill(0);
            }
        }
    }
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
    assert_eq!(
        gpu.readback(&snapshot, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        original
    );
    let frozen = image.shared();
    let rect = Rect {
        left: 90,
        top: 70,
        width: 20,
        height: 12,
    };
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: rect,
            color: 0xffabcdef,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    for y in 70..82 {
        for x in 90..110 {
            let i = ((y * size.width + x) * 4) as usize;
            expected[i..i + 4].copy_from_slice(&[0xab, 0xcd, 0xef, 255]);
        }
    }
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
    gpu.independ(&mut image, false, true).unwrap();
    assert_eq!(
        gpu.readback(&image, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        expected
    );
    let mut dense = pixels(
        &gpu,
        size,
        gpu.readback(&frozen, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .to_vec(),
    );
    {
        use krkr_protocol::transform::{Filter, ImageOperation, Sampling, StretchRect, Transform};
        let output = Size {
            width: 271,
            height: 203,
        };
        let mut a = gpu.create_image(output, 0xff112233).unwrap();
        let mut b = gpu.create_image(output, 0xff112233).unwrap();
        for (target, source) in [(&mut a, &frozen), (&mut b, &dense)] {
            gpu.transform(
                target,
                source,
                size.rect(),
                Transform::Stretch(StretchRect {
                    left: 0,
                    top: 0,
                    width: 271,
                    height: 203,
                }),
                Sampling {
                    filter: Filter::Linear,
                    sharpness: -1.,
                    no_clip: false,
                },
                ImageOperation::Copy { hold_alpha: false },
                output.rect(),
                None,
            )
            .unwrap();
        }
        assert_eq!(
            gpu.readback(&a, output.rect(), false)
                .unwrap()
                .data
                .as_slice(),
            gpu.readback(&b, output.rect(), false)
                .unwrap()
                .data
                .as_slice()
        );
    }
    let mut blurred = frozen.shared();
    let blur = Adjustment::BoxBlur {
        radius: [2, 2],
        alpha: true,
    };
    gpu.adjust(&mut blurred, size.rect(), &blur).unwrap();
    gpu.adjust(&mut dense, size.rect(), &blur).unwrap();
    assert_eq!(
        gpu.readback(&blurred, size.rect(), false)
            .unwrap()
            .data
            .as_slice(),
        gpu.readback(&dense, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
    );
}

use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, BlendOptions, DrawFace, Fill, Rect, Size},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render_wgpu::{Gpu, Image};
use std::time::{Duration, Instant};

fn gpu() -> Gpu {
    pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap()
}
fn sampling(filter: Filter) -> Sampling {
    Sampling {
        filter,
        sharpness: -1.0,
        no_clip: false,
    }
}
fn copy() -> ImageOperation {
    ImageOperation::Copy { hold_alpha: false }
}
fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let start = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn row(gpu: &Gpu, colors: &[u32]) -> Image {
    let mut image = gpu
        .create_image(
            Size {
                width: colors.len() as u32,
                height: 1,
            },
            0,
        )
        .unwrap();
    let fills: Vec<_> = colors
        .iter()
        .enumerate()
        .map(|(i, &color)| Fill {
            rectangle: Rect {
                left: i as i32,
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
fn stretch(width: i32, height: i32) -> Transform {
    Transform::Stretch(StretchRect {
        left: 0,
        top: 0,
        width,
        height,
    })
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn compact_upload_preserves_logical_crop_and_copy_on_write() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    let gpu = gpu();
    let budget = Budget::new(1024 * 1024);
    let mut bytes = Bytes::zeroed(8, &budget).unwrap();
    bytes
        .as_mut_slice()
        .copy_from_slice(&[17, 34, 51, 64, 85, 102, 119, 128]);
    let stored = Size {
        width: 2,
        height: 1,
    };
    let logical = Size {
        width: 4,
        height: 2,
    };
    let mut image = gpu.create_image(stored, 0).unwrap();
    gpu.upload_scaled(
        &mut image,
        &Pixels {
            size: stored,
            main: Some(bytes),
            province: None,
        },
        logical,
    )
    .unwrap();
    assert_eq!(image.size, logical);
    let expected = [
        17, 34, 51, 64, 17, 34, 51, 64, 85, 102, 119, 128, 85, 102, 119, 128,
    ]
    .repeat(2);
    assert_eq!(pixels(&gpu, &image), expected);
    let mut cropped = gpu
        .create_image(
            Size {
                width: 2,
                height: 2,
            },
            0,
        )
        .unwrap();
    let clip = cropped.size.rect();
    gpu.transform(
        &mut cropped,
        &image.source(),
        Rect {
            left: 2,
            top: 0,
            width: 2,
            height: 2,
        },
        stretch(2, 2),
        sampling(Filter::Nearest),
        copy(),
        clip,
        None,
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &cropped), [85, 102, 119, 128].repeat(4));
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                left: 3,
                top: 1,
                width: 1,
                height: 1,
            },
            color: 0xffff0000,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(&pixels(&gpu, &image)[28..], &[255, 0, 0, 255]);
    assert_eq!(pixels(&gpu, &cropped), [85, 102, 119, 128].repeat(4));
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn nearest_stretch_flip_and_affine_pixel_centers() {
    let gpu = gpu();
    let source = row(&gpu, &[0x40112233, 0x80556677]);
    let size = Size {
        width: 4,
        height: 2,
    };
    let mut output = gpu.create_image(size, 0x12345678).unwrap();
    gpu.transform(
        &mut output,
        &source.source(),
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
        &source.source(),
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
        &source.source(),
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
        &source.source(),
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
#[ignore = "requires a real desktop GPU"]
fn linear_borders_hold_alpha_and_affine_clear() {
    let gpu = gpu();
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
        &source.source(),
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
        &source.source(),
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
        &source.source(),
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
        &source.source(),
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
#[ignore = "requires a real desktop GPU"]
fn every_stretch_filter_uses_its_kernel_and_then_blends() {
    let gpu = gpu();
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
            &source.source(),
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
        &source.source(),
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
        &source.source(),
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
#[ignore = "requires a real desktop GPU"]
fn transformed_self_writes_and_rejected_budgets_keep_owned_pixels() {
    let mut gpu = gpu();
    let mut output = row(&gpu, &[0x64141414, 0x64505050, 0x648c8c8c, 0x64c8c8c8]);
    let size = output.size;
    let saved = output.shared();
    let source = output.source();
    gpu.transform(
        &mut output,
        &source,
        size.rect(),
        stretch(2, 1),
        sampling(Filter::Area),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &output),
        [
            50, 50, 50, 100, 170, 170, 170, 100, 140, 140, 140, 100, 200, 200, 200, 100
        ]
    );
    assert_eq!(
        pixels(&gpu, &saved),
        [
            20, 20, 20, 100, 80, 80, 80, 100, 140, 140, 140, 100, 200, 200, 200, 100
        ]
    );
    let source = output.source();
    gpu.transform(
        &mut output,
        &source,
        size.rect(),
        stretch(2, 1),
        sampling(Filter::Area),
        copy(),
        size.rect(),
        None,
    )
    .unwrap();
    assert_eq!(
        pixels(&gpu, &output),
        [
            110, 110, 110, 100, 170, 170, 170, 100, 140, 140, 140, 100, 200, 200, 200, 100
        ]
    );
    // Self-affine sampling needs a source snapshot; a rejected reservation
    // must leave both the current image and its shared predecessor intact.
    gpu.trim_scratch();
    gpu.scratch = Budget::new(3);
    let before = pixels(&gpu, &output);
    let source = output.source();
    assert!(
        gpu.transform(
            &mut output,
            &source,
            size.rect(),
            Transform::Affine([[3.5, -0.5], [-0.5, -0.5], [3.5, 0.5]]),
            sampling(Filter::Nearest),
            copy(),
            size.rect(),
            None
        )
        .is_err()
    );
    assert_eq!(pixels(&gpu, &output), before);
    assert_eq!(gpu.staging.used(), 0);
    gpu.scratch = Budget::new(4);
    let large = Size {
        width: 960,
        height: 544,
    };
    let mut image = gpu.create_image(large, 0x80402010).unwrap();
    let source = image.source();
    gpu.transform(
        &mut image,
        &source,
        large.rect(),
        Transform::Affine([[-0.5, -0.5], [959.5, -0.5], [-0.5, 543.5]]),
        sampling(Filter::Nearest),
        copy(),
        Rect {
            left: 500,
            top: 200,
            width: 1,
            height: 1,
        },
        None,
    )
    .unwrap();
    assert_eq!(
        gpu.scratch.used(),
        4,
        "a one-pixel self-write must not copy the 960x544 source"
    );
    assert_eq!(
        &pixels(&gpu, &image)[(200 * 960 + 500) * 4..(200 * 960 + 500) * 4 + 4],
        &[64, 32, 16, 128]
    );
}

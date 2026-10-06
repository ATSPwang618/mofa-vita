use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    transform::Perspective,
};
use krkr_render_wgpu::{Gpu, Image};
use std::time::{Duration, Instant};

fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn rectangle(width: f64, height: f64) -> [[f64; 2]; 4] {
    [[0.0, 0.0], [width, 0.0], [0.0, height], [width, height]]
}
fn close(actual: &[u8], expected: &[u8], label: &str) {
    assert_eq!(actual.len(), expected.len());
    for (i, (a, b)) in actual.iter().zip(expected).enumerate() {
        assert!(a.abs_diff(*b) <= 2, "{label} channel {i}: {a} != {b}");
    }
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn projective_pixels_clipping_aliasing_and_budget_failure() {
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
    eprintln!("perspective GPU: {:?}", gpu.adapter.get_info());
    let mut source = gpu
        .create_image(
            Size {
                width: 4,
                height: 4,
            },
            0,
        )
        .unwrap();
    let fills: Vec<_> = (0..4)
        .flat_map(|y| {
            (0..4).map(move |x| Fill {
                rectangle: Rect {
                    left: x,
                    top: y,
                    width: 1,
                    height: 1,
                },
                color: 0x80000014 | ((40 + 40 * x) as u32) << 16 | ((30 + 50 * y) as u32) << 8,
                face: DrawFace::Alpha,
                hold_alpha: false,
            })
        })
        .collect();
    gpu.fill(&mut source, &fills).unwrap();
    let size = Size {
        width: 8,
        height: 8,
    };
    // Analytic projective map x=8u/(1+u/2), y=8v/(1+u/2).
    // This oracle does not use the production homography solver.
    let mapping = Perspective {
        source: [0.0, 0.0, 4.0, 4.0],
        destination: [
            [0.0, 0.0],
            [16.0 / 3.0, 0.0],
            [0.0, 8.0],
            [16.0 / 3.0, 16.0 / 3.0],
        ],
    };
    for clip in [
        size.rect(),
        Rect {
            left: 1,
            top: 1,
            width: 4,
            height: 5,
        },
    ] {
        let mut output = gpu.create_image(size, 0x40141414).unwrap();
        gpu.perspective(&mut output, &source.source(), mapping, clip)
            .unwrap();
        let mut expected = Vec::new();
        for y in 0..8 {
            for x in 0..8 {
                let denominator = 8.0 - 0.5 * (x as f64 + 0.5);
                let u = (x as f64 + 0.5) / denominator;
                let v = (y as f64 + 0.5) / denominator;
                if u < 1.0
                    && v < 1.0
                    && x >= clip.left
                    && y >= clip.top
                    && x < clip.left + clip.width as i32
                    && y < clip.top + clip.height as i32
                {
                    let rgba = [
                        40.0 + 40.0 * (4.0 * u - 0.5).clamp(0.0, 3.0),
                        30.0 + 50.0 * (4.0 * v - 0.5).clamp(0.0, 3.0),
                        20.0,
                    ];
                    expected.extend(
                        rgba.map(|s| (s * 128.0 / 255.0 + 20.0 * 127.0 / 255.0).round() as u8),
                    );
                    expected.push((128.0f64 + 64.0 * 127.0 / 255.0).round() as u8);
                } else {
                    expected.extend([20, 20, 20, 64]);
                }
            }
        }
        close(
            &pixels(&gpu, &output),
            &expected,
            "projective bilinear and alpha",
        );
    }
    // Full source-edge clamp, reversed sampling and both COW/self-overlap paths.
    let mapping = Perspective {
        source: [5.0, -1.0, -1.0, 5.0],
        destination: rectangle(4.0, 4.0),
    };
    let before = pixels(&gpu, &source);
    let saved = source.shared();
    let mut expected = source.shared();
    gpu.perspective(&mut expected, &source.source(), mapping, source.size.rect())
        .unwrap();
    let input = source.source();
    let clip = source.size.rect();
    gpu.perspective(&mut source, &input, mapping, clip).unwrap();
    close(
        &pixels(&gpu, &source),
        &pixels(&gpu, &expected),
        "shared source detach",
    );
    assert_eq!(pixels(&gpu, &saved), before);
    let input = source.source();
    // No logical second owner: this operation must snapshot the old pixels.
    gpu.perspective(&mut source, &input, mapping, clip).unwrap();
    let independent_reference = expected.shared();
    let input = independent_reference.source();
    gpu.perspective(&mut expected, &input, mapping, clip)
        .unwrap();
    close(
        &pixels(&gpu, &source),
        &pixels(&gpu, &expected),
        "overlapping self source",
    );
    let before = pixels(&gpu, &source);
    for bad in [
        Perspective {
            destination: [[0.0, 0.0]; 4],
            ..mapping
        },
        Perspective {
            source: [f64::NAN, 0.0, 4.0, 4.0],
            ..mapping
        },
    ] {
        assert!(
            gpu.perspective(&mut source, &saved.source(), bad, clip)
                .is_err()
        );
    }
    gpu.trim_scratch();
    gpu.scratch = Budget::new(3);
    let input = source.source();
    assert!(gpu.perspective(&mut source, &input, mapping, clip).is_err());
    assert_eq!(pixels(&gpu, &source), before);
    gpu.staging = Budget::new(1);
    assert!(
        gpu.perspective(&mut source, &saved.source(), mapping, clip)
            .is_err()
    );
    gpu.staging = Budget::new(32 * 1024 * 1024);
    assert_eq!(pixels(&gpu, &source), before);
    // A one-pixel update of a 2K image must reserve a local source footprint.
    gpu.trim_scratch();
    gpu.scratch = Budget::new(256);
    let size = Size {
        width: 2560,
        height: 1440,
    };
    let mut large = gpu.create_image(size, 0xff204060).unwrap();
    let input = large.source();
    gpu.perspective(
        &mut large,
        &input,
        Perspective {
            source: [0.0, 0.0, 2560.0, 1440.0],
            destination: rectangle(2560.0, 1440.0),
        },
        Rect {
            left: 1200,
            top: 700,
            width: 1,
            height: 1,
        },
    )
    .unwrap();
    assert!(gpu.scratch.used() <= 256);
    assert_eq!(
        &pixels(&gpu, &large)[(700 * 2560 + 1200) * 4..(700 * 2560 + 1200) * 4 + 4],
        &[32, 64, 96, 255]
    );
    assert_eq!(gpu.staging.used(), 0);
}

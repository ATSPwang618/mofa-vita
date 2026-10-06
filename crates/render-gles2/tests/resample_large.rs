#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
#[allow(dead_code)]
mod traffic;
use krkr_protocol::{
    graphics::Size,
    pixels::{Bytes, Pixels},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render_gles2::{Config, Gpu};

// A compact full-HD script canvas and save-sized target exercise the Vita path.
// Splitting the source into tiles forces the independent CPU row implementation.
#[test]
fn large_reductions_match_cpu_rounding_without_readback() {
    let context = support::Context::new();
    let logical = Size {
        width: 1920,
        height: 1080,
    };
    let stored = Size {
        width: 960,
        height: 540,
    };
    let input: Vec<u8> = (0..stored.height)
        .flat_map(|y| {
            (0..stored.width).flat_map(move |x| {
                // Spatial detail, transparency, and saturated values expose errors at
                // kernel edges and the u8 intermediate between the two filter axes.
                [
                    (x.wrapping_mul(29) ^ y.wrapping_mul(7)) as u8,
                    (x.wrapping_mul(3) + y.wrapping_mul(47)) as u8,
                    if (x / 23 + y / 31) % 2 == 0 { 0 } else { 255 },
                    (x + y * 11) as u8,
                ]
            })
        })
        .collect();
    for (filter, flip, clipped, width, height) in [
        (Filter::Cubic, false, false, 301, 171),
        (Filter::Cubic, true, true, 301, 171),
        (Filter::Linear, false, true, 301, 171),
        (Filter::Lanczos2, true, false, 301, 171),
        (Filter::Lanczos3, false, true, 301, 171),
        (Filter::Cubic, true, false, 160, 90),
    ] {
        let output = Size { width, height };
        let mut reference = None;
        for tile_edge in [256, 1024] {
            let gpu = unsafe {
                Gpu::new(
                    context.gl_with(traffic::intercept),
                    Config {
                        work_framebuffer: true,
                        tile_edge,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let mut source = gpu.create_image(stored, 0).unwrap();
            gpu.upload(
                &mut source,
                &Pixels {
                    size: stored,
                    main: Some(Bytes::with_permit(
                        input.clone(),
                        gpu.staging.reserve(input.len()).unwrap(),
                    )),
                    province: None,
                },
            )
            .unwrap();
            let source = gpu.logical_image(source, logical).unwrap();
            let mut target = gpu.create_image(output, 0x9f37518d).unwrap();
            let mut clip = output.rect();
            if clipped {
                clip.left = 7;
                clip.top = 9;
                clip.width -= 18;
                clip.height -= 21;
            }
            traffic::reset();
            gpu.transform(
                &mut target,
                &source,
                logical.rect(),
                Transform::Stretch(StretchRect {
                    left: if flip { output.width as i32 } else { 0 },
                    top: if flip { output.height as i32 } else { 0 },
                    width: if flip {
                        -(output.width as i32)
                    } else {
                        output.width as i32
                    },
                    height: if flip {
                        -(output.height as i32)
                    } else {
                        output.height as i32
                    },
                }),
                Sampling {
                    filter,
                    sharpness: -1.,
                    no_clip: false,
                },
                ImageOperation::Copy {
                    hold_alpha: clipped,
                },
                clip,
                None,
            )
            .unwrap();
            if tile_edge == 1024 {
                assert_eq!(
                    traffic::read_calls(),
                    0,
                    "GPU reduction read pixels: {filter:?}"
                );
            } else {
                assert!(traffic::read_calls() > 0, "CPU reference was not exercised");
            }
            let actual = gpu
                .readback(&target, output.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec();
            if let Some(expected) = reference.as_ref() {
                let expected: &Vec<u8> = expected;
                for (index, (&a, &b)) in actual.iter().zip(expected).enumerate() {
                    assert!(
                        (i16::from(a) - i16::from(b)).abs() <= 1,
                        "{filter:?} flip={flip} clipped={clipped} byte={index}: GPU={a} CPU={b}"
                    );
                }
            } else {
                reference = Some(actual);
            }
        }
    }
}

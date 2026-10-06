#![cfg(target_os = "linux")]
mod support;
#[allow(dead_code)]
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{DrawFace, Fill, Rect, Size},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render_gles2::{Config, Gpu};

#[test]
fn direct_filters_match_cpu_rows_for_compact_flipped_clipped_rgba_without_readback() {
    let context = support::Context::new();
    for filter in [
        Filter::Linear,
        Filter::Cubic,
        Filter::Lanczos3,
        Filter::Area,
    ] {
        let mut results = Vec::new();
        for tile_edge in [4, 64] {
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
            let size = Size {
                width: 7,
                height: 5,
            };
            let mut source = gpu.create_image(size, 0).unwrap();
            let fills: Vec<_> = (0..size.height)
                .flat_map(|y| {
                    (0..size.width).map(move |x| Fill {
                        rectangle: Rect {
                            left: x as i32,
                            top: y as i32,
                            width: 1,
                            height: 1,
                        },
                        color: (((x * 31 + y * 47) % 256) << 24)
                            | (((x * 53 + y * 7) % 256) << 16)
                            | (((x * 11 + y * 61) % 256) << 8)
                            | ((x * 37 + y * 17) % 256),
                        face: DrawFace::Alpha,
                        hold_alpha: false,
                    })
                })
                .collect();
            gpu.fill(&mut source, &fills).unwrap();
            let source = gpu
                .logical_image(
                    source,
                    Size {
                        width: 14,
                        height: 10,
                    },
                )
                .unwrap();
            let output_size = Size {
                width: 18,
                height: 14,
            };
            let mut output = gpu.create_image(output_size, 0x79533197).unwrap();
            gpu.resolve().unwrap();
            traffic::reset();
            gpu.transform(
                &mut output,
                &source,
                Rect {
                    left: 2,
                    top: 2,
                    width: 10,
                    height: 6,
                },
                Transform::Stretch(if filter == Filter::Area {
                    StretchRect {
                        left: 10,
                        top: 8,
                        width: -5,
                        height: -4,
                    }
                } else {
                    StretchRect {
                        left: 16,
                        top: 12,
                        width: -14,
                        height: -10,
                    }
                }),
                Sampling {
                    filter,
                    sharpness: -1.,
                    no_clip: false,
                },
                ImageOperation::Copy { hold_alpha: false },
                Rect {
                    left: 3,
                    top: 3,
                    width: 12,
                    height: 8,
                },
                None,
            )
            .unwrap();
            let reads = traffic::read_calls();
            if tile_edge == 64 {
                assert_eq!(
                    reads, 0,
                    "{filter:?} fast filter read pixels back to the CPU"
                );
            } else {
                assert!(reads > 0, "reference must exercise the CPU fallback");
            }
            results.push(
                gpu.readback(&output, output_size.rect(), false)
                    .unwrap()
                    .data
                    .as_slice()
                    .to_vec(),
            );
        }
        for (index, (&a, &b)) in results[0].iter().zip(&results[1]).enumerate() {
            assert!(
                (i16::from(a) - i16::from(b)).abs() <= 1,
                "{filter:?} byte {index}: CPU={a}, GPU={b}"
            );
        }
    }
}

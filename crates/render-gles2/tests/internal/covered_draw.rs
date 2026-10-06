use super::*;
use crate::{Config, Gpu, drawing::Sample, test_support::Context};
use krkr_protocol::{
    graphics::Size,
    pixels::{Bytes, Pixels},
};

#[test]
fn proven_raw_draws_omit_fragment_kill_and_keep_edges_masks_and_compact_pixels() {
    for work in [false, true] {
        let context = Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    canvas_limit: None,
                    tile_edge: 64,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 19,
            height: 13,
        };
        let size = Size {
            width: 17,
            height: 11,
        };
        let raw: Vec<u8> = (0..stored.width * stored.height)
            .flat_map(|i| {
                [
                    (i * 31) as u8,
                    (i * 47) as u8,
                    (i * 73) as u8,
                    (i * 11) as u8,
                ]
            })
            .collect();
        let mut bytes = Bytes::zeroed(raw.len(), &gpu.staging).unwrap();
        bytes.as_mut_slice().copy_from_slice(&raw);
        let mut source = gpu.reserve_upload(stored, true, false).unwrap();
        gpu.upload(
            &mut source,
            &Pixels {
                size: stored,
                main: Some(bytes),
                province: None,
            },
        )
        .unwrap();
        for (mapping, compact, mask, covered) in [
            ([1., 0., 1., 0., 1., 1.], false, [true; 4], true),
            ([-1., 0., 18., 0., 1., 1.], false, [true; 4], true),
            ([2., 0., 0.25, 0., 2., 0.25], true, [true; 4], true),
            ([-1., 0., 19., 0., 1., 1.], false, [true; 4], false),
            (
                [1., 0., 1., 0., 1., 1.],
                false,
                [true, false, true, true],
                false,
            ),
        ] {
            let mut draw = Draw::copy(mapping, mask);
            if compact {
                let logical = Size {
                    width: 38,
                    height: 26,
                };
                draw.sampling = Some(Sample {
                    display: false,
                    region: logical.rect(),
                    bounds: logical.rect(),
                    linear: false,
                    clear: false,
                    scale: Some([0.5; 2]),
                });
            }
            let image = gpu.create_image(size, 0x91355779).unwrap();
            gpu.program.cache.borrow_mut().clear();
            gpu.draw(
                image.plane(false).unwrap(),
                Some(source.plane(false).unwrap()),
                size.rect(),
                &draw,
            )
            .unwrap();
            let used: Vec<_> = gpu
                .program
                .cache
                .borrow()
                .iter()
                .map(|(key, _)| *key)
                .collect();
            assert_eq!(
                used.iter().any(|key| key.covered),
                covered,
                "work={work}, map={mapping:?}"
            );
            for key in used.iter().filter(|key| key.covered) {
                assert!(!crate::draw_source::fragment(*key).contains("discard"));
            }
            let pixels = gpu.readback(&image, size.rect(), false).unwrap();
            for y in 0..size.height {
                for x in 0..size.width {
                    let mut q = [
                        mapping[0] * x as f32 + mapping[2],
                        mapping[4] * y as f32 + mapping[5],
                    ];
                    for point in &mut q {
                        *point = (*point + 0.5).floor();
                        if compact {
                            *point = ((*point + 0.5) * 0.5).floor();
                        }
                    }
                    let mut expected = [0x35, 0x57, 0x79, 0x91];
                    if q[0] >= 0.
                        && q[0] < stored.width as f32
                        && q[1] >= 0.
                        && q[1] < stored.height as f32
                    {
                        let at = (q[1] as usize * stored.width as usize + q[0] as usize) * 4;
                        for channel in 0..4 {
                            if mask[channel] {
                                expected[channel] = raw[at + channel];
                            }
                        }
                    }
                    let at = ((y * size.width + x) * 4) as usize;
                    assert_eq!(
                        &pixels.data.as_slice()[at..at + 4],
                        &expected,
                        "work={work}, compact={compact}, map={mapping:?}, pixel=({x},{y})"
                    );
                }
            }
        }
    }
}

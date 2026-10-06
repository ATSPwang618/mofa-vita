use super::*;
use crate::{Config, test_support::Context};
use krkr_protocol::pixels::{Bytes, Pixels};

fn reference_raster_rect(raster: Raster, area: Rect) -> Option<Rect> {
    let edge = |at: i64, origin: i32, logical: u32, physical: u32| {
        let n = 2 * i128::from(at - i64::from(origin)) * i128::from(physical) - i128::from(logical);
        (-(-n).div_euclid(2 * i128::from(logical))).clamp(0, i128::from(physical)) as i32
    };
    let left = edge(
        i64::from(area.left),
        raster.origin.0,
        raster.logical.width,
        raster.physical.width,
    );
    let top = edge(
        i64::from(area.top),
        raster.origin.1,
        raster.logical.height,
        raster.physical.height,
    );
    let right = edge(
        i64::from(area.left) + i64::from(area.width),
        raster.origin.0,
        raster.logical.width,
        raster.physical.width,
    );
    let bottom = edge(
        i64::from(area.top) + i64::from(area.height),
        raster.origin.1,
        raster.logical.height,
        raster.physical.height,
    );
    (left < right && top < bottom).then_some(Rect {
        left,
        top,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

#[test]
fn raster_integer_edges_match_wide_reference_at_clipping_boundaries() {
    for logical in [1, 2, 3, 127, 1120, i32::MAX as u32] {
        for physical in [1, 2, 3, 525, 1120, i32::MAX as u32] {
            for origin in [i32::MIN, -100, 0, 100, i32::MAX] {
                let raster = Raster::new(
                    Size {
                        width: logical,
                        height: logical,
                    },
                    Size {
                        width: physical,
                        height: physical,
                    },
                    (origin, origin),
                )
                .unwrap();
                for left in [
                    i32::MIN,
                    -1,
                    0,
                    1,
                    origin,
                    origin.saturating_add(1),
                    origin.saturating_add((logical / 2) as i32),
                    i32::MAX,
                ] {
                    for width in [0, 1, 2, logical / 2, logical, u32::MAX] {
                        let area = Rect {
                            left,
                            top: left,
                            width,
                            height: width,
                        };
                        assert_eq!(
                            raster.rect(area),
                            reference_raster_rect(raster, area),
                            "{logical}/{physical} origin={origin} area={area:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual raster throughput comparison"]
fn compare_raster_mapping_throughput() {
    use std::{hint::black_box, time::Instant};
    let rasters = [
        Raster::new(
            Size {
                width: 1120,
                height: 672,
            },
            Size {
                width: 525,
                height: 315,
            },
            (-73, 19),
        )
        .unwrap(),
        Raster::new(
            Size {
                width: 960,
                height: 544,
            },
            Size {
                width: 960,
                height: 544,
            },
            (0, 0),
        )
        .unwrap(),
    ];
    let run = |reference: bool| {
        let started = Instant::now();
        for i in 0..1_000_000 {
            let raster = black_box(rasters[i % 2]);
            let area = black_box(Rect {
                left: (i % 1400) as i32 - 100,
                top: (i % 800) as i32 - 50,
                width: (i % 500) as u32 + 1,
                height: (i % 300) as u32 + 1,
            });
            black_box(if reference {
                reference_raster_rect(raster, area)
            } else {
                raster.rect(area)
            });
        }
        started.elapsed()
    };
    for _ in 0..3 {
        println!("raster reference={:?} integer={:?}", run(true), run(false));
    }
}

#[test]
fn reduced_canvas_copy_filters_pixels_and_hidden_rgb() {
    for work_framebuffer in [false, true] {
        for edge in [4, 64] {
            let context = Context::new();
            let gpu = unsafe {
                Gpu::new(
                    context.gl(),
                    Config {
                        canvas_limit: Some(Size {
                            width: 4,
                            height: 2,
                        }),
                        work_framebuffer,
                        tile_edge: edge,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let size = Size {
                width: 8,
                height: 4,
            };
            gpu.set_canvas_size(size);
            for (colors, expected) in [
                ([[0, 0, 0, 255], [255, 255, 255, 255]], [128, 128, 128, 255]),
                ([[220, 40, 30, 255], [0, 255, 0, 0]], [220, 40, 30, 128]),
            ] {
                let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
                for (i, p) in bytes
                    .as_mut_slice()
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .enumerate()
                {
                    *p = colors[(i % size.width as usize + i / size.width as usize) % 2];
                }
                let source = gpu
                    .assign_bitmap(
                        None,
                        &Pixels {
                            size,
                            main: Some(bytes),
                            province: None,
                        },
                    )
                    .unwrap();
                let mut target = gpu.create_image(size, 0).unwrap();
                gpu.copy_rect(
                    &mut target,
                    &source,
                    size.rect(),
                    0,
                    0,
                    size.rect(),
                    DrawFace::Alpha,
                    false,
                )
                .unwrap();
                let actual = gpu.readback(&target, size.rect(), false).unwrap();
                for p in actual.data.as_slice().as_chunks::<4>().0 {
                    assert!(
                        p.iter().zip(expected).all(|(&a, b)| a.abs_diff(b) <= 1),
                        "work={work_framebuffer} edge={edge} actual={p:?} expected={expected:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn filtered_compact_canvas_uses_continuous_stored_pixels() {
    use krkr_protocol::transform::{Filter, ImageOperation, Sampling, Transform};
    for work_framebuffer in [false, true] {
        for edge in [4, 64] {
            let context = Context::new();
            let gpu = unsafe {
                Gpu::new(
                    context.gl(),
                    Config {
                        tile_edge: edge,
                        work_framebuffer,
                        canvas_limit: Some(Size {
                            width: 960,
                            height: 544,
                        }),
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let stored = Size {
                width: 16,
                height: 16,
            };
            let logical = Size {
                width: 20,
                height: 20,
            };
            let output = Size {
                width: 32,
                height: 32,
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
            let mut source = gpu.reserve_upload(stored, true, false).unwrap();
            let mut bytes = Bytes::zeroed(raw.len(), &gpu.staging).unwrap();
            bytes.as_mut_slice().copy_from_slice(&raw);
            gpu.upload(
                &mut source,
                &Pixels {
                    size: stored,
                    main: Some(bytes),
                    province: None,
                },
            )
            .unwrap();
            let physical = source.shared_main();
            source.size = logical;
            source.canvas = true;
            for points in [
                [[0.5, 0.5], [31.5, 0.5], [0.5, 31.5]],
                [[6.25, 2.75], [29.75, 9.125], [0.125, 27.75]],
            ] {
                let mut results = Vec::new();
                for input in [&physical, &source] {
                    let mut target = gpu.create_image(output, 0).unwrap();
                    gpu.transform(
                        &mut target,
                        input,
                        input.size.rect(),
                        Transform::Affine(points),
                        Sampling {
                            filter: Filter::FastLinear,
                            sharpness: -1.,
                            no_clip: false,
                        },
                        ImageOperation::Copy { hold_alpha: false },
                        output.rect(),
                        Some(0),
                    )
                    .unwrap();
                    results.push(gpu.readback(&target, output.rect(), false).unwrap());
                }
                let difference = results[0]
                    .data
                    .as_slice()
                    .iter()
                    .zip(results[1].data.as_slice())
                    .map(|(&a, &b)| a.abs_diff(b))
                    .max()
                    .unwrap();
                assert!(
                    difference <= 1,
                    "work={work_framebuffer} edge={edge} points={points:?} delta={difference}"
                );
            }
        }
    }
}

#[test]
fn compact_canvas_strip_blends_match_full_image_blend() {
    for work_framebuffer in [false, true] {
        let context = Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    canvas_limit: Some(Size {
                        width: 960,
                        height: 544,
                    }),
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(Size {
            width: 1024,
            height: 576,
        });
        let size = Size {
            width: 678,
            height: 768,
        };
        for (mode, color) in [
            (Blend::Alpha, 0xff164b5a),
            (Blend::Multiplicative, 0xff2a2144),
        ] {
            let stored = gpu.canvas_storage(size, None);
            let raw: Vec<u8> = (0..stored.width * stored.height)
                .flat_map(|i| {
                    [
                        180 + (i % 40) as u8,
                        150 + (i % 37) as u8,
                        140 + (i % 31) as u8,
                        255,
                    ]
                })
                .collect();
            let mut images = Vec::new();
            for _ in 0..2 {
                let mut image = gpu.reserve_upload(stored, true, false).unwrap();
                let mut bytes = Bytes::zeroed(raw.len(), &gpu.staging).unwrap();
                bytes.as_mut_slice().copy_from_slice(&raw);
                gpu.upload(
                    &mut image,
                    &Pixels {
                        size: stored,
                        main: Some(bytes),
                        province: None,
                    },
                )
                .unwrap();
                image.size = size;
                image.canvas = true;
                images.push(image);
            }
            let mut strips = images.pop().unwrap();
            let mut whole = images.pop().unwrap();
            let full = gpu.create_image(size, color).unwrap();
            let mut strip = gpu
                .create_image(
                    Size {
                        width: size.width,
                        height: 32,
                    },
                    0,
                )
                .unwrap();
            let source_rect = Rect {
                left: 0,
                top: 0,
                width: size.width,
                height: 8,
            };
            gpu.fill(
                &mut strip,
                &[krkr_protocol::graphics::Fill {
                    rectangle: source_rect,
                    color,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            let options = BlendOptions {
                mode,
                face: DrawFace::Opaque,
                opacity: 31,
                hold_alpha: true,
            };
            gpu.operate(&mut whole, &full, size.rect(), 0, 0, size.rect(), options)
                .unwrap();
            for y in (0..size.height).step_by(8) {
                gpu.operate(
                    &mut strips,
                    &strip,
                    source_rect,
                    0,
                    y as i32,
                    size.rect(),
                    options,
                )
                .unwrap();
            }
            let expected = gpu.readback(&whole, size.rect(), false).unwrap();
            let actual = gpu.readback(&strips, size.rect(), false).unwrap();
            let differences: Vec<_> = expected
                .data
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .zip(actual.data.as_slice().as_chunks::<4>().0.iter())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .map(|(i, (a, b))| (i % size.width as usize, i / size.width as usize, a, b))
                .take(12)
                .collect();
            assert!(
                differences.is_empty(),
                "work={work_framebuffer} mode={mode:?} differences={differences:?}"
            );
        }
    }
}

#[test]
fn filtered_fragmented_canvas_matches_contiguous_storage() {
    for (work_framebuffer, effect_sharpen) in
        [(false, false), (true, false), (false, true), (true, true)]
    {
        let context = Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer,
                    effect_sharpen,
                    tile_edge: 64,
                    canvas_limit: None,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 31,
            height: 23,
        };
        let logical = Size {
            width: 62,
            height: 46,
        };
        let output = Size {
            width: 45,
            height: 33,
        };
        let raw: Vec<u8> = (0..stored.width * stored.height)
            .flat_map(|i| {
                [
                    (i * 31) as u8,
                    (i * 47) as u8,
                    (i * 73) as u8,
                    if i < stored.width * stored.height / 2 {
                        255
                    } else {
                        (i * 11) as u8
                    },
                ]
            })
            .collect();
        let mut sources = Vec::new();
        for edge in [8, 64] {
            let mut image = gpu.reserve_upload(stored, true, false).unwrap();
            image.main = Some(gpu.plane_with_edge(stored, &gpu.resident, edge).unwrap());
            let mut bytes = Bytes::zeroed(raw.len(), &gpu.staging).unwrap();
            bytes.as_mut_slice().copy_from_slice(&raw);
            gpu.upload(
                &mut image,
                &Pixels {
                    size: stored,
                    main: Some(bytes),
                    province: None,
                },
            )
            .unwrap();
            image.size = logical;
            image.canvas = true;
            sources.push(image);
        }
        let raster = Raster::new(logical, output, (0, 0)).unwrap();
        for (raw_copy, options) in [
            (
                true,
                BlendOptions::for_composition(Blend::Opaque, DrawFace::Opaque, 255),
            ),
            (
                false,
                BlendOptions::for_composition(Blend::Alpha, DrawFace::Alpha, 137),
            ),
            (
                false,
                BlendOptions::for_composition(Blend::Opaque, DrawFace::Opaque, 255),
            ),
        ] {
            for area in [
                output.rect(),
                Rect {
                    left: 3,
                    top: 2,
                    width: 37,
                    height: 27,
                },
                Rect {
                    left: 3,
                    top: 6,
                    width: 37,
                    height: 3,
                },
            ] {
                gpu.flattened_images.borrow_mut().clear();
                let mut results = Vec::new();
                for source in &sources {
                    let mut image = gpu.create_image(output, 0x91355779).unwrap();
                    gpu.scene_bitmap(
                        &mut image,
                        source,
                        area,
                        raster,
                        (0, 0),
                        (1, -1),
                        options,
                        raw_copy,
                    )
                    .unwrap();
                    results.push(gpu.readback(&image, output.rect(), false).unwrap());
                }
                let difference = results[0]
                    .data
                    .as_slice()
                    .iter()
                    .zip(results[1].data.as_slice())
                    .map(|(&a, &b)| a.abs_diff(b))
                    .max()
                    .unwrap();
                assert!(
                    difference <= 1,
                    "work={work_framebuffer} raw={raw_copy} area={area:?} delta={difference}"
                );
            }
        }
        for source in &mut sources {
            source.size = stored;
            assert_eq!(
                gpu.readback(source, stored.rect(), false)
                    .unwrap()
                    .data
                    .as_slice(),
                raw
            );
        }
        // Reuse a gather across draws, then update one tile while the layout
        // stays unchanged. Cached borders must match a fresh contiguous source.
        let first = gpu.display_gather(&sources[0], stored.rect()).unwrap();
        let second = gpu.display_gather(&sources[0], stored.rect()).unwrap();
        assert!(std::rc::Rc::ptr_eq(
            &first.tiles[0].texture,
            &second.tiles[0].texture
        ));
        let tile = &sources[0].plane(false).unwrap().tiles[0];
        let mut changed = raw.clone();
        for y in 0..tile.rectangle.height {
            for x in 0..tile.rectangle.width {
                let i = (y * stored.width + x) as usize * 4;
                changed[i..i + 4].copy_from_slice(&[231, 17, 91, 255]);
            }
        }
        gpu.device
            .upload(
                &tile.texture,
                &[231, 17, 91, 255]
                    .repeat(tile.texture.size.width as usize * tile.texture.size.height as usize),
            )
            .unwrap();
        gpu.device
            .upload(&sources[1].plane(false).unwrap().tiles[0].texture, &changed)
            .unwrap();
        let updated = gpu.display_gather(&sources[0], stored.rect()).unwrap();
        assert!(std::rc::Rc::ptr_eq(
            &first.tiles[0].texture,
            &updated.tiles[0].texture
        ));
        for source in &mut sources {
            source.size = logical;
        }
        let mut results = Vec::new();
        for source in &sources {
            let mut image = gpu.create_image(output, 0).unwrap();
            gpu.scene_bitmap(
                &mut image,
                source,
                output.rect(),
                raster,
                (0, 0),
                (0, 0),
                BlendOptions::for_composition(Blend::Opaque, DrawFace::Opaque, 255),
                true,
            )
            .unwrap();
            results.push(gpu.readback(&image, output.rect(), false).unwrap());
        }
        assert!(
            results[0]
                .data
                .as_slice()
                .iter()
                .zip(results[1].data.as_slice())
                .all(|(&a, &b)| a.abs_diff(b) <= 1)
        );
        drop((first, second, updated));
        let before = gpu.display_gather(&sources[0], stored.rect()).unwrap();
        let weak = std::rc::Rc::downgrade(&before.tiles[0].texture);
        drop(before);
        drop(sources);
        gpu.maintain().unwrap();
        assert_eq!(
            weak.strong_count(),
            0,
            "dead sources must not pin display gathers"
        );
    }
}

#[test]
fn compact_scene_storage_matches_canvas_density_without_upscaling() {
    let context = Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(Size {
                    width: 480,
                    height: 272,
                }),
                compact_scene: true,
                small_canvas_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(Size {
        width: 1280,
        height: 720,
    });
    assert_eq!(
        gpu.scene_storage_size(
            Size {
                width: 1280,
                height: 720
            },
            Size {
                width: 960,
                height: 540
            }
        ),
        Size {
            width: 480,
            height: 270
        }
    );
    assert_eq!(
        gpu.scene_storage_size(
            Size {
                width: 1280,
                height: 720
            },
            Size {
                width: 320,
                height: 180
            }
        ),
        Size {
            width: 320,
            height: 180
        }
    );
    assert_eq!(
        gpu.scene_storage_size(
            Size {
                width: 40,
                height: 30
            },
            Size {
                width: 960,
                height: 540
            }
        ),
        Size {
            width: 40,
            height: 30
        }
    );
}

#[test]
fn sharpening_preserves_ui_alpha_and_matches_fused_composition() {
    let context = Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                effect_sharpen: true,
                canvas_limit: None,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let stored = Size {
        width: 8,
        height: 8,
    };
    let logical = Size {
        width: 16,
        height: 16,
    };
    let output = Size {
        width: 24,
        height: 24,
    };
    let raster = Raster::new(logical, output, (0, 0)).unwrap();
    let mut images = Vec::new();
    for (canvas, text, alpha) in [
        (true, false, 255),
        (false, false, 255),
        (true, true, 255),
        (true, false, 96),
        (false, false, 96),
    ] {
        let mut image = gpu.reserve_upload(stored, true, false).unwrap();
        let mut bytes = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, pixel) in bytes
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            let color = if i % 8 < 4 { 40 } else { 200 };
            pixel.copy_from_slice(&[color, color, color, alpha]);
        }
        gpu.upload(
            &mut image,
            &Pixels {
                size: stored,
                main: Some(bytes),
                province: None,
            },
        )
        .unwrap();
        image.size = logical;
        image.canvas = canvas;
        image.text = text;
        images.push(image);
    }
    // Aliased textures still need a repaint if their upsampling policy changes.
    let effect_version = crate::scene_damage::Version::capture(&images[0]).unwrap();
    let mut alias = images[0].shared_main();
    alias.text = true;
    assert!(!effect_version.matches(&alias));
    assert_eq!(
        crate::scene_damage::Version::capture(&alias)
            .unwrap()
            .damage(&effect_version),
        Some(logical.rect())
    );
    let options = BlendOptions::for_composition(Blend::Alpha, DrawFace::Alpha, 255);
    let mut pixels = Vec::new();
    for image in &images {
        let mut target = gpu.create_image(output, 0).unwrap();
        gpu.scene_bitmap(
            &mut target,
            image,
            output.rect(),
            raster,
            (0, 0),
            (0, 0),
            options,
            true,
        )
        .unwrap();
        pixels.push(gpu.readback(&target, output.rect(), false).unwrap());
    }
    assert_ne!(
        pixels[0].data.as_slice(),
        pixels[1].data.as_slice(),
        "opaque effect must actually sharpen"
    );
    assert_eq!(
        pixels[1].data.as_slice(),
        pixels[2].data.as_slice(),
        "text copies retain their sampler"
    );
    assert_eq!(
        pixels[3].data.as_slice(),
        pixels[4].data.as_slice(),
        "transparent edges retain alpha-weighted colors"
    );
    for pixel in pixels[0].data.as_slice().as_chunks::<4>().0 {
        assert!(
            (40..=200).contains(&pixel[0]),
            "no color ringing: {pixel:?}"
        );
        assert_eq!(pixel[3], 255);
    }
    let layers: Vec<_> = images[..3]
        .iter()
        .enumerate()
        .map(|(i, image)| crate::scene_batch::Layer {
            image: image.shared_main(),
            origin: (0, 0),
            opacity: [137, 113, 191][i],
            coverage: output.rect(),
        })
        .collect();
    let mut fused = gpu.create_image(output, 0x71355779).unwrap();
    let mut separate = gpu.create_image(output, 0x71355779).unwrap();
    assert!(
        gpu.scene_alpha_batch(
            &mut fused,
            &layers,
            output.rect(),
            raster,
            (0, 0),
            DrawFace::Alpha
        )
        .unwrap()
    );
    for layer in &layers {
        gpu.scene_bitmap(
            &mut separate,
            &layer.image,
            output.rect(),
            raster,
            (0, 0),
            (0, 0),
            BlendOptions::for_composition(Blend::Alpha, DrawFace::Alpha, layer.opacity),
            false,
        )
        .unwrap();
    }
    let fused = gpu.readback(&fused, output.rect(), false).unwrap();
    let separate = gpu.readback(&separate, output.rect(), false).unwrap();
    for (a, b) in fused.data.as_slice().iter().zip(separate.data.as_slice()) {
        assert!(a.abs_diff(*b) <= 1, "fused/individual delta: {a} vs {b}");
    }
}

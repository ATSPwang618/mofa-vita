#![cfg(target_os = "linux")]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{Blend, DrawFace, Fill, ImageRef, Node, Rect, Scene, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image, SceneState};
use std::{collections::HashMap, sync::Arc};

fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn cropped_opaque_snapshots_share_aligned_pixels_and_detach_on_writes() {
    let logical = Size {
        width: 1120,
        height: 672,
    };
    let stored = Size {
        width: 1050,
        height: 630,
    };
    let viewport = Size {
        width: 1024,
        height: 576,
    };
    let mut reference = Vec::new();
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    canvas_limit: Some(Size {
                        width: 960,
                        height: 544,
                    }),
                    tile_edge: 512,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(viewport);
        let data: Vec<_> = (0..stored.height)
            .flat_map(|y| {
                (0..stored.width).flat_map(move |x| {
                    [
                        (x * 13 + y * 7) as u8,
                        (x * 3 + y * 17) as u8,
                        (x ^ y) as u8,
                        (x + y * 3) as u8,
                    ]
                })
            })
            .collect();
        let mut input = gpu.reserve_upload(stored, true, false).unwrap();
        gpu.upload(
            &mut input,
            &Pixels {
                size: stored,
                main: Some(Bytes::with_permit(
                    data,
                    gpu.staging.reserve(stored.rgba_bytes().unwrap()).unwrap(),
                )),
                province: None,
            },
        )
        .unwrap();
        let source = gpu.logical_image(input, logical).unwrap();
        let original = read(&gpu, &source);
        for (index, (offset, hold_alpha, clipped)) in [
            (48, false, false),
            (47, false, false),
            (48, true, false),
            (48, false, true),
        ]
        .into_iter()
        .enumerate()
        {
            let mut snapshot = gpu.create_image(viewport, 0x71375983).unwrap();
            let clip = if clipped {
                Rect {
                    left: 7,
                    top: 11,
                    width: 997,
                    height: 541,
                }
            } else {
                viewport.rect()
            };
            let area = Rect {
                left: offset,
                top: offset,
                ..viewport.rect()
            };
            let before = gpu.resident.used();
            traffic::reset();
            gpu.operate(
                &mut snapshot,
                &source,
                area,
                0,
                0,
                clip,
                krkr_protocol::graphics::BlendOptions {
                    mode: Blend::Opaque,
                    face: DrawFace::Opaque,
                    opacity: 255,
                    hold_alpha,
                },
            )
            .unwrap();
            gpu.resolve().unwrap();
            if work && index == 0 {
                assert!(gpu.copy_is_view(&snapshot, &source, area, viewport.rect()));
                assert_eq!(traffic::draw_calls(), 0);
                assert_eq!(traffic::store_calls(), 0);
                assert_eq!(gpu.resident.used(), before);
            }
            let pixels = read(&gpu, &snapshot);
            if work {
                assert_eq!(pixels, reference[index], "snapshot case {index}");
            } else {
                reference.push(pixels);
            }
            // Writes to the source and to the snapshot must remain independent.
            let mut changed_source = source.shared();
            gpu.fill(
                &mut changed_source,
                &[Fill {
                    rectangle: area,
                    color: 0xff123456,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            assert_eq!(read(&gpu, &snapshot), reference[index]);
            gpu.fill(
                &mut snapshot,
                &[Fill {
                    rectangle: Rect {
                        left: 1,
                        top: 1,
                        width: 17,
                        height: 19,
                    },
                    color: 0xffabcdef,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            assert_eq!(read(&gpu, &source), original);
        }
    }
}

#[test]
fn local_blends_do_not_resolve_a_pending_full_screen() {
    let mut reference = None;
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 128,
            height: 96,
        };
        let mut target = gpu.create_image(size, 0x79375983).unwrap();
        traffic::reset();
        // Local overlapping portraits/text over a freshly replaced background.
        // The background's remaining dirty pixels must survive until resolve.
        for (left, top, color, opacity) in [
            (4, 6, 0x008020e0, 127),
            (8, 9, 0x00d0c050, 193),
            (100, 70, 0x0020b080, -91),
        ] {
            gpu.color(
                &mut target,
                Rect {
                    left,
                    top,
                    width: 16,
                    height: 18,
                },
                color,
                opacity,
                DrawFace::Alpha,
            )
            .unwrap();
        }
        if work {
            let copies = traffic::stored_pixels();
            assert!(
                copies <= 3 * 16 * 18,
                "local reads copied {copies} pixels of an entire {} pixel screen",
                size.width * size.height
            );
        }
        gpu.resolve().unwrap();
        let result = read(&gpu, &target);
        if let Some(expected) = &reference {
            assert_eq!(&result, expected);
        }
        reference = Some(result);
    }
}

#[test]
fn fully_covered_groups_without_visible_children_match_grouped_blends() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 19,
        height: 13,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let images = HashMap::from([(reference.id, gpu.create_image(size, 0x79375983).unwrap())]);
    for blend in (1..=28).filter_map(Blend::from_legacy) {
        for opacity in [127, 255] {
            let parent = Node {
                parent: None,
                visible: true,
                opacity,
                cache: None,
                image: Some(reference.clone()),
                neutral_color: 0,
                rectangle: size.rect(),
                image_left: 0,
                image_top: 0,
                blend,
            };
            let mut child = parent.clone();
            child.parent = Some(0);
            child.image = None;
            child.blend = Blend::Alpha;
            child.opacity = 255;
            let mut scene = Scene {
                nodes: vec![parent, child],
                ..Default::default()
            };
            // A visible empty child forces the original grouped composition.
            let grouped = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
            let expected = read(&gpu, &grouped);
            for hidden in [true, false] {
                scene.nodes[1].visible = !hidden;
                scene.nodes[1].rectangle.left = if hidden { 0 } else { size.width as i32 };
                let result = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
                assert_eq!(
                    read(&gpu, &result),
                    expected,
                    "{blend:?} opacity={opacity} hidden={hidden}"
                );
            }
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn disjoint_blends_inside_dirty_bounds_share_one_backdrop() {
    let mut reference = None;
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 64,
            height: 48,
        };
        let mut target = gpu.create_image(size, 0x80304050).unwrap();
        gpu.resolve().unwrap();
        traffic::reset();
        for (left, top) in [(1, 1), (40, 30), (20, 15), (30, 8)] {
            gpu.color(
                &mut target,
                Rect {
                    left,
                    top,
                    width: 5,
                    height: 4,
                },
                0x00e08020,
                127,
                DrawFace::Alpha,
            )
            .unwrap();
        }
        if work {
            assert_eq!(
                traffic::store_calls(),
                0,
                "gaps do not depend on earlier writes"
            );
        }
        // A real overlap must see the earlier blend, including its alpha.
        gpu.color(
            &mut target,
            Rect {
                left: 21,
                top: 16,
                width: 5,
                height: 4,
            },
            0x002080e0,
            173,
            DrawFace::Alpha,
        )
        .unwrap();
        if work {
            assert_eq!(
                traffic::store_calls(),
                1,
                "overlap must commit the backdrop"
            );
        }
        gpu.resolve().unwrap();
        if work {
            assert_eq!(traffic::store_calls(), 2);
        }
        let result = read(&gpu, &target);
        if let Some(expected) = &reference {
            assert_eq!(&result, expected);
        }
        reference = Some(result);
    }
}

#[test]
fn fragmented_blends_remain_exact_after_dirty_tracking_capacity() {
    let mut reference = None;
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 64,
            height: 48,
        };
        let mut target = gpu.create_image(size, 0x71304050).unwrap();
        gpu.resolve().unwrap();
        for i in 0..120 {
            let cell = (i * 37) % 80;
            gpu.color(
                &mut target,
                Rect {
                    left: (cell % 10) * 6,
                    top: (cell / 10) * 6,
                    width: 3,
                    height: 3,
                },
                0x00e08020,
                if i < 80 { 127 } else { -97 },
                DrawFace::Alpha,
            )
            .unwrap();
        }
        let result = read(&gpu, &target);
        if let Some(expected) = &reference {
            assert_eq!(&result, expected);
        }
        reference = Some(result);
    }
}

#[test]
fn failed_tiled_upload_preserves_shared_pixels() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 16,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 37,
        height: 29,
    };
    let mut target = gpu.create_image(size, 0x80304050).unwrap();
    let snapshot = target.shared();
    let upload = Pixels {
        size,
        main: Some(Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap()),
        province: None,
    };
    let lock = gpu.staging.reserve(gpu.staging.available()).unwrap();
    assert!(gpu.upload(&mut target, &upload).is_err());
    drop(lock);
    let expected = [0x30, 0x40, 0x50, 0x80].repeat((size.width * size.height) as usize);
    assert_eq!(read(&gpu, &target), expected);
    // Allow only the first shared tile to detach; later admission must fail
    // without publishing an uninitialized first tile into the image.
    let lock = gpu
        .resident
        .reserve(gpu.resident.available() - 16 * 16 * 4)
        .unwrap();
    assert!(gpu.upload(&mut target, &upload).is_err());
    drop(lock);
    assert_eq!(read(&gpu, &target), expected);
    gpu.upload(&mut target, &upload).unwrap();
    assert_eq!(
        read(&gpu, &target),
        upload.main.as_ref().unwrap().as_slice()
    );
    assert_eq!(read(&gpu, &snapshot), expected);
}

#[test]
fn overwrites_preserve_gaps_and_masked_channels() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 64,
        height: 48,
    };
    let mut target = gpu.create_image(size, 0x80304050).unwrap();
    let other = gpu.create_image(size, 0xffabcdef).unwrap();
    gpu.resolve().unwrap();
    traffic::reset();
    let areas = [
        Rect {
            left: 3,
            top: 5,
            width: 7,
            height: 6,
        },
        Rect {
            left: 23,
            top: 19,
            width: 5,
            height: 9,
        },
        Rect {
            left: 7,
            top: 8,
            width: 10,
            height: 7,
        },
    ];
    let mut expected = [0x30, 0x40, 0x50, 0x80].repeat((size.width * size.height) as usize);
    for (index, area) in areas.into_iter().enumerate() {
        gpu.fill(
            &mut target,
            &[Fill {
                rectangle: area,
                color: 0x91726354,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        if index == 0 {
            assert_eq!(
                traffic::loaded_pixels(),
                1,
                "overwrite must only prime PVR residency"
            );
        }
        for y in area.top..area.top + area.height as i32 {
            for x in area.left..area.left + area.width as i32 {
                let at = (y as usize * size.width as usize + x as usize) * 4;
                expected[at..at + 4].copy_from_slice(&[0x72, 0x63, 0x54, 0x91]);
            }
        }
    }
    // Reading the other image forces a target switch and stores the entire
    // dirty bounding box, including the gaps between the overwrites.
    read(&gpu, &other);
    assert_eq!(read(&gpu, &target), expected);
    read(&gpu, &other);
    gpu.fill(
        &mut target,
        &[Fill {
            rectangle: size.rect(),
            color: 0x45,
            face: DrawFace::Mask,
            hold_alpha: false,
        }],
    )
    .unwrap();
    for pixel in expected.as_chunks_mut::<4>().0.iter_mut() {
        pixel[3] = 0x45;
    }
    assert_eq!(read(&gpu, &target), expected);
}

#[test]
fn sparse_writes_store_only_changed_regions_and_keep_blend_dependencies() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 256,
        height: 192,
    };
    let mut target = gpu.create_image(size, 0x80304050).unwrap();
    let other = gpu.create_image(size, 0xff000000).unwrap();
    gpu.resolve().unwrap();
    let corners = [
        Rect {
            left: 2,
            top: 3,
            width: 8,
            height: 6,
        },
        Rect {
            left: 240,
            top: 178,
            width: 8,
            height: 6,
        },
    ];
    let mut expected = [0x30, 0x40, 0x50, 0x80].repeat((size.width * size.height) as usize);
    traffic::reset();
    for area in corners {
        gpu.fill(
            &mut target,
            &[Fill {
                rectangle: area,
                color: 0x91726354,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        for y in area.top..area.top + area.height as i32 {
            for x in area.left..area.left + area.width as i32 {
                let at = (y as usize * size.width as usize + x as usize) * 4;
                expected[at..at + 4].copy_from_slice(&[0x72, 0x63, 0x54, 0x91]);
            }
        }
    }
    gpu.resolve().unwrap();
    assert_eq!(traffic::store_calls(), 2);
    assert_eq!(traffic::stored_pixels(), 96);
    // Force a surface switch: neither the gaps nor the sparse writes may
    // depend on the previous contents of the shared renderbuffer.
    read(&gpu, &other);
    assert_eq!(read(&gpu, &target), expected);
    let source = gpu
        .create_image(
            Size {
                width: 8,
                height: 6,
            },
            0x80402010,
        )
        .unwrap();
    let mut control = gpu
        .assign_bitmap(
            None,
            &Pixels {
                size,
                main: Some({
                    let mut bytes = Bytes::zeroed(expected.len(), &gpu.staging).unwrap();
                    bytes.as_mut_slice().copy_from_slice(&expected);
                    bytes
                }),
                province: None,
            },
        )
        .unwrap();
    for area in corners {
        let options = krkr_protocol::graphics::BlendOptions::for_composition(
            Blend::Alpha,
            DrawFace::Alpha,
            137,
        );
        gpu.operate(
            &mut target,
            &source,
            source.size.rect(),
            area.left,
            area.top,
            size.rect(),
            options,
        )
        .unwrap();
        gpu.operate(
            &mut control,
            &source,
            source.size.rect(),
            area.left,
            area.top,
            size.rect(),
            options,
        )
        .unwrap();
        gpu.resolve().unwrap();
    }
    assert_eq!(read(&gpu, &target), read(&gpu, &control));
}

#[test]
fn batched_fills_match_ordered_single_fills_across_tiles_masks_and_compact_grids() {
    for work in [false, true] {
        for compact in [false, true] {
            let context = support::Context::new();
            let size = Size {
                width: 73,
                height: 51,
            };
            let gpu = unsafe {
                Gpu::new(
                    context.gl(),
                    Config {
                        work_framebuffer: work,
                        tile_edge: 16,
                        canvas_limit: compact.then_some(Size {
                            width: 37,
                            height: 26,
                        }),
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            gpu.set_canvas_size(size);
            let original = gpu.create_image(size, 0x79395783).unwrap();
            let mut batch = original.shared();
            let mut singles = original.shared();
            let fills: Vec<_> = (0..256)
                .map(|i| Fill {
                    rectangle: Rect {
                        left: i * 7 % 91 - 9,
                        top: i * 11 % 63 - 7,
                        width: 13,
                        height: 9,
                    },
                    color: 0x31407090u32.wrapping_add((i as u32).wrapping_mul(0x01030709)),
                    face: [
                        DrawFace::Alpha,
                        DrawFace::Mask,
                        DrawFace::Opaque,
                        DrawFace::AddAlpha,
                    ][i as usize % 4],
                    hold_alpha: i % 3 != 0,
                })
                .collect();
            gpu.fill(&mut batch, &fills).unwrap();
            for fill in &fills {
                gpu.fill(&mut singles, &[*fill]).unwrap();
            }
            assert_eq!(
                read(&gpu, &batch),
                read(&gpu, &singles),
                "work={work} compact={compact}"
            );
            assert_eq!(
                read(&gpu, &original),
                [0x39, 0x57, 0x83, 0x79].repeat((size.width * size.height) as usize)
            );
        }
    }
}

#[test]
fn rectangle_vertex_batches_preserve_byte_colors_masks_clips_and_overlaps() {
    for work in [false, true] {
        for compact in [false, true] {
            let context = support::Context::new();
            let size = Size {
                width: 73,
                height: 51,
            };
            let gpu = unsafe {
                Gpu::new(
                    context.gl(),
                    Config {
                        work_framebuffer: work,
                        tile_edge: 16,
                        canvas_limit: compact.then_some(Size {
                            width: 37,
                            height: 26,
                        }),
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            gpu.set_canvas_size(size);
            let original = gpu.create_image(size, 0x79395783).unwrap();
            for (face, hold_alpha) in [
                (DrawFace::Alpha, false),
                (DrawFace::Mask, false),
                (DrawFace::Opaque, true),
            ] {
                let mut batch = original.shared();
                let mut singles = original.shared();
                let fills: Vec<_> = (0..289)
                    .map(|i| Fill {
                        rectangle: Rect {
                            left: i * 7 % 91 - 9,
                            top: i * 11 % 63 - 7,
                            width: (i % 17 + 1) as u32,
                            height: (i % 11 + 1) as u32,
                        },
                        color: 0x31407090u32.wrapping_add((i as u32).wrapping_mul(0x01030709)),
                        face,
                        hold_alpha,
                    })
                    .collect();
                gpu.fill(&mut batch, &fills).unwrap();
                for fill in &fills {
                    gpu.fill(&mut singles, &[*fill]).unwrap();
                }
                assert_eq!(
                    read(&gpu, &batch),
                    read(&gpu, &singles),
                    "work={work} compact={compact} face={face:?}"
                );
                gpu.collect().unwrap();
            }
            assert_eq!(
                read(&gpu, &original),
                [0x39, 0x57, 0x83, 0x79].repeat((size.width * size.height) as usize)
            );
        }
    }
}

#[test]
fn pixel_icons_use_bounded_draw_batches_instead_of_per_pixel_clears() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 32,
            height: 32,
        };
        let mut image = gpu.create_image(size, 0).unwrap();
        let fills: Vec<_> = (0..1024)
            .map(|i| Fill {
                rectangle: Rect {
                    left: i % 32,
                    top: i / 32,
                    width: 1,
                    height: 1,
                },
                color: 0x19375779u32.wrapping_add((i as u32).wrapping_mul(0x01030709)),
                face: DrawFace::Alpha,
                hold_alpha: false,
            })
            .collect();
        gpu.resolve().unwrap();
        traffic::reset();
        gpu.fill(&mut image, &fills).unwrap();
        gpu.resolve().unwrap();
        assert_eq!(traffic::clear_calls(), 0);
        assert!(
            traffic::draw_calls() <= 16,
            "draws={}",
            traffic::draw_calls()
        );
        let expected: Vec<_> = fills
            .iter()
            .flat_map(|fill| {
                [
                    (fill.color >> 16) as u8,
                    (fill.color >> 8) as u8,
                    fill.color as u8,
                    (fill.color >> 24) as u8,
                ]
            })
            .collect();
        assert_eq!(read(&gpu, &image), expected);
    }
}

#[test]
fn local_copy_from_a_compact_strip_visits_only_intersecting_source_tiles() {
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    tile_edge: 16,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 80,
            height: 48,
        };
        let logical = Size {
            width: 160,
            height: 96,
        };
        let mut rgba = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, pixel) in rgba
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            pixel.copy_from_slice(&[(i % 80 * 3) as u8, (i / 80 * 5) as u8, (i * 17) as u8, 0x93]);
        }
        let source = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size: stored,
                    main: Some(rgba),
                    province: None,
                },
            )
            .unwrap();
        let source = gpu.logical_image(source, logical).unwrap();
        let size = Size {
            width: 8,
            height: 8,
        };
        let mut target = gpu.create_image(size, 0x71395783).unwrap();
        gpu.resolve().unwrap();
        traffic::reset();
        gpu.copy_rect(
            &mut target,
            &source,
            Rect {
                left: 20,
                top: 20,
                width: 6,
                height: 4,
            },
            1,
            2,
            size.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        gpu.resolve().unwrap();
        assert!(
            traffic::draw_calls() <= 3,
            "visited unrelated tiles: {} draws",
            traffic::draw_calls()
        );
        let result = read(&gpu, &target);
        for y in 0..8 {
            for x in 0..8 {
                let expected = if (1..7).contains(&x) && (2..6).contains(&y) {
                    let sx = (20 + x - 1) / 2;
                    let sy = (20 + y - 2) / 2;
                    [
                        (sx * 3) as u8,
                        (sy * 5) as u8,
                        ((sy * 80 + sx) * 17) as u8,
                        0x93,
                    ]
                } else {
                    [0x39, 0x57, 0x83, 0x71]
                };
                assert_eq!(
                    &result[(y * 8 + x) * 4..(y * 8 + x + 1) * 4],
                    &expected,
                    "({x}, {y}) work={work}"
                );
            }
        }
    }
}

#[test]
fn full_copies_skip_bulk_destination_loads_across_tiles_and_keep_snapshots() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 16,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 37,
        height: 29,
    };
    let source = gpu.create_image(size, 0x71395783).unwrap();
    let mut target = gpu.create_image(size, 0xa0908070).unwrap();
    gpu.resolve().unwrap();
    traffic::reset();
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
    // One residency sample per target tile, instead of loading all 1,073 pixels.
    assert!(traffic::loaded_pixels() <= 6);
    assert_eq!(
        read(&gpu, &target),
        [0x39, 0x57, 0x83, 0x71].repeat((size.width * size.height) as usize)
    );
    let snapshot = target.shared();
    // Exercise device-level COW copies followed by a masked write.
    gpu.fill(
        &mut target,
        &[Fill {
            rectangle: size.rect(),
            color: 17,
            face: DrawFace::Mask,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(read(&gpu, &snapshot), read(&gpu, &source));
    assert!(
        read(&gpu, &target)
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [0x39, 0x57, 0x83, 17])
    );
}

#[test]
fn complete_rgba_upload_needs_no_extra_staging_even_for_tall_strips() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 256,
                ..Default::default()
            },
        )
        .unwrap()
    };
    for size in [
        Size {
            width: 160,
            height: 90,
        },
        Size {
            width: 160,
            height: 600,
        },
    ] {
        let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, pixel) in bytes
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            pixel.copy_from_slice(&[i as u8, (i / 160) as u8, (i * 7) as u8, (i * 17) as u8]);
        }
        let pixels = Pixels {
            size,
            main: Some(bytes),
            province: None,
        };
        let lock = gpu.staging.reserve(gpu.staging.available()).unwrap();
        let image = gpu.assign_bitmap(None, &pixels).unwrap();
        drop(lock);
        assert_eq!(read(&gpu, &image), pixels.main.as_ref().unwrap().as_slice());
    }
}

#[test]
fn aligned_shared_copies_need_no_allocations_or_transfers() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                tile_edge: 16,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 37,
        height: 29,
    };
    let source = gpu.create_image(size, 0x71395783).unwrap();
    let mut target = source.shared();
    gpu.resolve().unwrap();
    let resident = gpu.resident.used();
    traffic::reset();
    for face in [DrawFace::Alpha, DrawFace::Mask, DrawFace::Opaque] {
        gpu.copy_rect(
            &mut target,
            &source,
            size.rect(),
            0,
            0,
            size.rect(),
            face,
            true,
        )
        .unwrap();
    }
    gpu.resolve().unwrap();
    assert_eq!(gpu.resident.used(), resident);
    assert_eq!(traffic::store_calls(), 0);
    assert_eq!(traffic::loaded_pixels(), 0);
    assert_eq!(read(&gpu, &target), read(&gpu, &source));
    // A moved copy of the same backing must still detach and perform the copy.
    gpu.copy_rect(
        &mut target,
        &source,
        size.rect(),
        1,
        0,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    assert!(gpu.resident.used() > resident);
}

#[test]
fn complete_plane_copy_shares_storage_but_preserves_independent_writes_and_province() {
    for work in [false, true] {
        for compact in [false, true] {
            let context = support::Context::new();
            let size = Size {
                width: 37,
                height: 29,
            };
            let gpu = unsafe {
                Gpu::new(
                    context.gl_with(traffic::intercept),
                    Config {
                        work_framebuffer: work,
                        tile_edge: 16,
                        canvas_limit: compact.then_some(Size {
                            width: 19,
                            height: 15,
                        }),
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            gpu.set_canvas_size(size);
            let mut source = gpu.create_image(size, 0x71395783).unwrap();
            let mut target = gpu.create_image(size, 0xa0908070).unwrap();
            gpu.fill(
                &mut target,
                &[Fill {
                    rectangle: size.rect(),
                    color: 23,
                    face: DrawFace::Province,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            let original = target.shared();
            gpu.resolve().unwrap();
            let resident = gpu.resident.used();
            traffic::reset();
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
            gpu.resolve().unwrap();
            assert_eq!(gpu.resident.used(), resident);
            assert_eq!(traffic::store_calls(), 0);
            assert_eq!(traffic::loaded_pixels(), 0);
            assert_eq!(read(&gpu, &target), read(&gpu, &source));
            assert_eq!(
                read(&gpu, &original),
                [0x90, 0x80, 0x70, 0xa0].repeat((size.width * size.height) as usize)
            );
            assert!(target.has_province());
            assert_eq!(
                gpu.readback(&target, size.rect(), true)
                    .unwrap()
                    .data
                    .as_slice(),
                vec![23; (size.width * size.height) as usize]
            );
            let before = read(&gpu, &source);
            gpu.fill(
                &mut target,
                &[Fill {
                    rectangle: Rect {
                        left: 4,
                        top: 3,
                        width: 5,
                        height: 4,
                    },
                    color: 0xffe07030,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            assert_eq!(read(&gpu, &source), before);
            let target_before = read(&gpu, &target);
            gpu.fill(
                &mut source,
                &[Fill {
                    rectangle: Rect {
                        left: 29,
                        top: 20,
                        width: 5,
                        height: 4,
                    },
                    color: 0xff2070e0,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            assert_eq!(read(&gpu, &target), target_before);
        }
    }
}

#[test]
fn varying_glyph_damage_reuses_one_patch_shape_and_stays_pixel_exact() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 320,
        height: 180,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let mut images = HashMap::from([(reference.id, gpu.create_image(size, 0xff345678).unwrap())]);
    let scene = Scene {
        nodes: vec![Node {
            parent: None,
            visible: true,
            opacity: 255,
            cache: None,
            image: Some(reference.clone()),
            neutral_color: 0,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
        }],
        ..Default::default()
    };
    let (mut canvas, mut state) = (None, SceneState::default());
    gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
        .unwrap();
    let mut warm_bytes = None;
    for i in 0..40 {
        let area = if i == 39 {
            Rect {
                left: 319,
                top: 179,
                width: 1,
                height: 1,
            }
        } else {
            Rect {
                left: i * 7,
                top: 10 + i * 3,
                width: (i % 11 + 1) as u32,
                height: (i % 9 + 1) as u32,
            }
        };
        gpu.fill(
            images.get_mut(&reference.id).unwrap(),
            &[Fill {
                rectangle: area,
                color: 0xffbcdeff,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let damage = gpu
            .update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
            .unwrap()
            .unwrap();
        assert_eq!((damage.width, damage.height), (16, 16));
        assert_eq!(damage.intersection(area), Some(area));
        gpu.maintain().unwrap();
        if let Some(bytes) = warm_bytes {
            assert_eq!(gpu.scratch.used(), bytes);
        }
        warm_bytes = Some(gpu.scratch.used());
    }
    assert_eq!(
        read(&gpu, canvas.as_ref().unwrap()),
        read(&gpu, &images[&reference.id])
    );
}

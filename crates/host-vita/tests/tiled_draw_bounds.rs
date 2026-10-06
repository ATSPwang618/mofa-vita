#![cfg(target_os = "linux")]
#![allow(unsafe_code)]
#[path = "../../render-gles2/tests/support/mod.rs"]
mod support;
#[path = "../../render-gles2/tests/support/quad_traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::Bytes,
    texture::{Compressed, Format},
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render_gles2::{Config, Gpu};
use std::sync::atomic::AtomicBool;

fn sources(gpu: &Gpu, rgba: bool) -> (krkr_render_gles2::Image, krkr_render_gles2::Image) {
    let size = Size {
        width: 128,
        height: 64,
    };
    let tile = Size {
        width: 32,
        height: 32,
    };
    sources_sized(gpu, rgba, size, tile)
}
fn sources_sized(
    gpu: &Gpu,
    rgba: bool,
    size: Size,
    tile: Size,
) -> (krkr_render_gles2::Image, krkr_render_gles2::Image) {
    let length = Compressed::payload_len(size, tile, Format::Etc1).unwrap();
    let mut bytes = Bytes::zeroed(length, &Budget::new(length)).unwrap();
    for (i, block) in bytes.as_mut_slice().chunks_exact_mut(8).enumerate() {
        block.copy_from_slice(&[
            0x11 + ((i / 64) % 8 * 0x11) as u8,
            0x34,
            0x56,
            0,
            0x93,
            0x69,
            0xa5,
            0x5a,
        ]);
    }
    let texture = Compressed::tiled(size, tile, Format::Etc1, bytes, 0).unwrap();
    let mut source = gpu.load_compressed(&texture).unwrap();
    let mut decoded =
        krkr_image::compressed::decode(&texture, &gpu.staging, &AtomicBool::new(false)).unwrap();
    if rgba {
        use krkr_protocol::graphics::{DrawFace, Fill};
        // Editing alpha expands the original tiles to RGBA without
        // changing their geometry; compare those with one ordinary RGBA tile.
        gpu.fill(
            &mut source,
            &[Fill {
                rectangle: size.rect(),
                color: 173,
                face: DrawFace::Mask,
                hold_alpha: false,
            }],
        )
        .unwrap();
        for p in decoded
            .main
            .as_mut()
            .unwrap()
            .as_mut_slice()
            .chunks_exact_mut(4)
        {
            p[3] = 173;
        }
    }
    let reference = gpu.assign_bitmap(None, &decoded).unwrap();
    (source, reference)
}

#[test]
fn tiled_draws_do_not_shade_a_full_quad_for_each_source_tile() {
    for work_framebuffer in [false, true] {
        for (size, tile, output) in [
            (
                Size {
                    width: 128,
                    height: 64,
                },
                Size {
                    width: 32,
                    height: 32,
                },
                Size {
                    width: 64,
                    height: 32,
                },
            ),
            (
                Size {
                    width: 2048,
                    height: 3072,
                },
                Size {
                    width: 1024,
                    height: 1024,
                },
                Size {
                    width: 960,
                    height: 544,
                },
            ),
        ] {
            let context = support::Context::new();
            let gpu = unsafe {
                Gpu::new(
                    context.gl_with(traffic::intercept),
                    Config {
                        tile_edge: 1024,
                        work_framebuffer,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let (source, reference) = sources_sized(&gpu, false, size, tile);
            let render = |source| {
                let mut target = gpu.create_image(output, 0x71375983).unwrap();
                gpu.resolve().unwrap();
                traffic::reset();
                gpu.transform(
                    &mut target,
                    source,
                    size.rect(),
                    Transform::Stretch(StretchRect {
                        left: 0,
                        top: 0,
                        width: output.width as i32,
                        height: output.height as i32,
                    }),
                    Sampling {
                        filter: Filter::Nearest,
                        sharpness: -1.,
                        no_clip: false,
                    },
                    ImageOperation::Copy { hold_alpha: false },
                    output.rect(),
                    None,
                )
                .unwrap();
                let submitted = traffic::pixels();
                let pixels = gpu.readback(&target, output.rect(), false).unwrap().data;
                (submitted, pixels)
            };
            let (submitted, actual) = render(&source);
            let (_, expected) = render(&reference);
            assert_eq!(actual.as_slice(), expected.as_slice());
            eprintln!(
                "{size:?} -> {output:?}: submitted={submitted} pixels, full-quads={}",
                output.rgba_bytes().unwrap() / 4
                    * (size.width / tile.width * (size.height / tile.height)) as usize
            );
            assert!(
                submitted <= output.rgba_bytes().unwrap() / 4 * 3,
                "submitted={submitted}"
            );
        }
    }
}

#[test]
fn tile_geometry_preserves_rgba_compressed_logical_flips_clips_and_affine_blends() {
    use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, Rect};
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: 256,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        for rgba in [false, true] {
            let (source, reference) = sources(&gpu, rgba);
            for logical in [
                Size {
                    width: 128,
                    height: 64,
                },
                Size {
                    width: 237,
                    height: 113,
                },
            ] {
                let source = gpu.logical_image(source.shared(), logical).unwrap();
                let reference = gpu.logical_image(reference.shared(), logical).unwrap();
                let output = Size {
                    width: 73,
                    height: 47,
                };
                let region = Rect {
                    left: 5,
                    top: 3,
                    width: logical.width - 11,
                    height: logical.height - 7,
                };
                for transform in [
                    Transform::Stretch(StretchRect {
                        left: -3,
                        top: -2,
                        width: 72,
                        height: 51,
                    }),
                    Transform::Stretch(StretchRect {
                        left: 70,
                        top: 44,
                        width: -64,
                        height: -38,
                    }),
                    Transform::Affine([[2.4, 3.7], [61.3, -5.2], [15.2, 39.8]]),
                ] {
                    for operation in [
                        ImageOperation::Copy { hold_alpha: false },
                        ImageOperation::Blend(BlendOptions::for_composition(
                            Blend::Alpha,
                            DrawFace::Alpha,
                            137,
                        )),
                        ImageOperation::Blend(BlendOptions::for_composition(
                            Blend::AddAlpha,
                            DrawFace::AddAlpha,
                            193,
                        )),
                    ] {
                        let render = |source| {
                            let mut target = gpu.create_image(output, 0x71375983).unwrap();
                            gpu.transform(
                                &mut target,
                                source,
                                region,
                                transform,
                                Sampling {
                                    filter: Filter::Nearest,
                                    sharpness: -1.,
                                    no_clip: false,
                                },
                                operation,
                                Rect {
                                    left: 7,
                                    top: 4,
                                    width: 57,
                                    height: 37,
                                },
                                None,
                            )
                            .unwrap();
                            gpu.readback(&target, output.rect(), false).unwrap().data
                        };
                        assert_eq!(
                            render(&source).as_slice(),
                            render(&reference).as_slice(),
                            "work={work_framebuffer} rgba={rgba} logical={logical:?} transform={transform:?} operation={operation:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn native_compressed_tiles_batch_without_expansion_or_readback() {
    #[allow(dead_code)]
    #[path = "../../render-gles2/tests/support/traffic.rs"]
    mod transfers;
    use krkr_protocol::graphics::{Blend, ImageRef, Node, Scene};
    use std::{collections::HashMap, sync::Arc};

    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(transfers::intercept),
                Config {
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 2048,
            height: 3072,
        };
        let tile = Size {
            width: 1024,
            height: 1024,
        };
        let (source, _) = sources_sized(&gpu, false, stored, tile);
        let canvas = Size {
            width: 1024,
            height: 576,
        };
        let screen = Size {
            width: 960,
            height: 544,
        };
        for (logical, offset) in [
            (canvas, (0, 0)),
            (
                Size {
                    width: 1774,
                    height: 3260,
                },
                (-416, -1536),
            ),
        ] {
            let source = gpu.logical_image(source.shared(), logical).unwrap();
            let mut ids = slotmap::SlotMap::with_key();
            let mut images = HashMap::new();
            let mut scene = Scene::default();
            for index in 0..8 {
                let id = ids.insert(());
                images.insert(id, source.shared());
                scene.nodes.push(Node {
                    parent: None,
                    cache: None,
                    visible: true,
                    opacity: (43 + index * 23) as u8,
                    image: Some(ImageRef {
                        id,
                        lifetime: Arc::default(),
                    }),
                    rectangle: canvas.rect(),
                    image_left: offset.0,
                    image_top: offset.1,
                    blend: Blend::Alpha,
                    neutral_color: 0,
                });
            }
            let separate = Scene {
                nodes: scene
                    .nodes
                    .iter()
                    .flat_map(|node| {
                        let mut hidden = node.clone();
                        hidden.visible = false;
                        hidden.image = None;
                        [node.clone(), hidden]
                    })
                    .collect(),
                ..Default::default()
            };
            gpu.resolve().unwrap();
            transfers::reset();
            let actual = gpu
                .scene_surface_scaled(canvas, screen, &scene, &images)
                .unwrap();
            let optimized = (
                transfers::draw_calls(),
                transfers::store_calls(),
                transfers::stored_pixels(),
            );
            assert_eq!(transfers::read_calls(), 0);
            assert_eq!(source.resident_bytes(), 3 * 1024 * 1024);
            let actual = gpu.readback(&actual, screen.rect(), false).unwrap().data;
            transfers::reset();
            let expected = gpu
                .scene_surface_scaled(canvas, screen, &separate, &images)
                .unwrap();
            let baseline = (
                transfers::draw_calls(),
                transfers::store_calls(),
                transfers::stored_pixels(),
            );
            let expected = gpu.readback(&expected, screen.rect(), false).unwrap().data;
            assert_eq!(
                actual.as_slice(),
                expected.as_slice(),
                "work={work_framebuffer} logical={logical:?}"
            );
            assert!(optimized.0 < baseline.0, "{baseline:?} -> {optimized:?}");
            assert!(optimized.2 < baseline.2, "{baseline:?} -> {optimized:?}");
            eprintln!(
                "960x544 native tiled stack work={work_framebuffer} logical={logical:?}: {baseline:?} -> {optimized:?}"
            );
        }
    }
}

#[test]
fn tiled_presentation_keeps_scissor_window_offsets_and_cursor_alpha() {
    use glow::HasContext;
    use krkr_protocol::graphics::Rect;
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                tile_edge: 256,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let (source, reference) = sources(&gpu, true);
    let physical = Size {
        width: 64,
        height: 64,
    };
    let destination = Rect {
        left: -3,
        top: 4,
        width: 80,
        height: 44,
    };
    let clip = Rect {
        left: 1,
        top: 2,
        width: 57,
        height: 60,
    };
    let gl = context.gl();
    for cursor in [false, true] {
        let render = |image| {
            gpu.clear_display(physical).unwrap();
            traffic::reset();
            if cursor {
                gpu.present_cursor(image, physical, destination).unwrap();
            } else {
                gpu.present_window(image, physical, destination, clip)
                    .unwrap();
            }
            let area = traffic::pixels();
            let mut bytes = vec![0; physical.rgba_bytes().unwrap()];
            unsafe {
                gl.read_pixels(
                    0,
                    0,
                    64,
                    64,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(&mut bytes)),
                );
            }
            (area, bytes)
        };
        let (area, actual) = render(&source);
        let (_, expected) = render(&reference);
        assert_eq!(actual, expected, "cursor={cursor}");
        assert!(
            area < destination.width as usize * destination.height as usize * 3,
            "area={area}"
        );
    }
}

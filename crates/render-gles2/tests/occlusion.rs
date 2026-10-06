#![cfg(target_os = "linux")]
mod support;
#[allow(dead_code)]
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::graphics::{Blend, ImageId, ImageRef, Node, Rect, Scene, Size};
use krkr_render_gles2::{Config, Gpu, Image};
use std::{collections::HashMap, sync::Arc};

fn render(gpu: &Gpu, scene: &Scene, images: &HashMap<ImageId, Image>) -> (Vec<u8>, usize, usize) {
    gpu.collect().unwrap();
    traffic::reset();
    let size = Size {
        width: 64,
        height: 48,
    };
    let output = gpu.scene_surface(size, scene, images, (0, 0)).unwrap();
    let draws = traffic::draw_calls();
    let stored = traffic::stored_pixels();
    let pixels = gpu
        .readback(&output, size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec();
    (pixels, draws, stored)
}

#[test]
fn opaque_children_skip_covered_work_without_changing_group_blends_or_bitmap_gaps() {
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
        width: 48,
        height: 32,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let mut images = HashMap::new();
    let mut references = Vec::new();
    for color in [0x80571993, 0x99753159, 0x41539771] {
        let id = ids.insert(());
        images.insert(id, gpu.create_image(size, color).unwrap());
        references.push(ImageRef {
            id,
            lifetime: Arc::default(),
        });
    }
    for parent_blend in [
        Blend::Opaque,
        Blend::Alpha,
        Blend::AddAlpha,
        Blend::Additive,
        Blend::Multiplicative,
    ] {
        for (opacity, offset) in [(255, 0), (193, 0), (255, 3), (255, -3)] {
            let base = Node {
                parent: None,
                cache: None,
                visible: true,
                opacity: 191,
                image: Some(references[0].clone()),
                rectangle: Rect {
                    left: 7,
                    top: 5,
                    ..size.rect()
                },
                image_left: 0,
                image_top: 0,
                blend: parent_blend,
                neutral_color: 0,
            };
            let mut nodes = vec![base.clone()];
            for _ in 0..8 {
                nodes.push(Node {
                    parent: Some(0),
                    rectangle: size.rect(),
                    blend: Blend::Alpha,
                    image: Some(references[1].clone()),
                    opacity: 173,
                    ..base.clone()
                });
            }
            let cover_index = nodes.len();
            nodes.push(Node {
                parent: Some(0),
                rectangle: size.rect(),
                blend: Blend::Opaque,
                image: Some(references[2].clone()),
                opacity,
                image_left: offset,
                ..base.clone()
            });
            nodes.push(Node {
                parent: Some(0),
                rectangle: Rect {
                    left: 9,
                    top: 8,
                    width: 13,
                    height: 7,
                },
                blend: Blend::Alpha,
                opacity: 127,
                ..base.clone()
            });
            let scene = Scene {
                nodes,
                ..Default::default()
            };
            let actual = render(&gpu, &scene, &images);

            // Two adjacent halves produce the same opaque pixels, but neither
            // alone covers the parent. This exercises the original draw order
            // as the reference, including all underlying transparent layers.
            let mut reference = Scene {
                nodes: scene.nodes.clone(),
                ..Default::default()
            };
            let mut right = reference.nodes[cover_index].clone();
            reference.nodes[cover_index].rectangle.width = size.width / 2;
            right.rectangle.left = size.width as i32 / 2;
            right.rectangle.width = size.width / 2;
            right.image_left -= size.width as i32 / 2;
            reference.nodes.insert(cover_index + 1, right);
            let expected = render(&gpu, &reference, &images);
            assert_eq!(
                actual.0, expected.0,
                "{parent_blend:?}, opacity={opacity}, offset={offset}"
            );
            if opacity == 255 && offset == 0 {
                if parent_blend == Blend::Alpha {
                    eprintln!(
                        "opaque child: draws {} -> {}, copied pixels {} -> {}",
                        expected.1, actual.1, expected.2, actual.2
                    );
                }
                let mut pruned = Scene {
                    nodes: scene.nodes.clone(),
                    ..Default::default()
                };
                for node in &mut pruned.nodes[1..cover_index] {
                    node.visible = false;
                }
                let absent = render(&gpu, &pruned, &images);
                // Covered work must cost exactly as little as explicitly
                // hidden work, even when the reference fuses its alpha stack.
                assert_eq!(actual.1, absent.1, "covered layers still drew");
                assert_eq!(actual.2, absent.2, "covered layers still copied");
                assert_eq!(actual.0, absent.0);
                assert!(actual.1 < expected.1);
                assert!(
                    actual.2 < expected.2,
                    "covered layers still copied their backdrop"
                );
            }
        }
    }
}

#[test]
fn covering_solid_root_initializes_fresh_and_reused_canvases_without_a_clear() {
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
    let mut scene = Scene {
        nodes: vec![Node {
            parent: None,
            cache: None,
            visible: true,
            opacity: 255,
            image: None,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            neutral_color: 0x123456,
        }],
        ..Default::default()
    };
    for color in [0x123456, 0x987654] {
        scene.nodes[0].neutral_color = color;
        gpu.collect().unwrap();
        traffic::reset();
        let output = gpu
            .scene_surface(size, &scene, &HashMap::new(), (0, 0))
            .unwrap();
        assert_eq!(traffic::clear_calls(), 0);
        assert!(
            gpu.readback(&output, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .chunks_exact(4)
                .all(|p| p
                    == [
                        ((color >> 16) & 255) as u8,
                        ((color >> 8) & 255) as u8,
                        (color & 255) as u8,
                        255
                    ])
        );
    }
}

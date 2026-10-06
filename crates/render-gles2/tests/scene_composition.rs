#![cfg(target_os = "linux")]
mod support;
#[allow(dead_code)]
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, DrawFace, Fill, ImageId, ImageRef, Node, Rect, Scene, Size},
    transition::{Effect, Frame, SceneTransition},
};
use krkr_render_gles2::{Config, Gpu, Image, SceneState};
use std::{collections::HashMap, sync::Arc};

fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

fn tree(gpu: &Gpu, size: Size) -> (Scene, HashMap<ImageId, Image>) {
    let mut ids = slotmap::SlotMap::with_key();
    let mut images = HashMap::new();
    let mut nodes = Vec::new();
    for i in 0..12 {
        let id = ids.insert(());
        images.insert(
            id,
            gpu.create_image(size, 0x91572931 + i * 0x04030905).unwrap(),
        );
        nodes.push(Node {
            parent: match i {
                0 => None,
                1 => Some(0),
                _ => Some(1),
            },
            cache: Some(Arc::new(())),
            visible: true,
            opacity: if i < 2 { 193 } else { 227 },
            image: Some(ImageRef {
                id,
                lifetime: Arc::default(),
            }),
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Alpha,
            neutral_color: 0,
        });
    }
    (
        Scene {
            nodes,
            ..Default::default()
        },
        images,
    )
}

#[test]
fn clipped_and_hidden_panels_do_not_disable_visible_group_caching() {
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
        width: 96,
        height: 54,
    };
    let (mut scene, images) = tree(&gpu, size);
    let panel_start = scene.nodes.len();
    for i in 0..240 {
        let mut panel = scene.nodes[2].clone();
        panel.image = None;
        panel.blend = Blend::Opaque;
        panel.opacity = if i % 3 == 2 { 0 } else { 255 };
        panel.visible = i % 3 != 1;
        panel.rectangle = Rect {
            left: 200,
            top: 0,
            width: 24,
            height: 24,
        };
        panel.neutral_color = 0x67ac23;
        scene.nodes.push(panel);
    }
    traffic::reset();
    let first = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    let cold = traffic::draw_calls();
    let original = pixels(&gpu, &first);
    drop(first);
    gpu.maintain().unwrap();
    traffic::reset();
    let second = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    let warm = traffic::draw_calls();
    assert!(
        warm * 2 < cold,
        "clipped panels disabled caching: cold={cold}, warm={warm}"
    );
    assert_eq!(pixels(&gpu, &second), original);
    drop(second);
    for change in 0..4 {
        let panel = &mut scene.nodes[panel_start];
        panel.rectangle.left = if change == 3 { 200 } else { 3 + change * 7 };
        panel.visible = change != 2;
        let output = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        let actual = pixels(&gpu, &output);
        if change < 2 {
            assert_ne!(actual, original);
        } else {
            assert_eq!(actual, original);
        }
        drop(output);
        gpu.collect().unwrap();
        let reference = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        assert_eq!(actual, pixels(&gpu, &reference), "panel change {change}");
    }
    eprintln!("clipped panels: draw calls {cold} -> {warm}");
}

#[test]
fn opaque_fullscreen_video_skips_covered_alpha_groups_but_partial_overlays_do_not() {
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
        width: 96,
        height: 54,
    };
    let (mut scene, mut images) = tree(&gpu, size);
    // Reuse an identity with a distinct source image; earlier layers still
    // exercise nested alpha composition when the overlay is translucent.
    let reference = scene.nodes.last().unwrap().image.clone().unwrap();
    images.insert(reference.id, gpu.create_image(size, 0xff123456).unwrap());
    let mut overlay = scene.nodes[0].clone();
    overlay.image = Some(reference);
    overlay.cache = None;
    overlay.blend = Blend::Opaque;
    overlay.opacity = 255;
    scene.nodes.push(overlay.clone());
    traffic::reset();
    let composed = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    let covered = traffic::draw_calls();
    let only = Scene {
        nodes: vec![overlay],
        ..Default::default()
    };
    let expected = gpu
        .scene_surface_scaled(size, size, &only, &images)
        .unwrap();
    assert_eq!(pixels(&gpu, &composed), pixels(&gpu, &expected));
    // A one-pixel uncovered edge must retain the earlier roots.
    scene.nodes.last_mut().unwrap().rectangle.width -= 1;
    gpu.collect().unwrap();
    traffic::reset();
    let partial = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    let uncovered = traffic::draw_calls();
    assert!(
        uncovered > covered * 2,
        "covered={covered} uncovered={uncovered}"
    );
    assert_ne!(pixels(&gpu, &partial), pixels(&gpu, &composed));
    scene.nodes.last_mut().unwrap().rectangle = size.rect();
    scene.nodes.last_mut().unwrap().opacity = 128;
    let faded = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    assert_ne!(pixels(&gpu, &faded), pixels(&gpu, &composed));
    for physical in [
        size,
        Size {
            width: 47,
            height: 25,
        },
    ] {
        for variant in 0..5 {
            let node = scene.nodes.last_mut().unwrap();
            node.opacity = if variant == 1 { 128 } else { 255 };
            node.blend = if variant == 2 {
                Blend::Alpha
            } else {
                Blend::Opaque
            };
            node.image_left = if variant == 3 { 1 } else { 0 };
            node.rectangle.width = size.width - u32::from(variant == 4);
            let actual = gpu
                .scene_surface_scaled(size, physical, &scene, &images)
                .unwrap();
            // An invisible child disables leaf-root culling without affecting
            // pixels, providing the original full-composition reference.
            let mut reference = Scene {
                nodes: scene.nodes.clone(),
                ..Default::default()
            };
            let mut hidden = reference.nodes.last().unwrap().clone();
            hidden.parent = Some(reference.nodes.len() - 1);
            hidden.visible = false;
            reference.nodes.push(hidden);
            let expected = gpu
                .scene_surface_scaled(size, physical, &reference, &images)
                .unwrap();
            assert_eq!(
                pixels(&gpu, &actual),
                pixels(&gpu, &expected),
                "variant={variant} physical={physical:?}"
            );
        }
    }
}

#[test]
fn changing_small_regions_in_nested_alpha_groups_avoids_full_recomposition() {
    for (logical, physical, tile_edge) in [
        (
            Size {
                width: 960,
                height: 540,
            },
            Size {
                width: 960,
                height: 540,
            },
            1024,
        ),
        (
            Size {
                width: 257,
                height: 163,
            },
            Size {
                width: 129,
                height: 83,
            },
            96,
        ),
    ] {
        let context = support::Context::new();
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
        let (mut scene, mut images) = tree(&gpu, logical);
        drop(
            gpu.scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap(),
        );
        for step in 0..8 {
            let id = scene.nodes[step + 2].image.as_ref().unwrap().id;
            // Both in-place writes and expired COW sources must preserve all
            // unchanged raster pixels and the old source snapshot.
            let old = (step % 2 == 0).then(|| images[&id].shared());
            let old_pixels = old.as_ref().map(|image| pixels(&gpu, image));
            let rectangle = Rect {
                left: (step * 17 + 9) as i32,
                top: (step * 11 + 7) as i32,
                width: 7,
                height: 9,
            };
            gpu.fill(
                images.get_mut(&id).unwrap(),
                &[Fill {
                    rectangle,
                    color: 0xc9248523 + step as u32 * 0x020413,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            if let Some(old) = old.as_ref() {
                assert_eq!(pixels(&gpu, old), old_pixels.unwrap());
            }
            drop(old);
            // Parent opacity changes invalidate the whole display, but not
            // the unchanged interior of the completed group.
            scene.nodes[0].opacity = 151 + step as u8;
            gpu.resolve().unwrap();
            traffic::reset();
            let result = gpu
                .scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap();
            let repaired_stores = traffic::stored_pixels();
            let actual = pixels(&gpu, &result);
            drop(result);
            gpu.collect().unwrap();
            traffic::reset();
            let reference = gpu
                .scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap();
            let cold_stores = traffic::stored_pixels();
            assert_eq!(
                actual,
                pixels(&gpu, &reference),
                "step {step}, tile {tile_edge}"
            );
            assert!(
                repaired_stores * 3 < cold_stores,
                "repair must avoid bulk transfers: warm={repaired_stores}, cold={cold_stores}"
            );
            if step == 0 {
                eprintln!(
                    "{physical:?}: copied bytes, cold={} repair={}",
                    cold_stores * 4,
                    repaired_stores * 4
                );
            }
            drop(reference);
            gpu.maintain().unwrap();
        }
    }
}

#[test]
fn repairing_a_crop_preserves_the_rest_of_cached_groups_and_later_full_frames() {
    for blend in [
        Blend::Alpha,
        Blend::AddAlpha,
        Blend::Multiplicative,
        Blend::Opaque,
    ] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: true,
                    tile_edge: 64,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 160,
            height: 112,
        };
        let (mut scene, mut images) = tree(&gpu, size);
        scene.nodes[1].blend = blend;
        // Group storage has a nonzero origin and the bitmap leaves a gap.
        scene.nodes[1].rectangle.left = 9;
        scene.nodes[1].rectangle.top = 5;
        scene.nodes[1].image_left = 7;
        let (mut canvas, mut state) = (None, SceneState::default());
        gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
            .unwrap();
        for step in 0..6 {
            let id = scene.nodes[3].image.as_ref().unwrap().id;
            for left in [8, 106] {
                gpu.fill(
                    images.get_mut(&id).unwrap(),
                    &[Fill {
                        rectangle: Rect {
                            left,
                            top: 28,
                            width: 3,
                            height: 4,
                        },
                        color: 0xa010a070 + step * 0x100401,
                        face: DrawFace::Alpha,
                        hold_alpha: false,
                    }],
                )
                .unwrap();
            }
            gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
                .unwrap();
            // Changing only the root's opacity makes the next frame request
            // all cached pixels, including outside the preceding repair.
            scene.nodes[0].opacity -= 7;
            if step == 2 {
                scene.nodes[2].visible = false;
            }
            if step == 3 {
                scene.nodes[1].rectangle.width -= 11;
            }
            if step == 4 {
                scene.nodes[1].image_left = -3;
            }
            gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
                .unwrap();
            let actual = pixels(&gpu, canvas.as_ref().unwrap());
            gpu.collect().unwrap();
            let reference = gpu
                .scene_surface_scaled(size, size, &scene, &images)
                .unwrap();
            assert_eq!(
                actual,
                pixels(&gpu, &reference),
                "blend {blend:?}, step {step}"
            );
        }
    }
}

#[test]
fn unrelated_transitions_keep_static_groups_cached_but_nested_effects_stay_live() {
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
        width: 160,
        height: 112,
    };
    let (mut scene, images) = tree(&gpu, size);
    let mut endpoint = scene.nodes[2].clone();
    endpoint.parent = None;
    endpoint.rectangle = Rect {
        left: 8,
        top: 6,
        width: 16,
        height: 16,
    };
    scene.nodes.push(endpoint.clone());
    endpoint.visible = false;
    endpoint.image = scene.nodes[4].image.clone();
    scene.nodes.push(endpoint);
    scene.transitions.push(SceneTransition {
        destination: 12,
        source: 13,
        with_children: false,
        frame: Frame {
            effect: Effect::CrossFade,
            face: DrawFace::Alpha,
            size,
            phase: 43,
        },
        rule: None,
        custom: None,
    });
    let mut previous = None;
    for step in 0..5 {
        scene.transitions[0].frame.phase += 37;
        if step == 2 {
            scene.transitions[0].destination = 11;
        }
        traffic::reset();
        let actual = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        let warm = traffic::stored_pixels();
        let actual = pixels(&gpu, &actual);
        if step >= 3 {
            assert!(
                previous.as_ref().unwrap() != &actual,
                "nested transition must advance"
            );
        }
        gpu.collect().unwrap();
        traffic::reset();
        let reference = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        let cold = traffic::stored_pixels();
        assert_eq!(actual, pixels(&gpu, &reference));
        if step == 1 {
            // Fused leaf stacks also reduce the uncached reference's work.
            // The cache must still avoid several full-surface transfers.
            assert!(
                cold.saturating_sub(warm) >= size.width as usize * size.height as usize * 4,
                "warm={warm} cold={cold}"
            );
        }
        previous = Some(actual);
    }
}

#[test]
fn moving_and_fading_a_descendant_repairs_old_and_new_extents() {
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
        width: 320,
        height: 180,
    };
    let (mut scene, images) = tree(&gpu, size);
    scene.nodes[11].rectangle = Rect {
        left: 7,
        top: 11,
        width: 19,
        height: 13,
    };
    drop(
        gpu.scene_surface_scaled(size, size, &scene, &images)
            .unwrap(),
    );
    for step in 0..8 {
        scene.nodes[11].rectangle.left += 13;
        scene.nodes[11].rectangle.top += 7;
        scene.nodes[11].opacity -= 19;
        traffic::reset();
        let output = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        let warm = traffic::stored_pixels();
        let actual = pixels(&gpu, &output);
        drop(output);
        gpu.collect().unwrap();
        traffic::reset();
        let reference = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        let cold = traffic::stored_pixels();
        assert!(actual == pixels(&gpu, &reference), "movement step {step}");
        assert!(
            warm * 3 < cold,
            "moving descendant warm={warm}, cold={cold}"
        );
    }
}

#[test]
fn stale_large_caches_do_not_expand_a_memory_limited_composition_band() {
    let context = support::Context::new();
    let mut gpu = unsafe {
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
        width: 160,
        height: 112,
    };
    let (scene, mut images) = tree(&gpu, size);
    drop(
        gpu.scene_surface_scaled(size, size, &scene, &images)
            .unwrap(),
    );
    gpu.maintain().unwrap();
    let id = scene.nodes[11].image.as_ref().unwrap().id;
    gpu.fill(
        images.get_mut(&id).unwrap(),
        &[Fill {
            rectangle: size.rect(),
            color: 0xc717cd82,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    gpu.resolve().unwrap();
    let previous = gpu.scratch.clone();
    // Two rows per nested group, in addition to the output image. Old cache
    // damage spans the entire canvas and must not override this admission.
    gpu.scratch = Budget::new(size.rgba_bytes().unwrap() + size.width as usize * 4 * 4);
    let result = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    let actual = pixels(&gpu, &result);
    drop(result);
    gpu.collect().unwrap();
    assert_eq!(gpu.scratch.used(), 0);
    gpu.scratch = previous;
    let reference = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    assert!(actual == pixels(&gpu, &reference));
}

#[test]
fn changing_between_leaf_and_group_repaints_neutral_pixels_outside_its_bitmap() {
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
        width: 64,
        height: 48,
    };
    for blend in (1..=28).filter_map(Blend::from_legacy) {
        let (mut scene, mut images) = tree(&gpu, size);
        scene.nodes.truncate(3);
        scene.nodes[1].blend = blend;
        scene.nodes[1].image_left = 19;
        scene.nodes[2].rectangle = Rect {
            left: 22,
            top: 17,
            width: 5,
            height: 3,
        };
        let child = scene.nodes[2].clone();
        let id = scene.nodes[1].image.as_ref().unwrap().id;
        images.insert(
            id,
            gpu.create_image(
                Size {
                    width: 23,
                    height: 19,
                },
                0x937fc561,
            )
            .unwrap(),
        );
        drop(
            gpu.scene_surface_scaled(size, size, &scene, &images)
                .unwrap(),
        );
        for present in [false, true, false] {
            scene.nodes.truncate(2);
            if present {
                scene.nodes.push(child.clone());
            }
            let result = gpu
                .scene_surface_scaled(size, size, &scene, &images)
                .unwrap();
            let actual = pixels(&gpu, &result);
            drop(result);
            gpu.collect().unwrap();
            let reference = gpu
                .scene_surface_scaled(size, size, &scene, &images)
                .unwrap();
            assert!(
                actual == pixels(&gpu, &reference),
                "blend={blend:?}, child={present}"
            );
        }
        gpu.collect().unwrap();
    }
}

#[test]
fn more_fullscreen_groups_than_cache_capacity_keep_a_reusable_subset() {
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
        width: 960,
        height: 540,
    };
    let (mut scene, images) = tree(&gpu, size);
    let prototype = scene.nodes[0].clone();
    scene.nodes.clear();
    // Five completed rasters exceed the 8 MiB cache. A stable subset should
    // survive, rather than rebuilding all five on every animation tick.
    for _ in 0..5 {
        let root = scene.nodes.len();
        let mut group = prototype.clone();
        group.cache = None;
        scene.nodes.push(group.clone());
        group.parent = Some(root);
        for _ in 0..4 {
            scene.nodes.push(group.clone());
        }
    }
    drop(
        gpu.scene_surface_scaled(size, size, &scene, &images)
            .unwrap(),
    );
    for step in 0..3 {
        for node in scene.nodes.iter_mut().filter(|n| n.parent.is_none()) {
            node.opacity -= 13;
        }
        traffic::reset();
        let result = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        let warm = traffic::stored_pixels();
        let actual = pixels(&gpu, &result);
        drop(result);
        gpu.collect().unwrap();
        traffic::reset();
        let reference = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        let cold = traffic::stored_pixels();
        assert!(actual == pixels(&gpu, &reference), "step {step}");
        assert!(
            warm * 3 < cold * 2,
            "oversubscribed cache: warm={warm}, cold={cold}"
        );
    }
}

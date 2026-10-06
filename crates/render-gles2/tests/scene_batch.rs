#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[allow(dead_code)]
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{Blend, ImageId, ImageRef, Node, Rect, Scene, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::{collections::HashMap, sync::Arc};

fn tree(
    gpu: &Gpu,
    logical: Size,
    stored: Size,
    count: usize,
    blend: Blend,
) -> (Scene, HashMap<ImageId, Image>) {
    let mut ids = slotmap::SlotMap::with_key();
    let mut images = HashMap::new();
    let mut nodes = Vec::new();
    for index in 0..=count {
        let id = ids.insert(());
        let mut bytes = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, pixel) in bytes
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            pixel.copy_from_slice(&[
                (i * 17 + index * 21) as u8,
                (i * 29 + index * 91) as u8,
                (i * 11 + index * 37) as u8,
                (i * 31 + index * 43) as u8,
            ]);
        }
        let image = gpu
            .assign_bitmap(
                None,
                &Pixels {
                    size: stored,
                    main: Some(bytes),
                    province: None,
                },
            )
            .unwrap();
        images.insert(id, gpu.logical_image(image, logical).unwrap());
        nodes.push(Node {
            parent: (index != 0).then_some(0),
            cache: None,
            visible: true,
            opacity: if index == 0 {
                255
            } else {
                [1, 127, 255, 193][index % 4]
            },
            image: Some(ImageRef {
                id,
                lifetime: Arc::default(),
            }),
            rectangle: logical.rect(),
            image_left: 0,
            image_top: 0,
            blend: if index == 0 { blend } else { Blend::Alpha },
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
fn separate(scene: &Scene) -> Scene {
    let mut nodes = Vec::new();
    // Invisible siblings split every consecutive run without changing whether
    // a leaf has children (which would create a group for uncovered bitmaps).
    for original in &scene.nodes {
        let mut node = original.clone();
        node.parent = node.parent.map(|i| i * 2);
        let mut hidden = node.clone();
        hidden.visible = false;
        hidden.image = None;
        nodes.push(node);
        nodes.push(hidden);
    }
    Scene {
        nodes,
        ..Default::default()
    }
}
fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn assert_pixels(actual: &[u8], expected: &[u8], context: &str) {
    assert_eq!(actual.len(), expected.len(), "{context}");
    let differences: Vec<_> = actual
        .iter()
        .zip(expected)
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(12)
        .collect();
    assert!(differences.is_empty(), "{context}: {differences:?}");
}

fn assert_batch_traffic(
    work_framebuffer: bool,
    face: Blend,
    baseline: (usize, usize, usize),
    optimized: (usize, usize, usize),
) {
    if work_framebuffer && face == Blend::Opaque {
        // Display composition retains fixed-blend rounding across incremental
        // updates. It deliberately draws opaque groups without shader batches.
        assert_eq!(optimized, baseline, "opaque display fallback traffic");
    } else {
        assert!(
            optimized.0 < baseline.0 && optimized.2 < baseline.2,
            "batch draws/transfers: {baseline:?} -> {optimized:?}"
        );
    }
}

#[test]
fn alpha_stacks_preserve_each_parent_face_opacity_and_compact_rounding() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let logical = Size {
            width: 137,
            height: 83,
        };
        let stored = Size {
            width: 59,
            height: 37,
        };
        let physical = Size {
            width: 61,
            height: 41,
        };
        for face in [Blend::Opaque, Blend::Alpha, Blend::AddAlpha] {
            for count in [2, 3, 4, 7] {
                let (mut scene, images) = tree(&gpu, logical, stored, count, face);
                // All siblings share coverage while their sampling origins and
                // logical image extents differ from the physical raster grid.
                for child in &mut scene.nodes[1..] {
                    child.rectangle = Rect {
                        left: 7,
                        top: 5,
                        width: 113,
                        height: 69,
                    };
                    child.image_left = -3;
                    child.image_top = -2;
                }
                let batched = gpu
                    .scene_surface_scaled(logical, physical, &scene, &images)
                    .unwrap();
                let reference = gpu
                    .scene_surface_scaled(logical, physical, &separate(&scene), &images)
                    .unwrap();
                assert_eq!(
                    pixels(&gpu, &batched),
                    pixels(&gpu, &reference),
                    "{work_framebuffer} {face:?} {count}"
                );
            }
        }
    }
}

#[test]
fn overlapping_fullscreen_layers_reduce_draws_and_backdrop_transfers() {
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
    let (scene, images) = tree(&gpu, size, size, 8, Blend::Opaque);
    traffic::reset();
    let batched = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
    let optimized = (
        traffic::draw_calls(),
        traffic::store_calls(),
        traffic::stored_pixels(),
    );
    let actual = pixels(&gpu, &batched);
    traffic::reset();
    let reference = gpu
        .scene_surface(size, &separate(&scene), &images, (0, 0))
        .unwrap();
    let baseline = (
        traffic::draw_calls(),
        traffic::store_calls(),
        traffic::stored_pixels(),
    );
    assert_eq!(actual, pixels(&gpu, &reference));
    assert!(
        optimized.0 * 2 <= baseline.0,
        "draws {baseline:?} -> {optimized:?}"
    );
    assert!(
        optimized.2 * 2 <= baseline.2,
        "transfers {baseline:?} -> {optimized:?}"
    );
    eprintln!("8 alpha layers (draws, copies, copied pixels): {baseline:?} -> {optimized:?}");
}

#[test]
fn clipped_layers_and_non_alpha_layers_preserve_order() {
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
        width: 53,
        height: 31,
    };
    let (mut scene, images) = tree(&gpu, size, size, 10, Blend::Opaque);
    scene.nodes[3].blend = Blend::PsScreen;
    scene.nodes[6].rectangle.left = 5;
    scene.nodes[7].visible = false;
    let batched = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
    let reference = gpu
        .scene_surface(size, &separate(&scene), &images, (0, 0))
        .unwrap();
    assert_eq!(pixels(&gpu, &batched), pixels(&gpu, &reference));
}

#[test]
fn offset_clipped_stacks_match_unbatched_pixels_and_reduce_transfers() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer,
                    tile_edge: 64,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let logical = Size {
            width: 137,
            height: 83,
        };
        let stored = Size {
            width: 59,
            height: 37,
        };
        let physical = Size {
            width: 121,
            height: 71,
        };
        for face in [Blend::Opaque, Blend::Alpha, Blend::AddAlpha] {
            let (mut scene, images) = tree(&gpu, logical, stored, 8, face);
            for (i, node) in scene.nodes[1..].iter_mut().enumerate() {
                node.rectangle = Rect {
                    left: 4 + i as i32,
                    top: 3 + (i % 3) as i32,
                    width: 119 - (i % 4) as u32,
                    height: 69 - (i % 3) as u32,
                };
                node.image_left = -((i % 3) as i32);
                node.image_top = (i % 2) as i32;
            }
            traffic::reset();
            let actual = gpu
                .scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap();
            let optimized = (
                traffic::draw_calls(),
                traffic::store_calls(),
                traffic::stored_pixels(),
            );
            let actual = pixels(&gpu, &actual);
            // The reference must rebuild, not reuse the optimized subtree's
            // completed raster. Identical new bitmaps invalidate weak versions.
            let (_, images) = tree(&gpu, logical, stored, 8, face);
            traffic::reset();
            let expected = gpu
                .scene_surface_scaled(logical, physical, &separate(&scene), &images)
                .unwrap();
            let baseline = (
                traffic::draw_calls(),
                traffic::store_calls(),
                traffic::stored_pixels(),
            );
            let expected = pixels(&gpu, &expected);
            let difference: Vec<_> = actual
                .iter()
                .zip(&expected)
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(12)
                .collect();
            assert!(
                difference.is_empty(),
                "{work_framebuffer} {face:?}: {difference:?}"
            );
            assert_batch_traffic(work_framebuffer, face, baseline, optimized);
            eprintln!(
                "offset alpha stack work={work_framebuffer} {face:?}: {baseline:?} -> {optimized:?}"
            );
        }
    }
}

#[test]
fn distant_leaves_do_not_create_a_larger_sampling_pass() {
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
    let (mut scene, images) = tree(&gpu, size, size, 4, Blend::Opaque);
    for (i, node) in scene.nodes[1..].iter_mut().enumerate() {
        node.rectangle = Rect {
            left: i as i32 * 24,
            top: 3,
            width: 8,
            height: 20,
        };
    }
    traffic::reset();
    let actual = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
    let counts = (
        traffic::draw_calls(),
        traffic::store_calls(),
        traffic::stored_pixels(),
    );
    let actual = pixels(&gpu, &actual);
    traffic::reset();
    let expected = gpu
        .scene_surface(size, &separate(&scene), &images, (0, 0))
        .unwrap();
    assert_eq!(
        counts,
        (
            traffic::draw_calls(),
            traffic::store_calls(),
            traffic::stored_pixels()
        )
    );
    assert_eq!(actual, pixels(&gpu, &expected));
}

#[test]
fn root_batches_preserve_tiled_output_and_partial_scene_updates() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: 64,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let logical = Size {
            width: 137,
            height: 83,
        };
        let physical = Size {
            width: 131,
            height: 79,
        };
        let stored = Size {
            width: 59,
            height: 37,
        };
        let (mut scene, images) = tree(&gpu, logical, stored, 6, Blend::Alpha);
        for node in &mut scene.nodes {
            node.parent = None;
            node.rectangle = Rect {
                left: 9,
                top: 3,
                width: 119,
                height: 73,
            };
        }
        let mut state = krkr_render_gles2::SceneState::default();
        let mut canvas = None;
        // Compare the same repaint regions: changing the local raster origin
        // can move an f32 interpolation across a byte-rounding boundary even
        // without batching. The batched and individual paths must match exactly.
        let mut reference_state = krkr_render_gles2::SceneState::default();
        let mut reference_canvas = None;
        for step in 0..3 {
            if step == 1 {
                scene.nodes[3].opacity = 29;
            } else if step == 2 {
                scene.nodes[3].rectangle.left += 7;
            }
            let damage = gpu
                .update_scene_surface(&mut canvas, &mut state, logical, physical, &scene, &images)
                .unwrap();
            assert!(damage.is_some());
            let mut actual = canvas.as_ref().unwrap().shared();
            actual.size = physical;
            gpu.update_scene_surface(
                &mut reference_canvas,
                &mut reference_state,
                logical,
                physical,
                &separate(&scene),
                &images,
            )
            .unwrap();
            let mut reference = reference_canvas.as_ref().unwrap().shared();
            reference.size = physical;
            assert_pixels(
                &pixels(&gpu, &actual),
                &pixels(&gpu, &reference),
                &format!("work={work_framebuffer} update={step} damage={damage:?}"),
            );
        }
    }
}

#[test]
fn tiled_alpha_stacks_preserve_pixels_and_reduce_backdrop_traffic() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: 64,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 128,
            height: 96,
        };
        // Exercise the nearest tile partitioner. Filtered tile boundaries use
        // the individual seam-gathering sampler (covered by the next test).
        let logical = stored;
        let physical = stored;
        for face in [Blend::Opaque, Blend::Alpha, Blend::AddAlpha] {
            let (scene, images) = tree(&gpu, logical, stored, 8, face);
            traffic::reset();
            let actual = gpu
                .scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap();
            let optimized = (
                traffic::draw_calls(),
                traffic::store_calls(),
                traffic::stored_pixels(),
            );
            let actual = pixels(&gpu, &actual);
            let (_, images) = tree(&gpu, logical, stored, 8, face);
            traffic::reset();
            let expected = gpu
                .scene_surface_scaled(logical, physical, &separate(&scene), &images)
                .unwrap();
            let baseline = (
                traffic::draw_calls(),
                traffic::store_calls(),
                traffic::stored_pixels(),
            );
            assert_pixels(
                &actual,
                &pixels(&gpu, &expected),
                &format!("{work_framebuffer} {face:?}"),
            );
            assert_batch_traffic(work_framebuffer, face, baseline, optimized);
            eprintln!(
                "tiled alpha stack work={work_framebuffer} {face:?}: {baseline:?} -> {optimized:?}"
            );
        }
    }
}

#[test]
fn tiled_batch_offsets_clips_mixed_grids_and_partial_updates_match_individual_draws() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    tile_edge: 32,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let logical = Size {
            width: 137,
            height: 83,
        };
        let physical = Size {
            width: 109,
            height: 73,
        };
        let stored = Size {
            width: 64,
            height: 96,
        };
        for face in [Blend::Opaque, Blend::Alpha, Blend::AddAlpha] {
            let (mut scene, mut images) = tree(&gpu, logical, stored, 7, face);
            scene.nodes[0].rectangle = Rect {
                left: 7,
                top: 5,
                width: logical.width - 11,
                height: logical.height - 9,
            };
            // Mix single- and multi-tile images without changing tree order.
            let (_, small) = tree(
                &gpu,
                logical,
                Size {
                    width: 29,
                    height: 23,
                },
                7,
                face,
            );
            let id = scene.nodes[2].image.as_ref().unwrap().id;
            images.insert(id, small[&id].shared());
            let mut state = krkr_render_gles2::SceneState::default();
            let mut canvas = None;
            let mut reference_state = krkr_render_gles2::SceneState::default();
            let mut reference_canvas = None;
            for step in 0..8 {
                for (i, node) in scene.nodes.iter_mut().enumerate().skip(1) {
                    node.rectangle.left = ((step + i) % 3) as i32 - 1;
                    node.rectangle.top = ((step * 2 + i) % 5) as i32 - 2;
                    node.rectangle.width = logical.width - step as u32;
                    node.image_left = -(((step + i) % 4) as i32);
                    node.image_top = ((step + i) % 2) as i32;
                    node.opacity = (step * 23 + i * 37) as u8;
                }
                gpu.update_scene_surface(
                    &mut canvas,
                    &mut state,
                    logical,
                    physical,
                    &scene,
                    &images,
                )
                .unwrap();
                let mut actual = canvas.as_ref().unwrap().shared();
                actual.size = physical;
                gpu.update_scene_surface(
                    &mut reference_canvas,
                    &mut reference_state,
                    logical,
                    physical,
                    &separate(&scene),
                    &images,
                )
                .unwrap();
                let mut expected = reference_canvas.as_ref().unwrap().shared();
                expected.size = physical;
                assert_pixels(
                    &pixels(&gpu, &actual),
                    &pixels(&gpu, &expected),
                    &format!("work={work_framebuffer} {face:?} step={step}"),
                );
            }
        }
    }
}
#[test]
fn menu_button_strips_batch_without_changing_fade_or_sprite_frames() {
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
    let logical = Size {
        width: 1024,
        height: 576,
    };
    let physical = Size {
        width: 960,
        height: 540,
    };
    let button = Size {
        width: 465,
        height: 29,
    };
    for face in [Blend::Opaque, Blend::Alpha, Blend::AddAlpha] {
        let (mut scene, images) = tree(&gpu, button, button, 10, face);
        scene.nodes[0].rectangle = logical.rect();
        scene.nodes[0].image = None;
        scene.nodes[0].neutral_color = 0xff132947;
        for (i, node) in scene.nodes.iter_mut().enumerate().skip(1) {
            node.rectangle = Rect {
                left: 870,
                top: 82 + (i as i32 - 1) * 32,
                width: 93,
                height: 29,
            };
        }
        for step in 0..4 {
            for (i, node) in scene.nodes.iter_mut().enumerate().skip(1) {
                node.image_left = -93 * ((i + step) % 5) as i32;
                node.opacity = [64, 128, 192, 255][step];
            }
            gpu.flush().unwrap();
            traffic::reset();
            let batched = gpu
                .scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap();
            gpu.flush().unwrap();
            let after = (
                traffic::draw_calls(),
                traffic::load_calls(),
                traffic::store_calls(),
            );
            let actual = pixels(&gpu, &batched);
            // Invalidate the completed group so the reference measures drawing
            // every button, rather than reusing the optimized group's raster.
            let (_, reference_images) = tree(&gpu, button, button, 10, face);
            traffic::reset();
            let individual = gpu
                .scene_surface_scaled(logical, physical, &separate(&scene), &reference_images)
                .unwrap();
            gpu.flush().unwrap();
            let before = (
                traffic::draw_calls(),
                traffic::load_calls(),
                traffic::store_calls(),
            );
            assert_pixels(
                &actual,
                &pixels(&gpu, &individual),
                &format!("menu fade {step} {face:?}"),
            );
            if face == Blend::Opaque {
                assert_eq!(after, before, "opaque menu fallback traffic");
            } else {
                // Disjoint strips already share work-surface transfers. Batch
                // them with fewer draws and without adding loads or stores.
                assert!(
                    after.0 < before.0 && after.1 <= before.1 && after.2 <= before.2,
                    "menu draws/loads/stores: {before:?} -> {after:?}"
                );
            }
            eprintln!("menu fade {step} {face:?} draws/loads/stores: {before:?} -> {after:?}");
        }
    }
}

#[test]
fn filtered_tiled_alpha_stacks_preserve_pixels_and_reduce_backdrop_traffic() {
    for work_framebuffer in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: 128,
                    work_framebuffer,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let stored = Size {
            width: 256,
            height: 128,
        };
        // A downscaled tiled stack must retain interpolation across seams.
        let logical = stored;
        let physical = Size {
            width: 239,
            height: 119,
        };
        for face in [Blend::Opaque, Blend::Alpha, Blend::AddAlpha] {
            let (scene, images) = tree(&gpu, logical, stored, 8, face);
            traffic::reset();
            let actual = gpu
                .scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap();
            let optimized = (
                traffic::draw_calls(),
                traffic::store_calls(),
                traffic::stored_pixels(),
            );
            let actual = pixels(&gpu, &actual);
            let (_, images) = tree(&gpu, logical, stored, 8, face);
            traffic::reset();
            let expected = gpu
                .scene_surface_scaled(logical, physical, &separate(&scene), &images)
                .unwrap();
            let baseline = (
                traffic::draw_calls(),
                traffic::store_calls(),
                traffic::stored_pixels(),
            );
            assert_pixels(
                &actual,
                &pixels(&gpu, &expected),
                &format!("{work_framebuffer} {face:?}"),
            );
            assert_batch_traffic(work_framebuffer, face, baseline, optimized);
            eprintln!(
                "tiled alpha stack work={work_framebuffer} {face:?}: {baseline:?} -> {optimized:?}"
            );
        }
    }
}

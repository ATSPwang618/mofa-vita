#![cfg(target_os = "linux")]
mod support;

#[test]
fn work_textures_recycle_without_host_upload_and_preserve_live_versions() {
    use krkr_protocol::budget::Budget;
    let context = support::Context::new();
    let staging = Budget::new(1024 * 1024);
    let gpu = unsafe {
        krkr_render_gles2::Gpu::new(
            context.gl(),
            krkr_render_gles2::Config {
                work_framebuffer: true,
                tile_edge: 32,
                staging: staging.clone(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = krkr_protocol::graphics::Size {
        width: 32,
        height: 16,
    };
    let old = gpu.create_surface_image(size).unwrap();
    let retained = old.shared();
    drop(old);
    gpu.maintain().unwrap();
    let lock = staging.reserve(staging.available()).unwrap();
    // The live snapshot must not be recycled; a fresh allocation needs staging.
    assert!(gpu.create_surface_image(size).is_err());
    drop(retained);
    gpu.maintain().unwrap();
    let mut reused = gpu.create_surface_image(size).unwrap();
    drop(lock);
    assert!(
        gpu.readback(&reused, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .iter()
            .all(|&v| v == 0)
    );
    gpu.fill(
        &mut reused,
        &[krkr_protocol::graphics::Fill {
            rectangle: size.rect(),
            color: 0xff123456,
            face: krkr_protocol::graphics::DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    gpu.flush().unwrap();
    drop(reused);
    gpu.maintain().unwrap();
    let reused = gpu.create_surface_image(size).unwrap();
    assert!(
        gpu.readback(&reused, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .iter()
            .all(|&v| v == 0)
    );
    drop(reused);
    gpu.collect().unwrap();
    assert_eq!(gpu.scratch.used(), 32 * 32 * 4); // only the persistent work surface
}
use krkr_protocol::graphics::{Blend, ImageRef, Node, Scene, Size};
use krkr_render_gles2::{Config, Gpu, Image};
use std::{collections::HashMap, sync::Arc};
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn node(image: &ImageRef, size: Size, parent: Option<usize>, blend: Blend) -> Node {
    Node {
        cache: None,
        visible: true,
        parent,
        image: Some(image.clone()),
        neutral_color: blend.neutral(),
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend,
        opacity: 255,
    }
}

#[test]
fn growing_sibling_groups_fit_the_admitted_scratch_depth() {
    use krkr_protocol::budget::Budget;
    let context = support::Context::new();
    let size = Size {
        width: 32,
        height: 16,
    };
    let mut scene = Scene::default();
    let solid = |size: Size, parent, blend, opacity, color| Node {
        cache: None,
        visible: true,
        parent,
        image: None,
        neutral_color: color,
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend,
        opacity,
    };
    scene
        .nodes
        .push(solid(size, None, Blend::Opaque, 255, 0x203040));
    for width in (4..=32).step_by(4) {
        let part = Size { width, ..size };
        let parent = scene.nodes.len();
        scene.nodes.push(solid(part, Some(0), Blend::Alpha, 117, 0));
        scene.nodes.push(solid(
            part,
            Some(parent),
            Blend::Opaque,
            255,
            0xb02030 + width,
        ));
    }
    let mut expected = None;
    for limit in [1024 * 1024, 32 * 32 * 4 + size.rgba_bytes().unwrap() * 2] {
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: true,
                    tile_edge: 32,
                    resident: Budget::new(1024 * 1024),
                    scratch: Budget::new(limit),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let _lock = gpu.resident.reserve(gpu.resident.available()).unwrap();
        let output = gpu
            .scene_surface(size, &scene, &HashMap::new(), (0, 0))
            .unwrap();
        let actual = read(&gpu, &output);
        if let Some(expected) = &expected {
            assert_eq!(&actual, expected);
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn subtree_cache_uses_weak_versions_and_invalidates_in_place_writes_and_geometry() {
    use krkr_protocol::graphics::{DrawFace, Fill, Rect};
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 32,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 16,
        height: 12,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let image = gpu.create_image(size, 0x80904020).unwrap();
    let mut images = HashMap::from([(reference.id, image)]);
    let mut group = node(&reference, size, None, Blend::Alpha);
    group.image = None;
    let child = node(&reference, size, Some(0), Blend::Alpha);
    let mut scene = Scene {
        nodes: vec![group, child],
        ..Default::default()
    };
    let initial = gpu.resident.used();
    let first = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    let before = read(&gpu, &first);
    let cached = gpu.resident.used();
    assert!(cached > initial); // automatic private group cache, even without a token
    let second = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    assert_eq!(read(&gpu, &second), before);
    assert_eq!(gpu.resident.used(), cached);
    drop(first);
    drop(second);
    gpu.maintain().unwrap();
    let used = gpu.resident.used();
    gpu.fill(
        images.get_mut(&reference.id).unwrap(),
        &[Fill {
            rectangle: size.rect(),
            color: 0xc0206090,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(gpu.resident.used(), used); // cache must not cause a source COW
    let changed = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    assert_ne!(read(&gpu, &changed), before);
    scene.nodes[1].rectangle = Rect {
        left: 3,
        top: 2,
        width: 8,
        height: 6,
    };
    scene.nodes[0].opacity = 117;
    let cropped = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    let expected = read(&gpu, &cropped);
    drop(cropped);
    gpu.collect().unwrap(); // recomposition without the retained result is the reference
    let uncached = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    assert_eq!(read(&gpu, &uncached), expected);
}

#[test]
fn durable_scene_outputs_keep_their_resident_budget_on_recomposition() {
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Default::default()).unwrap() };
    let size = Size {
        width: 32,
        height: 16,
    };
    let scene = Scene {
        nodes: vec![Node {
            cache: None,
            visible: true,
            parent: None,
            image: None,
            neutral_color: 0x123456,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        }],
        ..Default::default()
    };
    let resident = gpu.resident.used();
    let mut image = gpu
        .scene_image(size, &scene, &HashMap::new(), (0, 0))
        .unwrap();
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), resident + size.rgba_bytes().unwrap());
    assert_eq!(gpu.scratch.used(), 0);
    gpu.compose(&mut image, &scene, &HashMap::new()).unwrap();
    gpu.collect().unwrap();
    assert_eq!(gpu.resident.used(), resident + size.rgba_bytes().unwrap());
    assert_eq!(gpu.scratch.used(), 0);
    let surface = gpu
        .scene_surface(size, &scene, &HashMap::new(), (0, 0))
        .unwrap();
    assert_eq!(read(&gpu, &surface), read(&gpu, &image));
    gpu.collect().unwrap();
    assert_eq!(gpu.scratch.used(), size.rgba_bytes().unwrap());
}

#[test]
fn compact_captures_and_piled_copies_fit_the_original_vita_scratch_budget() {
    use krkr_protocol::{
        budget::Budget,
        graphics::{DrawFace, Rect},
    };
    let context = support::Context::new();
    let logical = Size {
        width: 1920,
        height: 1080,
    };
    let stored = Size {
        width: 960,
        height: 540,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                scratch: Budget::new(16 * 1024 * 1024),
                ..Config::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(logical);
    let root = Node {
        cache: None,
        visible: true,
        parent: None,
        image: None,
        neutral_color: 0x112233,
        rectangle: logical.rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    };
    let child = Node {
        parent: Some(0),
        neutral_color: 0xaabbcc,
        rectangle: Rect {
            left: 32,
            top: 24,
            width: 159,
            height: 127,
        },
        ..root.clone()
    };
    let mut scene = Scene {
        nodes: vec![root, child],
        ..Default::default()
    };
    let images = HashMap::new();
    // Retain the previously displayed canvas, as the Vita window does.
    let published = gpu
        .scene_surface_scaled(logical, stored, &scene, &images)
        .unwrap();
    let mut target = gpu.create_image(logical, 0).unwrap();
    for _ in 0..4 {
        let capture = gpu.scene_surface(logical, &scene, &images, (0, 0)).unwrap();
        assert_eq!(capture.size, logical);
        assert_eq!(capture.stored_size(), Some(stored));
        gpu.copy_rect(
            &mut target,
            &capture,
            logical.rect(),
            0,
            0,
            logical.rect(),
            DrawFace::Alpha,
            false,
        )
        .unwrap();
        assert_eq!(target.stored_size(), Some(stored));
        drop(capture);
        gpu.collect().unwrap();
        assert_eq!(
            gpu.scratch.used(),
            1024 * 1024 * 4 + stored.rgba_bytes().unwrap()
        );
    }
    // A nonzero, odd crop origin must still refer to script coordinates.
    let crop = Size {
        width: 1000,
        height: 600,
    };
    let capture = gpu.scene_surface(crop, &scene, &images, (31, 23)).unwrap();
    assert_eq!(
        capture.stored_size(),
        Some(Size {
            width: 500,
            height: 300
        })
    );
    let got = read(&gpu, &capture);
    for (i, pixel) in got.as_chunks::<4>().0.iter().enumerate() {
        let x = 31 + (i % 1000 / 2) * 2 + 1;
        let y = 23 + (i / 1000 / 2) * 2 + 1;
        let expected = if (32..191).contains(&x) && (24..151).contains(&y) {
            [0xaa, 0xbb, 0xcc, 255]
        } else {
            [0x11, 0x22, 0x33, 255]
        };
        assert_eq!(*pixel, expected, "crop pixel {i}");
    }
    drop(capture);
    // Durable composition and later recomposition must keep the same density.
    let mut durable = gpu.scene_image(logical, &scene, &images, (0, 0)).unwrap();
    assert_eq!(durable.stored_size(), Some(stored));
    gpu.compose(&mut durable, &scene, &images).unwrap();
    assert_eq!(durable.stored_size(), Some(stored));
    // Invalid scene input preserves the last successfully published pixels.
    let before = read(&gpu, &durable);
    scene.nodes[1].parent = Some(1);
    assert!(gpu.compose(&mut durable, &scene, &images).is_err());
    assert_eq!(read(&gpu, &durable), before);
    drop(published);
}

#[test]
fn imageless_opaque_layers_use_neutral_color_while_alpha_containers_stay_transparent() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 2,
        height: 1,
    };
    let root = Node {
        cache: None,
        visible: true,
        parent: None,
        image: None,
        neutral_color: 0x112233,
        rectangle: size.rect(),
        image_left: 100,
        image_top: 100,
        blend: Blend::Opaque,
        opacity: 255,
    };
    let mut child = root.clone();
    child.parent = Some(0);
    child.rectangle.left = 1;
    child.neutral_color = 0x445566;
    let mut container = root.clone();
    container.parent = Some(0);
    container.blend = Blend::Alpha;
    container.neutral_color = 0xffff0000;
    let scene = Scene {
        nodes: vec![root, child, container],
        ..Default::default()
    };
    let mut output = gpu.create_surface_image(size).unwrap();
    gpu.compose(&mut output, &scene, &HashMap::new()).unwrap();
    assert_eq!(read(&gpu, &output), [17, 34, 51, 255, 68, 85, 102, 255]);
}

#[test]

fn advanced_group_blends_the_completed_subtree_once() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 2,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 2,
        height: 1,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let mut images = HashMap::new();
    let mut image = |color| {
        let id = ids.insert(());
        images.insert(id, gpu.create_image(size, color).unwrap());
        ImageRef {
            id,
            lifetime: Arc::default(),
        }
    };
    let base = image(0xff808080);
    let parent = image(0x645096dc);
    let child = image(0xa0c86428);
    let mut nodes = vec![
        node(&base, size, None, Blend::Opaque),
        node(&parent, size, Some(0), Blend::PsMultiplicative),
        node(&child, size, Some(1), Blend::Opaque),
    ];
    nodes[2].rectangle.left = 1;
    nodes[2].rectangle.width = 1;
    let scene = Scene {
        viewport: Default::default(),
        transitions: Vec::new(),
        requires_op_seq: 0,
        nodes,
    };
    let mut output = gpu.create_surface_image(size).unwrap();
    gpu.compose(&mut output, &scene, &images).unwrap();
    // Current legacy MMX PS multiply interpolates all four bytes on an opaque
    // parent (no HDA), including the completed group's alpha.
    assert_eq!(read(&gpu, &output), [93, 107, 120, 194, 100, 50, 20, 254]);
}

#[test]
fn physical_raster_keeps_the_logical_grid_through_clips_nested_groups_and_compact_images() {
    use krkr_protocol::{
        budget::Budget,
        graphics::Rect,
        pixels::{Bytes, Pixels},
    };
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 7,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let logical = Size {
        width: 37,
        height: 23,
    };
    let stored = Size {
        width: 19,
        height: 12,
    };
    let mut raw = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
    for (i, p) in raw
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        p.copy_from_slice(&[
            (i * 31 + 19) as u8,
            (i * 71 + 37) as u8,
            (i * 43 + 251) as u8,
            (i * 17) as u8,
        ]);
    }
    let image = gpu
        .assign_bitmap(
            None,
            &Pixels {
                size: stored,
                main: Some(raw),
                province: None,
            },
        )
        .unwrap();
    let image = gpu.logical_image(image, logical).unwrap();
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let images = HashMap::from([(reference.id, image)]);
    let mut nodes = vec![
        node(&reference, logical, None, Blend::Opaque),
        node(&reference, logical, Some(0), Blend::PsMultiplicative),
        node(&reference, logical, Some(1), Blend::Alpha),
        node(&reference, logical, Some(2), Blend::Opaque),
    ];
    nodes[1].rectangle = Rect {
        left: -3,
        top: 2,
        width: 31,
        height: 19,
    };
    nodes[1].image_left = 3;
    nodes[1].image_top = -2;
    nodes[1].opacity = 121;
    nodes[2].rectangle = Rect {
        left: 9,
        top: -2,
        width: 15,
        height: 11,
    };
    nodes[2].opacity = 93;
    nodes[3].rectangle = Rect {
        left: -1,
        top: 3,
        width: 9,
        height: 4,
    };
    nodes[3].image_left = -7;
    let scene = Scene {
        nodes,
        ..Default::default()
    };
    let full = gpu.scene_surface(logical, &scene, &images, (0, 0)).unwrap();
    let expected = read(&gpu, &full);
    drop(full);
    gpu.collect().unwrap();
    let resident = gpu.resident.used();
    for physical in [
        Size {
            width: 13,
            height: 9,
        },
        Size {
            width: 18,
            height: 12,
        },
        Size {
            width: 74,
            height: 46,
        },
    ] {
        // At most two physical rows per nested group fit under this budget.
        gpu.scratch = Budget::new(physical.rgba_bytes().unwrap() + physical.width as usize * 4 * 6);
        let output = gpu
            .scene_surface_scaled(logical, physical, &scene, &images)
            .unwrap();
        let got = read(&gpu, &output);
        for y in 0..physical.height {
            for x in 0..physical.width {
                let sx = (u64::from(x) * 2 + 1) * u64::from(logical.width)
                    / (u64::from(physical.width) * 2);
                let sy = (u64::from(y) * 2 + 1) * u64::from(logical.height)
                    / (u64::from(physical.height) * 2);
                let src = (sy * u64::from(logical.width) + sx) as usize * 4;
                let dst = (y * physical.width + x) as usize * 4;
                assert_eq!(
                    &got[dst..dst + 4],
                    &expected[src..src + 4],
                    "{physical:?}, at {x},{y}"
                );
            }
        }
        gpu.collect().unwrap();
        assert_eq!(gpu.resident.used(), resident);
        assert_eq!(gpu.scratch.used(), physical.rgba_bytes().unwrap());
        assert_eq!(images[&reference.id].stored_size(), Some(stored));
        drop(output);
        gpu.collect().unwrap();
    }
    // Crop/capture remains in script coordinates, independent of display scale.
    gpu.scratch = Budget::new(65536);
    let crop = Size {
        width: 11,
        height: 7,
    };
    let output = gpu.scene_surface(crop, &scene, &images, (5, 3)).unwrap();
    let got = read(&gpu, &output);
    for y in 0..crop.height as usize {
        let src = ((y + 3) * logical.width as usize + 5) * 4;
        let dst = y * crop.width as usize * 4;
        assert_eq!(
            &got[dst..dst + crop.width as usize * 4],
            &expected[src..src + crop.width as usize * 4]
        );
    }
}

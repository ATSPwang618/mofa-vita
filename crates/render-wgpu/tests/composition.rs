use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, ImageRef, Node, Scene, Size},
};
use krkr_render_wgpu::{Gpu, Image};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

fn gpu() -> Gpu {
    pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap()
}
fn read(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let started = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result.unwrap().data.as_slice().to_vec();
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
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
#[ignore = "requires a real desktop GPU"]
fn character_runs_match_ordered_blends_across_overlaps() {
    use krkr_protocol::graphics::{BlendOptions, DrawFace};
    let gpu = gpu();
    let size = Size {
        width: 640,
        height: 240,
    };
    let glyph = Size {
        width: 16,
        height: 20,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let refs = [(); 2].map(|_| ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    });
    let images = HashMap::from([
        (refs[0].id, gpu.create_image(size, 0xff203040).unwrap()),
        (refs[1].id, gpu.create_image(glyph, 0xb0e08040).unwrap()),
    ]);
    let mut scene = Scene {
        nodes: vec![node(&refs[0], size, None, Blend::Opaque)],
        ..Default::default()
    };
    for i in 0..160 {
        let mut n = node(&refs[1], glyph, Some(0), Blend::Alpha);
        n.rectangle.left = (i % 32) * 19;
        n.rectangle.top = (i / 32) * 24;
        n.opacity = 170 + (i % 70) as u8;
        scene.nodes.push(n);
    }
    let mut output = gpu.create_surface_image(size).unwrap();
    let mut timings = Vec::new();
    for i in 0..60 {
        let start = Instant::now();
        gpu.compose(&mut output, &scene, &images).unwrap();
        if i >= 10 {
            timings.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        // Bound queued GPU work; do not include readback in CPU encoding time.
        read(&gpu, &output);
    }
    timings.sort_by(f64::total_cmp);
    eprintln!(
        "160 glyphs compose CPU: median={:.3} ms p95={:.3} ms",
        timings[25], timings[47]
    );
    // Switch between destination reads and RGB-only opaque copies inside a batch.
    for n in scene.nodes.iter_mut().skip(1).step_by(3) {
        n.blend = Blend::Opaque;
        n.opacity = 255;
    }
    // Overlapping and arithmetic layers must preserve all intervening writes.
    for (x, blend) in [
        (3, Blend::Alpha),
        (10, Blend::Opaque),
        (14, Blend::AddAlpha),
    ] {
        let mut n = node(&refs[1], glyph, Some(0), blend);
        n.rectangle.left = x;
        n.opacity = 191;
        scene.nodes.push(n);
    }
    let mut expected = gpu.create_image(size, 0xff203040).unwrap();
    for n in &scene.nodes[1..] {
        gpu.operate_rect(
            &mut expected,
            &images[&refs[1].id].source(),
            glyph.rect(),
            n.rectangle.left,
            n.rectangle.top,
            size.rect(),
            BlendOptions::for_composition(n.blend, DrawFace::Opaque, n.opacity),
        )
        .unwrap();
    }
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(read(&gpu, &output), read(&gpu, &expected));
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn character_batches_stay_within_the_reserved_band_budget() {
    use krkr_protocol::graphics::{BlendOptions, DrawFace};
    let mut gpu = gpu();
    let size = Size {
        width: 64,
        height: 32,
    };
    let glyph = Size {
        width: 8,
        height: 2,
    };
    // One output surface and one eight-row backdrop, with no spare capacity.
    gpu.scratch = Budget::new(size.rgba_bytes().unwrap() + 64 * 8 * 4);
    let mut ids = slotmap::SlotMap::with_key();
    let refs = [(); 2].map(|_| ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    });
    let images = HashMap::from([
        (refs[0].id, gpu.create_image(size, 0xff203040).unwrap()),
        (refs[1].id, gpu.create_image(glyph, 0xb0e08040).unwrap()),
    ]);
    let mut scene = Scene {
        nodes: vec![node(&refs[0], size, None, Blend::Opaque)],
        ..Default::default()
    };
    for y in [6, 14, 22, 30] {
        let mut n = node(&refs[1], glyph, Some(0), Blend::Alpha);
        n.rectangle.top = y;
        scene.nodes.push(n);
    }
    let mut output = gpu.create_surface_image(size).unwrap();
    gpu.compose(&mut output, &scene, &images).unwrap();
    let actual = read(&gpu, &output);
    let mut expected = gpu.create_image(size, 0xff203040).unwrap();
    for n in &scene.nodes[1..] {
        gpu.operate_rect(
            &mut expected,
            &images[&refs[1].id].source(),
            glyph.rect(),
            0,
            n.rectangle.top,
            size.rect(),
            BlendOptions::for_composition(n.blend, DrawFace::Opaque, n.opacity),
        )
        .unwrap();
    }
    assert_eq!(actual, read(&gpu, &expected));
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn long_cached_paragraph_reuses_its_raster_during_parent_fades() {
    let gpu = gpu();
    let size = Size {
        width: 320,
        height: 100,
    };
    let glyph = Size {
        width: 8,
        height: 10,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let refs = [(); 2].map(|_| ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    });
    let images = HashMap::from([
        (refs[0].id, gpu.create_image(size, 0xff203040).unwrap()),
        (refs[1].id, gpu.create_image(glyph, 0xb0e08040).unwrap()),
    ]);
    let mut group = node(&refs[0], size, Some(0), Blend::Alpha);
    group.image = None;
    group.opacity = 180;
    group.cache = Some(Arc::default());
    let mut scene = Scene {
        nodes: vec![node(&refs[0], size, None, Blend::Opaque), group],
        ..Default::default()
    };
    for i in 0..160 {
        let mut n = node(&refs[1], glyph, Some(1), Blend::Alpha);
        n.rectangle.left = (i % 32) * 10;
        n.rectangle.top = (i / 32) * 12;
        scene.nodes.push(n);
    }
    let mut output = gpu.create_surface_image(size).unwrap();
    gpu.compose(&mut output, &scene, &images).unwrap();
    let hits = gpu.cached_subtree_hits();
    scene.nodes[1].opacity = 181;
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(gpu.cached_subtree_hits(), hits + 1);
    let cached = read(&gpu, &output);
    scene.nodes[1].cache = None;
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(read(&gpu, &output), cached);
}
#[test]
#[ignore = "requires a real desktop GPU"]
fn clipped_composition_crops_a_larger_cached_group() {
    let gpu = gpu();
    let size = Size {
        width: 8,
        height: 6,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let refs = [(); 3].map(|_| ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    });
    let images = refs
        .iter()
        .zip([0xff204060, 0x804080c0, 0xffe0a020])
        .map(|(r, color)| (r.id, gpu.create_image(size, color).unwrap()))
        .collect::<HashMap<_, _>>();
    let mut scene = Scene {
        nodes: vec![
            node(&refs[0], size, None, Blend::Opaque),
            node(
                &refs[1],
                Size {
                    width: 6,
                    height: 4,
                },
                Some(0),
                Blend::Alpha,
            ),
            node(
                &refs[2],
                Size {
                    width: 2,
                    height: 2,
                },
                Some(1),
                Blend::Opaque,
            ),
        ],
        ..Default::default()
    };
    scene.nodes[1].cache = Some(Arc::new(()));
    scene.nodes[1].rectangle.left = 1;
    scene.nodes[1].rectangle.top = 1;
    scene.nodes[1].opacity = 137;
    scene.nodes[2].rectangle.left = 2;
    scene.nodes[2].rectangle.top = 1;
    let mut full = gpu.create_surface_image(size).unwrap();
    gpu.compose(&mut full, &scene, &images).unwrap();
    let expected = read(&gpu, &full);
    let patch_size = Size {
        width: 2,
        height: 2,
    };
    let mut patch = gpu.create_surface_image(patch_size).unwrap();
    for origin in [(2, 2), (4, 3), (0, 0)] {
        let hits = gpu.cached_subtree_hits();
        gpu.compose_region(&mut patch, &scene, &images, origin)
            .unwrap();
        assert_eq!(gpu.cached_subtree_hits(), hits + 1);
        let mut cropped = Vec::new();
        for y in origin.1..origin.1 + 2 {
            let start = (y as usize * size.width as usize + origin.0 as usize) * 4;
            cropped.extend_from_slice(&expected[start..start + 8]);
        }
        assert_eq!(read(&gpu, &patch), cropped);
    }
}
#[test]
#[ignore = "requires a real desktop GPU"]
fn cached_subtrees_reuse_pixels_and_invalidate_after_content_or_tree_changes() {
    use krkr_protocol::graphics::{DrawFace, Fill};
    let gpu = gpu();
    let size = Size {
        width: 4,
        height: 2,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let refs = [(); 3].map(|_| ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    });
    let mut images = refs
        .iter()
        .zip([0xff204060, 0x804080c0, 0xffe0a020])
        .map(|(r, color)| (r.id, gpu.create_image(size, color).unwrap()))
        .collect::<HashMap<_, _>>();
    // Tiny uniform images initially share the constructor's color cache.
    // Detach that owner before testing whether the subtree cache adds one.
    for image in images.values_mut() {
        gpu.independ_image(image, false, true).unwrap();
        assert_eq!(image.main_write_bytes(), 0);
    }
    let token = Arc::new(());
    let mut scene = Scene {
        nodes: vec![
            node(&refs[0], size, None, Blend::Opaque),
            node(&refs[1], size, Some(0), Blend::Alpha),
            node(&refs[2], size, Some(1), Blend::Opaque),
        ],
        ..Default::default()
    };
    scene.nodes[1].cache = Some(token.clone());
    scene.nodes[2].rectangle.width = 1;
    scene.nodes[2].rectangle.left = 1;
    let mut output = gpu.create_surface_image(size).unwrap();
    let mut reference = gpu.create_surface_image(size).unwrap();
    let compare =
        |scene: &Scene, images: &HashMap<_, _>, output: &mut Image, reference: &mut Image| {
            gpu.compose(output, scene, images).unwrap();
            let plain = Scene {
                nodes: scene
                    .nodes
                    .iter()
                    .cloned()
                    .map(|mut n| {
                        n.cache = None;
                        n
                    })
                    .collect(),
                ..Default::default()
            };
            gpu.compose(reference, &plain, images).unwrap();
            assert_eq!(read(&gpu, output), read(&gpu, reference));
        };
    compare(&scene, &images, &mut output, &mut reference);
    let hits = gpu.cached_subtree_hits();
    compare(&scene, &images, &mut output, &mut reference);
    assert_eq!(gpu.cached_subtree_hits(), hits + 1);
    assert_eq!(
        images[&refs[2].id].main_write_bytes(),
        0,
        "cache must not own source pixels"
    );
    let old = read(&gpu, &output);
    gpu.fill(
        images.get_mut(&refs[2].id).unwrap(),
        &[Fill {
            rectangle: size.rect(),
            color: 0xff00ff00,
            face: DrawFace::Opaque,
            hold_alpha: false,
        }],
    )
    .unwrap();
    compare(&scene, &images, &mut output, &mut reference);
    assert_eq!(
        gpu.cached_subtree_hits(),
        hits + 1,
        "changed pixels must miss"
    );
    assert_ne!(read(&gpu, &output), old);
    for blend in [
        Blend::Opaque,
        Blend::Alpha,
        Blend::AddAlpha,
        Blend::PsMultiplicative,
    ] {
        scene.nodes[1].blend = blend;
        for left in [-1, 0, 1] {
            scene.nodes[1].rectangle.left = left;
            scene.nodes[1].opacity = 137;
            scene.nodes[2].rectangle.left = 2;
            compare(&scene, &images, &mut output, &mut reference);
            scene.nodes[2].visible = false;
            compare(&scene, &images, &mut output, &mut reference);
            scene.nodes[2].visible = true;
        }
    }
    // Dropping the request token releases the composed raster. Source images
    // remain independent even while the cache entry is alive.
    scene.nodes[1].cache = None;
    drop(token);
    gpu.poll().unwrap();
    assert_eq!(images[&refs[2].id].main_write_bytes(), 0);
}
#[test]
#[ignore = "requires a real desktop GPU"]
fn imageless_opaque_layers_use_neutral_color_while_alpha_containers_stay_transparent() {
    let gpu = gpu();
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
#[ignore = "requires a real desktop GPU"]
fn advanced_group_blends_the_completed_subtree_once() {
    let gpu = gpu();
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
    // Legacy MMX PS multiply interpolates RGBA on an opaque parent (no HDA),
    // matching the GLES composition fixture and tvpps_asm.nas MulBlend.1.
    assert_eq!(read(&gpu, &output), [93, 107, 120, 194, 100, 50, 20, 254]);
}

#[test]
#[ignore = "requires a real desktop GPU"]
fn window_viewport_scales_complete_layers_and_clears_old_exposed_pixels() {
    let gpu = gpu();
    let size = Size {
        width: 4,
        height: 2,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let base = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let child = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let images = HashMap::from([
        (base.id, gpu.create_image(size, 0xffff0000).unwrap()),
        (child.id, gpu.create_image(size, 0xff0000ff).unwrap()),
    ]);
    let mut scene = Scene {
        viewport: Default::default(),
        requires_op_seq: 0,
        transitions: vec![],
        nodes: vec![
            node(&base, size, None, Blend::Opaque),
            node(&child, size, Some(0), Blend::Opaque),
        ],
    };
    scene.nodes[1].rectangle.left = 2;
    scene.nodes[1].rectangle.width = 2;
    scene.viewport = scene.viewport.zoom(2, 1).unwrap();
    scene.viewport.left = 1;
    scene.viewport.top = 1;
    let mut output = gpu
        .create_surface_image(Size {
            width: 10,
            height: 6,
        })
        .unwrap();
    let mut logical = None;
    gpu.compose_window(&mut output, &mut logical, &scene, &images)
        .unwrap();
    let pixels = read(&gpu, &output);
    let pixel = |x: usize, y: usize| &pixels[(y * 10 + x) * 4..(y * 10 + x + 1) * 4];
    assert_eq!(pixel(0, 0), [0, 0, 0, 0]);
    assert_eq!(pixel(2, 2), [255, 0, 0, 255]);
    assert_eq!(pixel(7, 2), [0, 0, 255, 255]);
    assert_eq!(pixel(9, 2), [0, 0, 0, 0]);
    scene.viewport.left = -3;
    scene.viewport.top = -1;
    gpu.compose_window(&mut output, &mut logical, &scene, &images)
        .unwrap();
    let pixels = read(&gpu, &output);
    assert_eq!(&pixels[3 * 4..4 * 4], [0, 0, 255, 255]);
    assert_eq!(&pixels[(2 * 10 + 7) * 4..(2 * 10 + 8) * 4], [0, 0, 0, 0]);
    scene.viewport = Default::default();
    gpu.compose_window(&mut output, &mut logical, &scene, &images)
        .unwrap();
    assert!(logical.is_none());
    let pixels = read(&gpu, &output);
    assert_eq!(&pixels[0..4], [255, 0, 0, 255]);
    assert_eq!(&pixels[3 * 4..4 * 4], [0, 0, 255, 255]);
    assert_eq!(&pixels[4 * 4..5 * 4], [0, 0, 0, 0]);
}
#[test]
#[ignore = "requires a real desktop GPU"]
fn many_fullscreen_layers_share_one_backdrop_and_opaque_draws_need_none() {
    let mut gpu = gpu();
    let size = Size {
        width: 960,
        height: 544,
    };
    let bytes = size.rgba_bytes().unwrap();
    gpu.scratch = Budget::new(bytes * 2);
    let mut ids = slotmap::SlotMap::with_key();
    let id = ids.insert(());
    let image = ImageRef {
        id,
        lifetime: Arc::default(),
    };
    let images = HashMap::from([(id, gpu.create_image(size, 0xff808080).unwrap())]);
    let mut output = gpu.create_surface_image(size).unwrap();
    let mut scene = Scene {
        viewport: Default::default(),
        transitions: Vec::new(),
        requires_op_seq: 0,
        nodes: vec![node(&image, size, None, Blend::Opaque)],
    };
    for _ in 0..32 {
        scene.nodes.push(node(&image, size, Some(0), Blend::Opaque));
    }
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(&read(&gpu, &output)[..4], &[128, 128, 128, 255]);
    assert_eq!(
        gpu.scratch.used(),
        bytes,
        "opaque copies must not allocate a backdrop"
    );
    for child in &mut scene.nodes[1..] {
        child.blend = Blend::PsMultiplicative;
    }
    gpu.compose(&mut output, &scene, &images).unwrap();
    // Each full-opacity MMX multiply loses one alpha unit: 255 - 32.
    assert_eq!(&read(&gpu, &output)[..4], &[0, 0, 0, 223]);
    assert_eq!(
        gpu.scratch.used(),
        bytes * 2,
        "32 advanced draws reuse one backdrop"
    );
    // Rejected admission happens before GPU pixel commands are submitted.
    let mut grouped = Scene {
        viewport: Default::default(),
        transitions: Vec::new(),
        requires_op_seq: 0,
        nodes: scene.nodes[..3].to_vec(),
    };
    grouped.nodes[2].parent = Some(1);
    // Group bands fit this budget now. Pin the free bytes to exercise actual
    // admission failure, independently of the renderer's banding strategy.
    gpu.trim_scratch();
    let held = gpu.scratch.reserve(gpu.scratch.available()).unwrap();
    assert!(gpu.compose(&mut output, &grouped, &images).is_err());
    drop(held);
    assert_eq!(&read(&gpu, &output)[..4], &[0, 0, 0, 223]);
    gpu.trim_scratch();
    assert_eq!(gpu.scratch.used(), bytes);
}

#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{Blend, DrawFace, Fill, ImageRef, Node, Rect, Scene, Size},
};
use krkr_render_gles2::{Config, Gpu, Image, SceneState};
use std::{collections::HashMap, sync::Arc};

fn node(reference: &ImageRef, size: Size, parent: Option<usize>, blend: Blend) -> Node {
    Node {
        cache: None,
        parent,
        visible: true,
        image: Some(reference.clone()),
        neutral_color: 0,
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend,
        opacity: 255,
    }
}
fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}

#[test]
fn opaque_scene_alpha_overlays_avoid_destination_transfers() {
    check_opaque_scene_overlay(Blend::Alpha, 255, false, true);
    check_opaque_scene_overlay(Blend::Alpha, 255, false, false);
}

#[test]
fn movie_additive_text_and_fades_avoid_destination_transfers() {
    for opacity in [1, 32, 128, 254, 255] {
        check_opaque_scene_overlay(Blend::AddAlpha, opacity, false, true);
        check_opaque_scene_overlay(Blend::AddAlpha, opacity, true, true);
    }
}

#[test]
fn ordinary_scene_premultiplied_overlays_avoid_destination_transfers() {
    for opacity in [32, 128, 255] {
        for root in [false, true] {
            check_opaque_scene_overlay(Blend::AddAlpha, opacity, root, false);
        }
    }
}

fn check_opaque_scene_overlay(mode: Blend, opacity: u8, root: bool, movie: bool) {
    use krkr_protocol::pixels::{Bytes, Pixels, Yuv420, Yuv420Layout};
    let size = Size {
        width: 128,
        height: 128,
    };
    let physical = Size {
        width: 96,
        height: 96,
    };
    let mut outputs = Vec::new();
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: work,
                    render_target_cache_entries: 8,
                    render_target_cache_bytes: 1024 * 1024,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut ids = slotmap::SlotMap::with_key();
        let refs: Vec<_> = (0..2)
            .map(|_| ImageRef {
                id: ids.insert(()),
                lifetime: Arc::default(),
            })
            .collect();
        let mut images = HashMap::new();
        for (index, reference) in refs.iter().enumerate() {
            if index == 0 && movie {
                let mut data =
                    Bytes::zeroed(Yuv420::byte_len(size).unwrap(), &gpu.staging).unwrap();
                for (pixel, y) in data.as_mut_slice()[..128 * 128].iter_mut().enumerate() {
                    *y = 16 + (pixel % 220) as u8;
                }
                data.as_mut_slice()[128 * 128..].fill(128);
                images.insert(
                    reference.id,
                    gpu.upload_yuv(
                        &Yuv420 {
                            size,
                            layout: Yuv420Layout::Nv12,
                            data,
                        },
                        size,
                    )
                    .unwrap(),
                );
                continue;
            }
            let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
            for (pixel, rgba) in bytes
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                rgba.copy_from_slice(&[
                    (pixel.wrapping_mul(37 + index)) as u8,
                    (pixel.wrapping_mul(71)) as u8,
                    (255 - pixel % 256) as u8,
                    if index == 0 { 255 } else { (pixel % 256) as u8 },
                ]);
                if mode == Blend::AddAlpha {
                    for channel in 0..3 {
                        rgba[channel] = (u32::from(rgba[channel]) * u32::from(rgba[3]) / 255) as u8;
                    }
                }
            }
            let mut image = gpu.reserve_upload(size, true, false).unwrap();
            gpu.upload(
                &mut image,
                &Pixels {
                    size,
                    main: Some(bytes),
                    province: None,
                },
            )
            .unwrap();
            images.insert(reference.id, image);
        }
        let mut scene = Scene {
            nodes: vec![
                node(&refs[0], size, None, Blend::Opaque),
                node(&refs[1], size, (!root).then_some(0), mode),
            ],
            ..Default::default()
        };
        scene.nodes[1].opacity = opacity;
        let (mut canvas, mut state) = (None, SceneState::default());
        gpu.update_scene_surface(&mut canvas, &mut state, size, physical, &scene, &images)
            .unwrap();
        // Force another frame into the same retained target.
        scene.nodes[1].image_left = -1;
        traffic::reset();
        gpu.update_scene_surface(&mut canvas, &mut state, size, physical, &scene, &images)
            .unwrap();
        eprintln!(
            "movie overlay mode={mode:?} opacity={opacity} root={root} work={work}: loads={} stores={} stored_pixels={}",
            traffic::load_calls(),
            traffic::store_calls(),
            traffic::stored_pixels(),
        );
        if work {
            assert_eq!(
                traffic::store_calls(),
                0,
                "alpha composition stored a destination backdrop"
            );
            assert_eq!(traffic::read_calls(), 0);
        }
        outputs.push(pixels(&gpu, canvas.as_ref().unwrap()));
    }
    // Fixed blending rounds normalized values; script image arithmetic stays
    // byte exact. Presentation differences are bounded to two byte levels.
    assert_eq!(outputs[0].len(), outputs[1].len());
    for (index, (before, after)) in outputs[0].iter().zip(&outputs[1]).enumerate() {
        assert!(
            before.abs_diff(*after) <= 2,
            "mode={mode:?} opacity={opacity} channel {index}: {before} -> {after}"
        );
    }
}

#[test]
fn retained_universal_transition_tracks_the_selected_rule_plane() {
    use krkr_protocol::transition::{Effect, Frame, SceneTransition};
    for work in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 16,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 24,
            height: 20,
        };
        let mut ids = slotmap::SlotMap::with_key();
        let refs: Vec<_> = (0..3)
            .map(|_| ImageRef {
                id: ids.insert(()),
                lifetime: Arc::default(),
            })
            .collect();
        let mut rule = gpu.create_province(size).unwrap();
        gpu.fill(
            &mut rule,
            &[Fill {
                rectangle: size.rect(),
                color: 0,
                face: DrawFace::Province,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let mut images = HashMap::from([
            (refs[0].id, gpu.create_image(size, 0xff123456).unwrap()),
            (refs[1].id, gpu.create_image(size, 0xffabcdef).unwrap()),
            (refs[2].id, rule),
        ]);
        let mut scene = Scene {
            nodes: vec![
                node(&refs[0], size, None, Blend::Alpha),
                node(&refs[1], size, None, Blend::Alpha),
            ],
            transitions: vec![SceneTransition {
                destination: 0,
                source: 1,
                with_children: false,
                frame: Frame {
                    size,
                    effect: Effect::Universal { vague: 63 },
                    face: DrawFace::Alpha,
                    phase: 150,
                },
                rule: Some(refs[2].clone()),
                custom: None,
            }],
            ..Default::default()
        };
        scene.nodes[1].visible = false;
        let (mut canvas, mut state) = (None, SceneState::default());
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_some());
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_none());
        let before = pixels(&gpu, canvas.as_ref().unwrap());
        gpu.fill(
            images.get_mut(&refs[2].id).unwrap(),
            &[Fill {
                rectangle: size.rect(),
                color: 255,
                face: DrawFace::Province,
                hold_alpha: false,
            }],
        )
        .unwrap();
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_some());
        assert_ne!(pixels(&gpu, canvas.as_ref().unwrap()), before);
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_none());

        // A rule with both planes must still observe province edits, while an
        // unrelated main-plane edit leaves the retained frame reusable.
        let both = gpu
            .enable_image(images.get(&refs[2].id), size, 0xff808080)
            .unwrap();
        images.insert(refs[2].id, both);
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_none());
        gpu.fill(
            images.get_mut(&refs[2].id).unwrap(),
            &[Fill {
                rectangle: size.rect(),
                color: 0xff010203,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_none());
        gpu.fill(
            images.get_mut(&refs[2].id).unwrap(),
            &[Fill {
                rectangle: size.rect(),
                color: 0,
                face: DrawFace::Province,
                hold_alpha: false,
            }],
        )
        .unwrap();
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_some());

        // Replacing the rule by an ordinary RGBA bitmap changes the selected
        // plane and remains supported by the same transition cache.
        images.insert(refs[2].id, gpu.create_image(size, 0xffffffff).unwrap());
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_some());
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_none());
    }
}

#[test]
fn unchanged_stamp_still_observes_geometry_images_and_visibility() {
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
        width: 32,
        height: 24,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let a = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let b = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let mut images = HashMap::from([
        (a.id, gpu.create_image(size, 0xff123456).unwrap()),
        (b.id, gpu.create_image(size, 0x80987654).unwrap()),
    ]);
    let mut scene = Scene {
        nodes: vec![
            node(&a, size, None, Blend::Opaque),
            node(
                &b,
                Size {
                    width: 16,
                    height: 12,
                },
                Some(0),
                Blend::Alpha,
            ),
        ],
        ..Default::default()
    };
    let original = scene.nodes.clone();
    let (mut canvas, mut state) = (None, SceneState::default());
    check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
    assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_none());
    for change in 0..9 {
        scene.nodes = original.clone();
        match change {
            0 => scene.nodes[1].rectangle.left = 3,
            1 => scene.nodes[1].rectangle.width = 7,
            2 => scene.nodes[1].image_left = 4,
            3 => scene.nodes[1].opacity = 71,
            4 => scene.nodes[1].visible = false,
            5 => scene.nodes[1].blend = Blend::Opaque,
            6 => scene.nodes[1].parent = None,
            7 => {
                scene.nodes[1].image = None;
                scene.nodes[1].blend = Blend::Opaque;
                scene.nodes[1].neutral_color = 0xffc08040;
            }
            _ => scene.nodes[1].image = Some(a.clone()),
        }
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
        assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_none());
    }
    scene.nodes = original;
    check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
    gpu.fill(
        images.get_mut(&b.id).unwrap(),
        &[Fill {
            rectangle: Rect {
                left: 3,
                top: 4,
                width: 2,
                height: 2,
            },
            color: 0xff00ff00,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert!(check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).is_some());
    scene.nodes[1].visible = false;
    check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
    let original = images
        .insert(b.id, gpu.reserve_upload(size, false, true).unwrap())
        .unwrap();
    assert!(
        gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
            .is_err()
    );
    images.insert(b.id, original);
    let other = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    assert!(
        other
            .update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
            .is_err()
    );
}
#[track_caller]
fn check(
    gpu: &Gpu,
    canvas: &mut Option<Image>,
    state: &mut SceneState,
    logical: Size,
    physical: Size,
    scene: &Scene,
    images: &HashMap<krkr_protocol::graphics::ImageId, Image>,
) -> Option<Rect> {
    let damage = gpu
        .update_scene_surface(canvas, state, logical, physical, scene, images)
        .unwrap();
    let expected = gpu
        .scene_surface_scaled(logical, physical, scene, images)
        .unwrap();
    let actual = canvas.as_ref().unwrap();
    let mut stored = actual.shared();
    stored.size = physical;
    let a = pixels(gpu, &stored);
    let b = pixels(gpu, &expected);
    let differences: Vec<_> = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0.iter())
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .take(8)
        .map(|(i, (a, b))| {
            (
                i % physical.width as usize,
                i / physical.width as usize,
                a,
                b,
            )
        })
        .collect();
    assert!(
        differences.is_empty(),
        "damage={damage:?}, different pixels={differences:?}"
    );
    damage
}

#[test]
fn movie_paragraph_reuses_previous_glyphs_while_adding_text() {
    use krkr_protocol::pixels::{Bytes, Yuv420, Yuv420Layout};
    let context = support::Context::new();
    let size = Size {
        width: 1024,
        height: 576,
    };
    let physical = Size {
        width: 960,
        height: 540,
    };
    let glyph_size = Size {
        width: 20,
        height: 24,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                render_target_cache_entries: 8,
                render_target_cache_bytes: 16 * 1024 * 1024,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut ids = slotmap::SlotMap::with_key();
    let refs: Vec<_> = (0..3)
        .map(|_| ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        })
        .collect();
    let mut data = Bytes::zeroed(Yuv420::byte_len(size).unwrap(), &gpu.staging).unwrap();
    data.as_mut_slice()[..1024 * 576].fill(128);
    data.as_mut_slice()[1024 * 576..].fill(128);
    let frame = Yuv420 {
        size,
        layout: Yuv420Layout::Nv12,
        data,
    };
    let mut images = HashMap::from([
        (refs[0].id, gpu.upload_yuv(&frame, size).unwrap()),
        (refs[1].id, gpu.create_image(size, 0).unwrap()),
        (
            refs[2].id,
            gpu.create_image(glyph_size, 0xb4d7c5a3).unwrap(),
        ),
    ]);
    let mut message = node(&refs[1], size, Some(0), Blend::AddAlpha);
    message.cache = Some(Arc::default());
    let mut scene = Scene {
        nodes: vec![node(&refs[0], size, None, Blend::Opaque), message],
        ..Default::default()
    };
    let (mut canvas, mut state) = (None, SceneState::default());
    let mut costs = Vec::new();
    for count in [20, 80, 160] {
        while scene.nodes.len() < count + 2 {
            let index = scene.nodes.len() - 2;
            let mut glyph = node(&refs[2], glyph_size, Some(1), Blend::Alpha);
            glyph.rectangle.left = 16 + (index % 40) as i32 * 24;
            glyph.rectangle.top = 32 + (index / 40) as i32 * 32;
            scene.nodes.push(glyph);
        }
        gpu.update_scene_surface(&mut canvas, &mut state, size, physical, &scene, &images)
            .unwrap();
        let mut glyph = scene.nodes.last().unwrap().clone();
        glyph.rectangle.left += 24;
        scene.nodes.push(glyph);
        images.insert(refs[0].id, gpu.upload_yuv(&frame, size).unwrap());
        traffic::reset();
        gpu.update_scene_surface(&mut canvas, &mut state, size, physical, &scene, &images)
            .unwrap();
        let draws = traffic::draw_calls();
        let stores = traffic::store_calls();
        let copied = traffic::stored_pixels();
        eprintln!(
            "movie paragraph glyphs={count} draws={draws} stores={stores} copied_pixels={copied}"
        );
        costs.push(draws);
        let actual = pixels(&gpu, canvas.as_ref().unwrap());
        gpu.collect().unwrap();
        let mut fresh = None;
        gpu.update_scene_surface(
            &mut fresh,
            &mut SceneState::default(),
            size,
            physical,
            &scene,
            &images,
        )
        .unwrap();
        assert_eq!(actual, pixels(&gpu, fresh.as_ref().unwrap()));
    }
    assert!(
        costs[2] <= costs[0] + 4,
        "previous glyphs were redrawn: {costs:?}"
    );
}

#[test]
fn paragraph_opacity_reuses_composition_beyond_128_character_layers() {
    let context = support::Context::new();
    let size = Size {
        width: 320,
        height: 240,
    };
    let glyph_size = Size {
        width: 8,
        height: 8,
    };
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
    let mut ids = slotmap::SlotMap::with_key();
    let refs: Vec<_> = (0..2)
        .map(|_| ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        })
        .collect();
    let images = HashMap::from([
        (refs[0].id, gpu.create_image(size, 0xff314159).unwrap()),
        (
            refs[1].id,
            gpu.create_image(glyph_size, 0x8bca2745).unwrap(),
        ),
    ]);
    let mut group = node(&refs[0], size, Some(0), Blend::Alpha);
    group.image = None;
    group.opacity = 180;
    let mut scene = Scene {
        nodes: vec![node(&refs[0], size, None, Blend::Opaque), group],
        ..Default::default()
    };
    for i in 0..160 {
        let mut glyph = node(&refs[1], glyph_size, Some(1), Blend::Alpha);
        glyph.rectangle.left = (i % 20) * 14;
        glyph.rectangle.top = (i / 20) * 20;
        scene.nodes.push(glyph);
    }
    let (mut canvas, mut state) = (None, SceneState::default());
    traffic::reset();
    gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
        .unwrap();
    let first = traffic::draw_calls();
    let first_loads = traffic::loaded_pixels();
    scene.nodes[1].opacity = 181;
    traffic::reset();
    gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
        .unwrap();
    let repeated = traffic::draw_calls();
    eprintln!(
        "paragraph fade draws: first={first} repeated={repeated}, loaded pixels: {first_loads} -> {}",
        traffic::loaded_pixels()
    );
    assert!(
        repeated * 4 < first,
        "unchanged glyph layers were recomposed: {first} -> {repeated}"
    );
    let actual = pixels(&gpu, canvas.as_ref().unwrap());
    gpu.collect().unwrap(); // Verify against a fresh composition without the cache.
    let expected = gpu
        .scene_surface_scaled(size, size, &scene, &images)
        .unwrap();
    assert_eq!(actual, pixels(&gpu, &expected));
}

#[test]
fn inserted_glyph_before_fullscreen_overlay_only_repaints_its_pixels() {
    for work in [false, true] {
        let context = support::Context::new();
        let size = Size {
            width: 64,
            height: 48,
        };
        let glyph_size = Size {
            width: 8,
            height: 8,
        };
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
        let mut ids = slotmap::SlotMap::with_key();
        let references: Vec<_> = (0..3)
            .map(|_| ImageRef {
                id: ids.insert(()),
                lifetime: Arc::default(),
            })
            .collect();
        let images = HashMap::from([
            (
                references[0].id,
                gpu.create_image(size, 0xff314159).unwrap(),
            ),
            (
                references[1].id,
                gpu.create_image(glyph_size, 0x8bca2745).unwrap(),
            ),
            (
                references[2].id,
                gpu.create_image(size, 0x70358fb1).unwrap(),
            ),
        ]);
        let root = node(&references[0], size, None, Blend::Opaque);
        let overlay = node(&references[2], size, Some(0), Blend::Alpha);
        let mut glyph = node(&references[1], glyph_size, Some(0), Blend::Alpha);
        glyph.rectangle.left = 16;
        glyph.rectangle.top = 24;
        let mut scene = Scene {
            nodes: vec![root, overlay],
            ..Default::default()
        };
        let (mut canvas, mut state) = (None, SceneState::default());
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
        scene.nodes.insert(1, glyph);
        for inserted in [true, false] {
            if !inserted {
                scene.nodes.remove(1);
            }
            let damage = check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).unwrap();
            assert!(damage.width <= 16 && damage.height <= 16, "{damage:?}");
        }
    }
}

#[test]
fn changed_node_count_repaints_visible_extents_and_matches_full_alpha_composition() {
    for work in [false, true] {
        let context = support::Context::new();
        let logical = Size {
            width: 128,
            height: 96,
        };
        let physical = Size {
            width: 64,
            height: 48,
        };
        let small = Size {
            width: 16,
            height: 12,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    canvas_limit: Some(physical),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut ids = slotmap::SlotMap::with_key();
        let references: Vec<_> = (0..3)
            .map(|_| ImageRef {
                id: ids.insert(()),
                lifetime: Arc::default(),
            })
            .collect();
        let images = HashMap::from([
            (
                references[0].id,
                gpu.create_image(logical, 0xff314159).unwrap(),
            ),
            (
                references[1].id,
                gpu.create_image(small, 0x8bca2745).unwrap(),
            ),
            (
                references[2].id,
                gpu.create_image(small, 0x9f358fb1).unwrap(),
            ),
        ]);
        let mut root = node(&references[0], logical, None, Blend::Alpha);
        root.opacity = 173;
        let mut a = node(&references[1], small, Some(0), Blend::Alpha);
        a.rectangle.left = 20;
        a.rectangle.top = 24;
        let mut b = node(&references[2], small, Some(0), Blend::AddAlpha);
        b.rectangle.left = 28;
        b.rectangle.top = 28;
        let mut nested = b.clone();
        nested.parent = Some(1);
        nested.rectangle.left = 2;
        nested.rectangle.top = 2;
        let (mut canvas, mut state) = (None, SceneState::default());
        for (i, nodes) in [
            vec![root.clone()],
            vec![root.clone(), a.clone()],
            vec![root.clone(), a.clone(), nested, b.clone()],
            vec![root.clone(), b.clone()],
            vec![root.clone(), a, b],
            vec![root],
        ]
        .into_iter()
        .enumerate()
        {
            let scene = Scene {
                nodes,
                ..Default::default()
            };
            let damage = check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images,
            )
            .unwrap();
            if i != 0 {
                assert!(
                    damage.width < physical.width / 2 && damage.height < physical.height / 2,
                    "small tree change repainted the canvas: {damage:?}"
                );
            }
        }
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &Scene::default(),
            &images,
        );
    }
}

#[test]
fn copied_textures_keep_local_damage_after_snapshots_are_released() {
    for work in [false, true] {
        let context = support::Context::new();
        let logical = Size {
            width: 256,
            height: 192,
        };
        let physical = Size {
            width: 128,
            height: 96,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    canvas_limit: Some(physical),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(logical);
        let mut ids = slotmap::SlotMap::with_key();
        let reference = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let mut images =
            HashMap::from([(reference.id, gpu.create_image(logical, 0xff234567).unwrap())]);
        // Materialize once before testing local COW damage. A first write to
        // a virtual solid changes its sampling grid and invalidates the whole image.
        gpu.fill(
            images.get_mut(&reference.id).unwrap(),
            &[Fill {
                rectangle: Rect {
                    left: 100,
                    top: 100,
                    width: 4,
                    height: 4,
                },
                color: 0xff345678,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let scene = Scene {
            nodes: vec![node(&reference, logical, None, Blend::Opaque)],
            ..Default::default()
        };
        let (mut canvas, mut state) = (None, SceneState::default());
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        let write = |image: &mut Image, x, color| {
            gpu.fill(
                image,
                &[Fill {
                    rectangle: Rect {
                        left: x,
                        top: 28,
                        width: 6,
                        height: 8,
                    },
                    color,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
        };
        // Emulate a published scene pinning the old bitmap while the VM draws
        // its next glyph. Release the old allocation before comparing stamps.
        let pinned = images[&reference.id].shared();
        write(images.get_mut(&reference.id).unwrap(), 18, 0xffcc8844);
        drop(pinned);
        gpu.maintain().unwrap();
        let damage = check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        )
        .unwrap();
        assert!(
            damage.width <= 16 && damage.height <= 16,
            "COW inflated damage: {damage:?}"
        );

        // A complete independent copy alone changes storage, not display pixels.
        gpu.independ(images.get_mut(&reference.id).unwrap(), false, true)
            .unwrap();
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            None
        );

        // Two intervening snapshots preserve the union of both edits.
        let first = images[&reference.id].shared();
        write(images.get_mut(&reference.id).unwrap(), 34, 0xff22aa66);
        let second = images[&reference.id].shared();
        write(images.get_mut(&reference.id).unwrap(), 50, 0xffee3377);
        drop((first, second));
        let damage = check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        )
        .unwrap();
        assert!(
            damage.width <= 32 && damage.height <= 16,
            "copy chain inflated damage: {damage:?}"
        );

        // A fast VM first writes the unpinned image, then publishes another
        // snapshot and triggers COW again before the next display frame. The
        // displayed generation predates the copy's source generation; its
        // in-place prefix must survive after both old textures expire.
        write(images.get_mut(&reference.id).unwrap(), 58, 0xff3355aa);
        let mut first = images[&reference.id].shared();
        write(images.get_mut(&reference.id).unwrap(), 66, 0xff55aa33);
        let second = images[&reference.id].shared();
        write(images.get_mut(&reference.id).unwrap(), 74, 0xffaa3355);
        // Subsequent edits to the old fork must not overwrite the captured
        // history, even when its own ring wraps before the next repaint.
        for i in 0..20 {
            write(&mut first, 180, 0xff123456 + i);
        }
        drop((first, second));
        gpu.maintain().unwrap();
        let damage = check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        )
        .unwrap();
        assert!(
            damage.width <= 24 && damage.height <= 16,
            "in-place edits before COW inflated damage: {damage:?}"
        );

        // Bounded ancestry falls back to a full repaint, never stale pixels.
        for i in 0..6 {
            let pinned = images[&reference.id].shared();
            write(
                images.get_mut(&reference.id).unwrap(),
                70 + i * 8,
                0xff778899 + i as u32,
            );
            drop(pinned);
        }
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            Some(physical.rect())
        );
        // Source history is bounded too. An older displayed generation must
        // conservatively repaint even if the subsequent copy itself is recent.
        for i in 0..20 {
            write(images.get_mut(&reference.id).unwrap(), 22, 0xff123456 + i);
        }
        let pinned = images[&reference.id].shared();
        write(images.get_mut(&reference.id).unwrap(), 30, 0xffabcdef);
        drop(pinned);
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            Some(physical.rect())
        );
        images.insert(reference.id, gpu.create_image(logical, 0xff102030).unwrap());
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            Some(physical.rect())
        );
    }
}

#[test]
fn shrinking_image_repaints_exposed_pixels_inside_unchanged_node() {
    for work in [false, true] {
        let context = support::Context::new();
        let size = Size {
            width: 61,
            height: 43,
        };
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
        let mut ids = slotmap::SlotMap::with_key();
        let reference = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let mut images =
            HashMap::from([(reference.id, gpu.create_image(size, 0xffcc8844).unwrap())]);
        let scene = Scene {
            nodes: vec![node(&reference, size, None, Blend::Opaque)],
            ..Default::default()
        };
        let (mut canvas, mut state) = (None, SceneState::default());
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
        images.insert(
            reference.id,
            gpu.create_image(
                Size {
                    width: 17,
                    height: 11,
                },
                0xff2277bb,
            )
            .unwrap(),
        );
        assert_eq!(
            check(&gpu, &mut canvas, &mut state, size, size, &scene, &images),
            Some(size.rect())
        );
    }
}

#[test]
fn retained_display_matches_full_composition_after_text_movement_hide_and_replacement() {
    for work in [false, true] {
        let context = support::Context::new();
        let logical = Size {
            width: 96,
            height: 64,
        };
        let physical = Size {
            width: 48,
            height: 32,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 16,
                    canvas_limit: Some(physical),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(logical);
        let mut ids = slotmap::SlotMap::with_key();
        let background = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let text = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let mut images = HashMap::from([
            (
                background.id,
                gpu.create_image(logical, 0xff274563).unwrap(),
            ),
            (text.id, gpu.create_image(logical, 0).unwrap()),
        ]);
        gpu.fill(
            images.get_mut(&text.id).unwrap(),
            &[Fill {
                rectangle: Rect {
                    left: 70,
                    top: 40,
                    width: 4,
                    height: 4,
                },
                color: 0xbded9739,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let mut group = node(&text, logical, Some(0), Blend::Alpha);
        group.image = None;
        group.opacity = 171;
        let mut scene = Scene {
            nodes: vec![
                node(&background, logical, None, Blend::Opaque),
                group,
                node(&text, logical, Some(1), Blend::Alpha),
            ],
            ..Default::default()
        };
        let (mut canvas, mut state) = (None, SceneState::default());
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            Some(physical.rect())
        );
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            None
        );
        for x in [4, 16, 28, 40, 52] {
            gpu.fill(
                images.get_mut(&text.id).unwrap(),
                &[Fill {
                    rectangle: Rect {
                        left: x,
                        top: 19,
                        width: 6,
                        height: 9,
                    },
                    color: 0xbded9739,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            let area = check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images,
            )
            .unwrap();
            assert!(
                area.width <= if work { 16 } else { 5 } && area.height <= if work { 16 } else { 7 },
                "glyph damage inflated: {area:?}"
            );
        }
        // Remembering frames cannot pin source textures and cause glyph COW.
        assert_eq!(gpu.canvas_write_bytes(&images[&text.id], false), 0);
        // Repair may shrink an overallocated signature. Account for the live
        // source textures instead of requiring metadata usage to grow forever.
        assert!(gpu.resident.used() >= images.values().map(Image::resident_bytes).sum());
        scene.nodes[1].rectangle.left = 7;
        scene.nodes[1].rectangle.width = 59;
        scene.nodes[1].image_top = -5;
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        scene.nodes[1].opacity = 83;
        scene.nodes[2].blend = Blend::AddAlpha;
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        scene.nodes[1].visible = false;
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        // Hidden source writes stay offscreen, then become visible together.
        gpu.fill(
            images.get_mut(&text.id).unwrap(),
            &[Fill {
                rectangle: logical.rect(),
                color: 0xa010f080,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            None
        );
        scene.nodes[1].visible = true;
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        images.insert(text.id, gpu.create_image(logical, 0xc0f0a040).unwrap());
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        scene.nodes.swap(1, 2);
        scene.nodes[1].parent = Some(0);
        scene.nodes[2].parent = Some(1);
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        scene.nodes.truncate(1);
        assert_eq!(
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images
            ),
            Some(physical.rect())
        );
    }
}

#[test]
fn reuse_admission_does_not_purge_a_full_pool_or_require_another_zero_upload() {
    let context = support::Context::new();
    let staging = Budget::new(1024 * 1024);
    let scratch = Budget::new(32 * 32 * 4 + 32 * 16 * 4);
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 32,
                staging: staging.clone(),
                scratch: scratch.clone(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 32,
        height: 16,
    };
    let previous = gpu.create_surface_image(size).unwrap();
    assert_eq!(scratch.available(), 0);
    drop(previous);
    gpu.maintain().unwrap();
    let lock = staging.reserve(staging.available()).unwrap();
    let reused = gpu.create_surface_image(size).unwrap();
    assert_eq!(scratch.available(), 0);
    drop(lock);
    assert!(pixels(&gpu, &reused).iter().all(|&v| v == 0));
}

#[test]
fn full_repaints_fit_a_single_canvas_without_extra_surface_headroom() {
    let context = support::Context::new();
    let size = Size {
        width: 32,
        height: 16,
    };
    // Exactly one work renderbuffer and one display texture; the previous
    // allocate-then-replace path cannot admit a second frame in this budget.
    let scratch = Budget::new(32 * 32 * 4 + size.rgba_bytes().unwrap());
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                tile_edge: 32,
                scratch: scratch.clone(),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let logical = Size {
        width: 64,
        height: 32,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let mut root = node(&reference, logical, None, Blend::Opaque);
    root.image = None;
    let mut scene = Scene {
        nodes: vec![root],
        ..Default::default()
    };
    let (mut canvas, mut state) = (None, SceneState::default());
    for color in [0x123456, 0xabcdef, 0x543210, 0x090807] {
        scene.nodes[0].neutral_color = color;
        assert_eq!(
            gpu.update_scene_surface(
                &mut canvas,
                &mut state,
                logical,
                size,
                &scene,
                &HashMap::new()
            )
            .unwrap(),
            Some(size.rect())
        );
        assert_eq!(scratch.available(), 0);
        let mut stored = canvas.as_ref().unwrap().shared();
        stored.size = size;
        let expected = [(color >> 16) as u8, (color >> 8) as u8, color as u8, 255];
        assert!(
            pixels(&gpu, &stored)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == expected)
        );
    }
}

#[test]
fn retained_full_canvas_matches_fresh_transition_frames_and_preserves_shared_captures() {
    use krkr_protocol::transition::{Direction, Effect, Frame, SceneTransition, Stay};
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
    let logical = Size {
        width: 97,
        height: 65,
    };
    let physical = Size {
        width: 49,
        height: 33,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let refs: Vec<_> = (0..5)
        .map(|_| ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        })
        .collect();
    let mut images = HashMap::from([
        (refs[0].id, gpu.create_image(logical, 0xc1345678).unwrap()),
        (refs[1].id, gpu.create_image(logical, 0xa3abcdef).unwrap()),
        (refs[2].id, gpu.create_image(logical, 0x91573184).unwrap()),
        (refs[3].id, gpu.create_image(logical, 0xff808080).unwrap()),
        (refs[4].id, gpu.create_image(logical, 0xa0803010).unwrap()),
    ]);
    let mut scene = Scene {
        nodes: vec![
            node(&refs[0], logical, None, Blend::Alpha),
            node(&refs[2], logical, Some(0), Blend::AddAlpha),
            node(&refs[1], logical, None, Blend::Alpha),
            node(&refs[2], logical, Some(2), Blend::Alpha),
            node(&refs[4], logical, None, Blend::Alpha),
        ],
        ..Default::default()
    };
    scene.nodes[0].opacity = 197;
    scene.nodes[1].rectangle.left = 11;
    scene.nodes[1].rectangle.top = -7;
    scene.nodes[2].visible = false;
    scene.nodes[4].rectangle = Rect {
        left: 30,
        top: 20,
        width: 12,
        height: 8,
    };
    let (mut canvas, mut state) = (None, SceneState::default());
    let mut edit = 0u32;
    for effect in [
        Effect::CrossFade,
        Effect::Universal { vague: 63 },
        Effect::Scroll {
            from: Direction::Left,
            stay: Stay::Neither,
        },
    ] {
        for children in [false, true] {
            for phase in [0, effect.phases(logical) / 2, effect.phases(logical)] {
                scene.transitions = vec![SceneTransition {
                    destination: 0,
                    source: 2,
                    with_children: children,
                    frame: Frame {
                        effect,
                        face: DrawFace::Alpha,
                        size: logical,
                        phase,
                    },
                    rule: Some(refs[3].clone()),
                    custom: None,
                }];
                // Retain every other frame to exercise copy-on-write as well
                // as the normal host-owned, unique display surface.
                let old = (phase == 0)
                    .then(|| canvas.as_ref().map(Image::shared))
                    .flatten();
                let before = old.as_ref().map(|image| pixels(&gpu, image));
                assert_eq!(
                    check(
                        &gpu,
                        &mut canvas,
                        &mut state,
                        logical,
                        physical,
                        &scene,
                        &images
                    ),
                    Some(physical.rect())
                );
                if let (Some(old), Some(before)) = (old, before) {
                    assert_eq!(pixels(&gpu, &old), before);
                }
                assert_eq!(
                    check(
                        &gpu,
                        &mut canvas,
                        &mut state,
                        logical,
                        physical,
                        &scene,
                        &images
                    ),
                    None
                );
                if phase == effect.phases(logical) / 2 {
                    scene.nodes[4].opacity = if scene.nodes[4].opacity == 255 {
                        91
                    } else {
                        255
                    };
                    let damage = check(
                        &gpu,
                        &mut canvas,
                        &mut state,
                        logical,
                        physical,
                        &scene,
                        &images,
                    )
                    .unwrap();
                    assert!(damage.width < physical.width && damage.height < physical.height);

                    // The source parent is hidden. Its child visibility/size,
                    // source pixels and independent rule pixels still matter.
                    for change in 0..4 {
                        match change {
                            0 | 3 => {
                                edit += 1;
                                let id = if change == 0 { refs[1].id } else { refs[3].id };
                                gpu.fill(
                                    images.get_mut(&id).unwrap(),
                                    &[Fill {
                                        rectangle: logical.rect(),
                                        color: 0xff102030 + edit * 257,
                                        face: DrawFace::Alpha,
                                        hold_alpha: false,
                                    }],
                                )
                                .unwrap();
                            }
                            1 => scene.nodes[3].visible = !scene.nodes[3].visible,
                            _ => scene.nodes[3].rectangle.width -= 1,
                        }
                        assert_eq!(
                            check(
                                &gpu,
                                &mut canvas,
                                &mut state,
                                logical,
                                physical,
                                &scene,
                                &images
                            ),
                            (children || change == 0 || change == 3).then_some(physical.rect())
                        );
                        assert_eq!(
                            check(
                                &gpu,
                                &mut canvas,
                                &mut state,
                                logical,
                                physical,
                                &scene,
                                &images
                            ),
                            None
                        );
                    }
                }
                gpu.maintain().unwrap();
            }
        }
    }
    scene.transitions.clear();
    scene.nodes.clear();
    check(
        &gpu,
        &mut canvas,
        &mut state,
        logical,
        physical,
        &scene,
        &images,
    );
}

#[test]
fn small_damage_crops_the_existing_static_group_cache() {
    for explicit_cache in [false, true] {
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
            width: 128,
            height: 96,
        };
        let mut ids = slotmap::SlotMap::with_key();
        let background = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let text = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let foreground = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let mut images = HashMap::from([
            (background.id, gpu.create_image(size, 0xff234567).unwrap()),
            (text.id, gpu.create_image(size, 0).unwrap()),
            (foreground.id, gpu.create_image(size, 0x753291e7).unwrap()),
        ]);
        let mut group = node(&foreground, size, Some(0), Blend::Alpha);
        group.opacity = 137;
        group.cache = explicit_cache.then(|| Arc::new(()));
        let mut child = node(
            &foreground,
            Size {
                width: 11,
                height: 9,
            },
            Some(2),
            Blend::Alpha,
        );
        child.rectangle.left = 17;
        child.rectangle.top = 13;
        let mut scene = Scene {
            nodes: vec![
                node(&background, size, None, Blend::Opaque),
                node(&text, size, Some(0), Blend::Alpha),
                group,
                child,
            ],
            ..Default::default()
        };
        let (mut canvas, mut state) = (None, SceneState::default());
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
        gpu.maintain().unwrap();
        let resident = gpu.resident.used();
        for (x, y) in [(18, 15), (53, 47), (107, 73)] {
            gpu.fill(
                images.get_mut(&text.id).unwrap(),
                &[Fill {
                    rectangle: Rect {
                        left: x,
                        top: y,
                        width: 5,
                        height: 7,
                    },
                    color: 0xffebdbca,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
            let damage = check(&gpu, &mut canvas, &mut state, size, size, &scene, &images).unwrap();
            assert_eq!((damage.width, damage.height), (16, 16));
            assert_eq!(
                gpu.resident.used(),
                resident,
                "static group was rebuilt for a smaller repaint"
            );
        }
        // Parent opacity is applied after the cached group. Changing the
        // child's geometry or source pixels must invalidate its completed raster.
        scene.nodes[2].opacity = 83;
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
        scene.nodes[3].rectangle.left += 9;
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
        gpu.fill(
            images.get_mut(&foreground.id).unwrap(),
            &[Fill {
                rectangle: size.rect(),
                color: 0xb1d96743,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
    }
}

#[test]
fn write_history_overflow_and_failed_scene_validation_are_conservative() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 64,
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
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let mut images = HashMap::from([(reference.id, gpu.create_image(size, 0xff102030).unwrap())]);
    let mut scene = Scene {
        nodes: vec![node(&reference, size, None, Blend::Opaque)],
        ..Default::default()
    };
    let (mut canvas, mut state) = (None, SceneState::default());
    check(&gpu, &mut canvas, &mut state, size, size, &scene, &images);
    let before = pixels(&gpu, canvas.as_ref().unwrap());
    scene.nodes[0].parent = Some(0);
    assert!(
        gpu.update_scene_surface(&mut canvas, &mut state, size, size, &scene, &images)
            .is_err()
    );
    assert_eq!(pixels(&gpu, canvas.as_ref().unwrap()), before);
    scene.nodes[0].parent = None;
    for x in 0..20 {
        gpu.fill(
            images.get_mut(&reference.id).unwrap(),
            &[Fill {
                rectangle: Rect {
                    left: x,
                    top: 5,
                    width: 1,
                    height: 2,
                },
                color: 0xffabcdef,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
    }
    assert_eq!(
        check(&gpu, &mut canvas, &mut state, size, size, &scene, &images),
        Some(size.rect())
    );
}

#[test]
fn patterned_buttons_keep_offsets_and_colors_across_fractional_scale_updates() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    for work in [false, true] {
        let context = support::Context::new();
        let logical = Size {
            width: 193,
            height: 109,
        };
        let physical = Size {
            width: 97,
            height: 55,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 32,
                    canvas_limit: Some(physical),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(logical);
        let mut ids = slotmap::SlotMap::with_key();
        let reference = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let stored = Size {
            width: 63,
            height: 27,
        };
        let mut bytes = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
        for (i, pixel) in bytes
            .as_mut_slice()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
        {
            let (x, y) = (i % 63, i / 63);
            pixel.copy_from_slice(&[
                (x * 17) as u8,
                (y * 23) as u8,
                (x * 7 + y * 11) as u8,
                (x * 13 + y * 19) as u8,
            ]);
        }
        let mut bitmap = gpu.reserve_upload(stored, true, false).unwrap();
        gpu.upload(
            &mut bitmap,
            &Pixels {
                size: stored,
                main: Some(bytes),
                province: None,
            },
        )
        .unwrap();
        let bitmap = gpu
            .logical_image(
                bitmap,
                Size {
                    width: 125,
                    height: 53,
                },
            )
            .unwrap();
        let images = HashMap::from([(reference.id, bitmap)]);
        let mut root = node(&reference, logical, None, Blend::Opaque);
        root.image = None;
        root.neutral_color = 0x26405a;
        let mut group = node(
            &reference,
            Size {
                width: 157,
                height: 89,
            },
            Some(0),
            Blend::Alpha,
        );
        group.image = None;
        group.rectangle.left = 7;
        group.rectangle.top = 3;
        group.opacity = 213;
        let mut scene = Scene {
            nodes: vec![root, group],
            ..Default::default()
        };
        for i in 0..3 {
            let mut button = node(
                &reference,
                Size {
                    width: 95 - i * 11,
                    height: 29,
                },
                Some(1),
                Blend::Alpha,
            );
            button.rectangle.left = 2 + i as i32 * 9;
            button.rectangle.top = 1 + i as i32 * 23;
            button.image_left = -13 - i as i32;
            button.image_top = -5;
            scene.nodes.push(button);
        }
        let (mut canvas, mut state) = (None, SceneState::default());
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        for i in 0..48 {
            let button = &mut scene.nodes[2 + i % 3];
            button.opacity = [255, 127, 63, 239][i % 4];
            button.rectangle.left = (i % 31) as i32 - 7;
            button.image_top = -((i % 17) as i32);
            button.visible = i % 7 != 0;
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images,
            );
            gpu.maintain().unwrap();
        }
    }
}

/// A retained display canvas stores physical pixels while keeping logical script
/// coordinates. Updating one damage region in place must not reallocate that
/// storage to the logical size: the region is cleared and rebuilt in physical
/// coordinates, so a materialized canvas moves the drawn pixels.
#[test]
fn partial_repaint_keeps_compact_display_canvas_storage_and_placement() {
    for work in [false, true] {
        let context = support::Context::new();
        let logical = Size {
            width: 193,
            height: 109,
        };
        let physical = Size {
            width: 97,
            height: 55,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 32,
                    canvas_limit: Some(physical),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(logical);
        let mut ids = slotmap::SlotMap::with_key();
        let reference = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        let mut images =
            HashMap::from([(reference.id, gpu.create_image(logical, 0xff204060).unwrap())]);
        gpu.fill(
            images.get_mut(&reference.id).unwrap(),
            &[Fill {
                rectangle: Rect {
                    left: 100,
                    top: 60,
                    width: 4,
                    height: 4,
                },
                color: 0xff345678,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let scene = Scene {
            nodes: vec![node(&reference, logical, None, Blend::Opaque)],
            ..Default::default()
        };
        let (mut canvas, mut state) = (None, SceneState::default());
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
        assert_eq!(
            canvas.as_ref().unwrap().stored_size(),
            Some(physical),
            "work={work}: composed canvas must keep display-density storage"
        );
        let area = Rect {
            left: 40,
            top: 20,
            width: 3,
            height: 2,
        };
        gpu.fill(
            images.get_mut(&reference.id).unwrap(),
            &[Fill {
                rectangle: area,
                color: 0xffb06020,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let damage = gpu
            .update_scene_surface(&mut canvas, &mut state, logical, physical, &scene, &images)
            .unwrap()
            .unwrap();
        assert_ne!(
            damage,
            physical.rect(),
            "work={work}: expected a partial damage region"
        );
        assert_eq!(
            canvas.as_ref().unwrap().stored_size(),
            Some(physical),
            "work={work}: a partial repaint must not materialize the compact canvas"
        );
        // Placement is only correct when the rebuilt region kept the shared
        // logical/physical grid; compare against one full composition.
        check(
            &gpu,
            &mut canvas,
            &mut state,
            logical,
            physical,
            &scene,
            &images,
        );
    }
}

#[test]
fn display_filter_preserves_thin_pixels_across_tiles_and_compact_images() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    for work in [false, true] {
        let context = support::Context::new();
        let logical = Size {
            width: 136,
            height: 88,
        };
        let physical = Size {
            width: 102,
            height: 66,
        };
        let size = Size {
            width: 128,
            height: 72,
        };
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
        let mut ids = slotmap::SlotMap::with_key();
        let reference = ImageRef {
            id: ids.insert(()),
            lifetime: Arc::default(),
        };
        for stored in [
            size,
            Size {
                width: 96,
                height: 54,
            },
        ] {
            let mut bytes = Bytes::zeroed(stored.rgba_bytes().unwrap(), &gpu.staging).unwrap();
            for (i, pixel) in bytes
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                let (x, y) = (i as u32 % stored.width, i as u32 / stored.width);
                let level = if x % 4 == 1 || y % 5 == 1 { 255 } else { 0 };
                pixel.copy_from_slice(&[level, level, level, 255]);
            }
            let input = bytes.as_slice().to_vec();
            let mut bitmap = gpu.reserve_upload(stored, true, false).unwrap();
            gpu.upload(
                &mut bitmap,
                &Pixels {
                    size: stored,
                    main: Some(bytes),
                    province: None,
                },
            )
            .unwrap();
            let mut images =
                HashMap::from([(reference.id, gpu.logical_image(bitmap, size).unwrap())]);
            for (left, top) in [(0, 0), (1, 1), (3, 5)] {
                let mut leaf = node(&reference, size, None, Blend::Opaque);
                leaf.rectangle.left = left;
                leaf.rectangle.top = top;
                let scene = Scene {
                    nodes: vec![leaf],
                    ..Default::default()
                };
                traffic::reset();
                let output = gpu
                    .scene_surface_scaled(logical, physical, &scene, &images)
                    .unwrap();
                assert_eq!(
                    traffic::read_calls(),
                    0,
                    "display filtering read pixels back to the CPU"
                );
                let actual = pixels(&gpu, &output);
                let sample = |x: i32, y: i32| {
                    let x = x.clamp(0, stored.width as i32 - 1) as usize;
                    let y = y.clamp(0, stored.height as i32 - 1) as usize;
                    f64::from(input[(y * stored.width as usize + x) * 4])
                };
                for y in 0..physical.height {
                    for x in 0..physical.width {
                        let lx = (f64::from(x) + 0.5) * f64::from(logical.width)
                            / f64::from(physical.width)
                            - f64::from(left);
                        let ly = (f64::from(y) + 0.5) * f64::from(logical.height)
                            / f64::from(physical.height)
                            - f64::from(top);
                        if lx < 0.
                            || lx >= f64::from(size.width)
                            || ly < 0.
                            || ly >= f64::from(size.height)
                        {
                            continue;
                        }
                        let sx = lx * f64::from(stored.width) / f64::from(size.width) - 0.5;
                        let sy = ly * f64::from(stored.height) / f64::from(size.height) - 0.5;
                        let (ix, iy) = (sx.floor() as i32, sy.floor() as i32);
                        let (fx, fy) = (sx - sx.floor(), sy - sy.floor());
                        let a = sample(ix, iy) * (1. - fx) + sample(ix + 1, iy) * fx;
                        let b = sample(ix, iy + 1) * (1. - fx) + sample(ix + 1, iy + 1) * fx;
                        let expected = (a * (1. - fy) + b * fy).round() as u8;
                        let value = actual[((y * physical.width + x) * 4) as usize];
                        assert!(
                            value.abs_diff(expected) <= 1,
                            "work={work} stored={stored:?} offset=({left},{top}) pixel=({x},{y}) got={value} expected={expected}"
                        );
                    }
                }
            }
            // A single source-pixel edit also affects its filtered neighbours.
            // Compare retained updates to full redraws across both tile axes.
            let scene = Scene {
                nodes: vec![node(&reference, size, None, Blend::Opaque)],
                ..Default::default()
            };
            let (mut canvas, mut state) = (None, SceneState::default());
            check(
                &gpu,
                &mut canvas,
                &mut state,
                logical,
                physical,
                &scene,
                &images,
            );
            for (x, y) in [(15, 15), (16, 16), (31, 32), (65, 47)] {
                gpu.fill(
                    images.get_mut(&reference.id).unwrap(),
                    &[Fill {
                        rectangle: Rect {
                            left: x,
                            top: y,
                            width: 1,
                            height: 1,
                        },
                        color: 0xffff8040,
                        face: DrawFace::Alpha,
                        hold_alpha: false,
                    }],
                )
                .unwrap();
                check(
                    &gpu,
                    &mut canvas,
                    &mut state,
                    logical,
                    physical,
                    &scene,
                    &images,
                );
            }
        }
    }
}

#[test]
fn display_filter_does_not_bleed_invisible_rgb_into_alpha_edges() {
    use krkr_protocol::pixels::{Bytes, Pixels};
    let context = support::Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 2,
        height: 1,
    };
    let physical = Size {
        width: 1,
        height: 1,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let bg = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let fg = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    for blend in [Blend::Alpha, Blend::AddAlpha] {
        let mut bytes = Bytes::zeroed(8, &gpu.staging).unwrap();
        bytes
            .as_mut_slice()
            .copy_from_slice(if blend == Blend::Alpha {
                &[255, 255, 255, 255, 255, 0, 255, 0]
            } else {
                &[255, 255, 255, 255, 0, 0, 0, 0]
            });
        let mut bitmap = gpu.reserve_upload(size, true, false).unwrap();
        gpu.upload(
            &mut bitmap,
            &Pixels {
                size,
                main: Some(bytes),
                province: None,
            },
        )
        .unwrap();
        let images = HashMap::from([
            (bg.id, gpu.create_image(size, 0xff000000).unwrap()),
            (fg.id, bitmap),
        ]);
        let scene = Scene {
            nodes: vec![
                node(&bg, size, None, Blend::Opaque),
                node(&fg, size, None, blend),
            ],
            ..Default::default()
        };
        let output = gpu
            .scene_surface_scaled(size, physical, &scene, &images)
            .unwrap();
        let value = pixels(&gpu, &output);
        assert_eq!(
            value[0], value[1],
            "{blend:?}: transparent magenta bled into the edge"
        );
        assert_eq!(value[1], value[2]);
        assert!(
            (126..=128).contains(&value[0]),
            "{blend:?}: alpha was applied twice: {value:?}"
        );
    }
}

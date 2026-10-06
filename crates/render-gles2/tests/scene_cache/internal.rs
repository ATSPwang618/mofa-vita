use super::*;
use crate::{Config, Gpu, test_support::Context};
use krkr_protocol::graphics::{DrawFace, Fill, ImageRef, Node, Size};
#[path = "allocations.rs"]
mod allocations;

#[test]
fn borrowed_queries_allocate_nothing_and_capture_changed_tails() {
    let context = Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 960,
        height: 544,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let mut images = HashMap::from([(
        reference.id,
        gpu.create_image(
            Size {
                width: 24,
                height: 24,
            },
            0x807f594b,
        )
        .unwrap(),
    )]);
    let raster = Raster::new(size, size, (0, 0)).unwrap();
    let mut scene = paragraph(&reference, 512, size);
    let children = Children::new(&scene.nodes, 127).unwrap();
    let signature = Signature::capture(
        &scene,
        &images,
        &children,
        0,
        raster,
        (0, 0),
        size.rect(),
        false,
    )
    .unwrap();
    let mut cache = Cache::default();
    cache.insert(Entry {
        owner: scene.nodes[0].cache.as_ref().map(Arc::downgrade),
        _metadata: gpu.resident.reserve(signature.bytes()).unwrap(),
        signature,
        image: gpu.create_image(size, 0).unwrap(),
        used: false,
    });
    for opacity in [1, 64, 127, 254, 255] {
        scene.nodes[0].opacity = opacity;
        let query = Query {
            scene: &scene,
            images: &images,
            children: &children,
            root: 0,
            raster,
            origin: (0, 0),
            region: size.rect(),
            content: false,
        };
        let (result, count) =
            allocations::measure(|| cache.lookup(scene.nodes[0].cache.as_ref(), &query));
        assert!(matches!(result, Lookup::Hit(..)));
        assert_eq!(count, 0, "unchanged paragraph lookup allocated");
    }
    scene.nodes.last_mut().unwrap().opacity = 93;
    let query = Query {
        scene: &scene,
        images: &images,
        children: &children,
        root: 0,
        raster,
        origin: (0, 0),
        region: size.rect(),
        content: false,
    };
    let (result, count) =
        allocations::measure(|| cache.lookup(scene.nodes[0].cache.as_ref(), &query));
    assert!(matches!(result, Lookup::Miss(Some(_))));
    assert_eq!(
        count, 1,
        "a changed single-tile tail needs one signature allocation"
    );
    scene.nodes.last_mut().unwrap().opacity = 255;
    // Pixel writes must invalidate the fast path even with identical nodes.
    gpu.fill(
        images.get_mut(&reference.id).unwrap(),
        &[Fill {
            rectangle: Rect {
                width: 2,
                height: 2,
                ..size.rect()
            },
            color: 0xff123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let query = Query {
        scene: &scene,
        images: &images,
        children: &children,
        root: 0,
        raster,
        origin: (0, 0),
        region: size.rect(),
        content: false,
    };
    assert!(matches!(
        cache.lookup(scene.nodes[0].cache.as_ref(), &query),
        Lookup::Miss(Some(_))
    ));
}

#[test]
fn borrowed_signature_matches_full_capture_after_tree_edits() {
    let size = Size {
        width: 960,
        height: 544,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    // Geometry alone is enough to exercise traversal, culling and order.
    let mut original = paragraph(&reference, 16, size);
    for node in &mut original.nodes {
        node.image = None;
    }
    let images = HashMap::new();
    let raster = Raster::new(size, size, (0, 0)).unwrap();
    let topology = Children::new(&original.nodes, 127).unwrap();
    let old = Signature::capture(
        &original,
        &images,
        &topology,
        0,
        raster,
        (0, 0),
        size.rect(),
        false,
    )
    .unwrap();
    for case in 0..12 {
        let mut scene = Scene {
            nodes: original.nodes.clone(),
            ..Default::default()
        };
        let mut origin = (0, 0);
        match case {
            0 => scene.nodes[0].opacity = 1,
            1 => scene.nodes[2].opacity = 1,
            2 => scene.nodes[2].visible = false,
            3 => scene.nodes[2].rectangle.left += 1,
            4 => {
                scene.nodes.pop();
            }
            5 => {
                scene.nodes.push(scene.nodes.last().unwrap().clone());
            }
            6 => scene.nodes.swap(2, 3),
            7 => scene.nodes[2].parent = Some(1),
            8 => scene.nodes[2].neutral_color = 0xabcdef,
            9 => scene.nodes[2].rectangle.left = 2000,
            10 => scene.nodes[0].rectangle.width = 48,
            11 => origin = (-24, -12),
            _ => unreachable!(),
        }
        let topology = Children::new(&scene.nodes, 127).unwrap();
        let query = Query {
            scene: &scene,
            images: &images,
            children: &topology,
            root: 0,
            raster,
            origin,
            region: size.rect(),
            content: false,
        };
        let compared = old.compare(&query).unwrap();
        assert_eq!(compared.is_none(), case == 0, "case {case}");
        let actual = compared.as_ref().map_or(&old, |change| &change.signature);
        let full = query.capture().unwrap();
        assert_eq!(actual.parts.len(), full.parts.len(), "case {case}");
        assert!(
            actual
                .parts
                .iter()
                .zip(&full.parts)
                .all(|(a, b)| a.same_geometry(b)),
            "case {case}"
        );
        assert!(actual.damage(&full, size.rect()).is_none(), "case {case}");
    }
    let mut hidden = original.nodes.last().unwrap().clone();
    hidden.visible = false;
    original.nodes.extend(std::iter::repeat_n(hidden, 2048));
    let topology = Children::new(&original.nodes, 127).unwrap();
    let query = Query {
        scene: &original,
        images: &images,
        children: &topology,
        root: 0,
        raster,
        origin: (0, 0),
        region: size.rect(),
        content: false,
    };
    assert!(
        old.compare(&query).unwrap().is_none(),
        "hidden siblings cannot consume the part limit"
    );
}

fn paragraph(reference: &ImageRef, count: usize, size: Size) -> Scene {
    let root = Node {
        cache: Some(Arc::new(())),
        parent: None,
        visible: true,
        image: None,
        neutral_color: 0,
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Alpha,
        opacity: 127,
    };
    let mut nodes = vec![root.clone()];
    for i in 0..count {
        nodes.push(Node {
            cache: None,
            parent: Some(0),
            image: Some(reference.clone()),
            rectangle: Rect {
                left: (i % 40) as i32 * 24,
                top: (i / 40) as i32 * 24,
                width: 24,
                height: 24,
            },
            opacity: 255,
            ..root.clone()
        });
    }
    Scene {
        viewport: Default::default(),
        requires_op_seq: 0,
        nodes,
        transitions: Vec::new(),
    }
}

#[test]
fn changed_prefix_proof_stays_with_its_raster_when_an_older_entry_is_removed() {
    let context = Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 960,
        height: 544,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let images = HashMap::new();
    let raster = Raster::new(size, size, (0, 0)).unwrap();
    let mut cache = Cache::default();
    let mut newest_plane = None;
    for version in 0..3 {
        let mut scene = paragraph(&reference, 4, size);
        for node in &mut scene.nodes {
            node.image = None;
        }
        if version == 0 {
            scene.nodes[0].rectangle.width = 95;
        }
        if version == 1 {
            scene.nodes[1].opacity = 83;
            scene.nodes[4].opacity = 93;
        }
        let children = Children::new(&scene.nodes, 127).unwrap();
        let region = Size {
            width: if version == 0 { 95 } else { 96 },
            height: 64 - version * 16,
        };
        let signature = Signature::capture(
            &scene,
            &images,
            &children,
            0,
            raster,
            (0, 0),
            region.rect(),
            false,
        )
        .unwrap();
        let image = gpu.create_image(region, 0xff123450 + version).unwrap();
        if version == 2 {
            newest_plane = image.main.clone();
        }
        cache.insert(Entry {
            owner: None,
            _metadata: gpu.resident.reserve(signature.bytes()).unwrap(),
            signature,
            image,
            used: false,
        });
    }
    assert_eq!(cache.entries.len(), 3);
    let mut scene = paragraph(&reference, 4, size);
    for node in &mut scene.nodes {
        node.image = None;
    }
    scene.nodes[4].opacity = 93;
    let children = Children::new(&scene.nodes, 127).unwrap();
    let mut query = Query {
        scene: &scene,
        images: &images,
        children: &children,
        root: 0,
        raster,
        origin: (0, 0),
        region: Size {
            width: 96,
            height: 32,
        }
        .rect(),
        content: false,
    };
    assert!(matches!(cache.lookup(None, &query), Lookup::Miss(Some(_))));
    assert_eq!(cache.entries.len(), 2);
    query.region = Size {
        width: 24,
        height: 24,
    }
    .rect();
    let Lookup::Hit(image, _) = cache.lookup(None, &query) else {
        panic!("first child remains unchanged in newest raster")
    };
    assert!(std::rc::Rc::ptr_eq(
        image.main.as_ref().unwrap(),
        newest_plane.as_ref().unwrap()
    ));
}

#[test]
fn repair_charges_the_smaller_signature_until_its_entry_drops() {
    let context = Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 32,
        height: 32,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let mut scene = paragraph(&reference, 1, size);
    scene.nodes[1].image = None;
    let images = HashMap::new();
    let children = Children::new(&scene.nodes, 127).unwrap();
    let raster = Raster::new(size, size, (0, 0)).unwrap();
    let signature = Signature::capture(
        &scene,
        &images,
        &children,
        0,
        raster,
        (0, 0),
        size.rect(),
        false,
    )
    .unwrap();
    let capacity = signature.parts.capacity();
    let metadata = Budget::new(LIMIT);
    let mut image = gpu.create_image(size, 0).unwrap();
    gpu.fill(
        &mut image,
        &[Fill {
            rectangle: Rect {
                width: 1,
                height: 1,
                ..size.rect()
            },
            color: 0xff123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let mut cache = Cache::default();
    cache.insert(Entry {
        owner: scene.nodes[0].cache.as_ref().map(Arc::downgrade),
        _metadata: metadata.reserve(signature.bytes()).unwrap(),
        signature,
        image,
        used: false,
    });
    let before = metadata.used();
    scene.nodes[1].opacity = 73;
    let query = Query {
        scene: &scene,
        images: &images,
        children: &children,
        root: 0,
        raster,
        origin: (0, 0),
        region: size.rect(),
        content: false,
    };
    let Lookup::Miss(Some(mut signature)) = cache.lookup(scene.nodes[0].cache.as_ref(), &query)
    else {
        panic!("changed child must miss")
    };
    assert_eq!(
        before - signature.bytes(),
        (capacity - signature.parts.capacity()) * std::mem::size_of::<Part>()
    );
    let (mut entry, _, _) = cache
        .take_dirty(
            scene.nodes[0].cache.as_ref(),
            &mut signature,
            size.rect(),
            size.rect(),
            LIMIT,
            &metadata,
        )
        .unwrap();
    assert_eq!(metadata.used(), signature.bytes());
    entry.signature = signature;
    cache.insert(entry);
    assert!(metadata.used() > 0);
    cache.clear();
    assert_eq!(metadata.used(), 0);
}

#[test]
fn endpoint_queries_exclude_only_the_root_transition_and_bound_deep_trees() {
    use krkr_protocol::transition::{Effect, SceneTransition};
    let size = Size {
        width: 32,
        height: 32,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let mut scene = paragraph(&reference, 20, size);
    for (i, node) in scene.nodes.iter_mut().enumerate() {
        node.image = None;
        node.rectangle = size.rect();
        node.parent = i.checked_sub(1);
    }
    let children = Children::new(&scene.nodes, 127).unwrap();
    let raster = Raster::new(size, size, (0, 0)).unwrap();
    let images = HashMap::new();
    let old = Signature::capture(
        &scene,
        &images,
        &children,
        0,
        raster,
        (0, 0),
        size.rect(),
        true,
    )
    .unwrap();
    assert_eq!(old.parts.len(), 21);
    scene.nodes[0].visible = false;
    scene.nodes[0].opacity = 0;
    scene.transitions.push(SceneTransition {
        destination: 0,
        source: 1,
        with_children: true,
        frame: Effect::CrossFade.frame(DrawFace::Alpha, size, 50, 100),
        rule: None,
        custom: None,
    });
    let query = Query {
        scene: &scene,
        images: &images,
        children: &children,
        root: 0,
        raster,
        origin: (0, 0),
        region: size.rect(),
        content: true,
    };
    assert!(old.compare(&query).unwrap().is_none());
    scene.transitions[0].destination = 1;
    let query = Query {
        scene: &scene,
        images: &images,
        children: &children,
        root: 0,
        raster,
        origin: (0, 0),
        region: size.rect(),
        content: true,
    };
    assert!(old.compare(&query).is_none());
    assert!(query.capture().is_none());
    let mut wide = paragraph(&reference, MAX_PARTS, size);
    for node in &mut wide.nodes {
        node.image = None;
        node.rectangle = size.rect();
    }
    let children = Children::new(&wide.nodes, 127).unwrap();
    assert!(
        Signature::capture(
            &wide,
            &images,
            &children,
            0,
            raster,
            (0, 0),
            size.rect(),
            false
        )
        .is_none()
    );
}

#[test]
#[ignore = "manual CPU measurement of retained subtree cache queries"]
fn subtree_cache_query_workload() {
    use std::{hint::black_box, time::Instant};
    let context = Context::new();
    let gpu = unsafe { Gpu::new(context.gl(), Config::default()).unwrap() };
    let size = Size {
        width: 960,
        height: 544,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let images = HashMap::from([(
        reference.id,
        gpu.create_image(
            Size {
                width: 24,
                height: 24,
            },
            0x807f594b,
        )
        .unwrap(),
    )]);
    let raster = Raster::new(size, size, (0, 0)).unwrap();
    for count in [32, 256, 512] {
        let scene = paragraph(&reference, count, size);
        let children = Children::new(&scene.nodes, 127).unwrap();
        let capture = || {
            Signature::capture(
                &scene,
                &images,
                &children,
                0,
                raster,
                (0, 0),
                size.rect(),
                false,
            )
            .unwrap()
        };
        let signature = capture();
        let mut cache = Cache::default();
        cache.insert(Entry {
            owner: scene.nodes[0].cache.as_ref().map(Arc::downgrade),
            _metadata: gpu.resident.reserve(signature.bytes()).unwrap(),
            signature,
            image: gpu.create_image(size, 0).unwrap(),
            used: false,
        });
        let mut scene = scene;
        for changing in [false, true] {
            for direct in [false, true] {
                for _ in 0..3 {
                    let start = Instant::now();
                    for tick in 0..10_000 {
                        scene.nodes.last_mut().unwrap().opacity = if changing {
                            100 + (tick % 100) as u8
                        } else {
                            255
                        };
                        if direct {
                            let query = Query {
                                scene: &scene,
                                images: &images,
                                children: &children,
                                root: 0,
                                raster,
                                origin: (0, 0),
                                region: size.rect(),
                                content: false,
                            };
                            match black_box(cache.lookup(scene.nodes[0].cache.as_ref(), &query)) {
                                Lookup::Hit(image, area) => {
                                    black_box((image, area));
                                }
                                Lookup::Miss(Some(signature)) if changing => {
                                    black_box(signature);
                                }
                                _ => panic!("unexpected cache result"),
                            }
                        } else {
                            let signature = black_box(
                                Signature::capture(
                                    &scene,
                                    &images,
                                    &children,
                                    0,
                                    raster,
                                    (0, 0),
                                    size.rect(),
                                    false,
                                )
                                .unwrap(),
                            );
                            let hit =
                                black_box(cache.get(scene.nodes[0].cache.as_ref(), &signature));
                            assert_eq!(hit.is_none(), changing);
                        }
                    }
                    eprintln!(
                        "subtree-query nodes={count} changing={changing} borrowed={direct} ns/op={:.1}",
                        start.elapsed().as_nanos() as f64 / 10_000.0
                    );
                }
            }
        }
    }
}

#[test]
fn region_cache_keeps_unmodified_pixels_across_expired_cow_versions() {
    for cow in [false, true] {
        let context = Context::new();
        let logical = Size {
            width: 192,
            height: 128,
        };
        let physical = Size {
            width: 96,
            height: 64,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: true,
                    tile_edge: 64,
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
            HashMap::from([(reference.id, gpu.create_image(logical, 0x937f594b).unwrap())]);
        // Materialize the initially uniform canvas before measuring retained
        // crops across later COW writes. Its first edit changes storage density.
        gpu.fill(
            images.get_mut(&reference.id).unwrap(),
            &[Fill {
                rectangle: Rect {
                    left: 180,
                    top: 120,
                    width: 2,
                    height: 2,
                },
                color: 0xff123456,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        let root = Node {
            parent: None,
            cache: Some(Arc::new(())),
            visible: true,
            opacity: 171,
            image: None,
            neutral_color: 0,
            rectangle: logical.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Alpha,
        };
        let mut child = root.clone();
        child.parent = Some(0);
        child.cache = None;
        child.opacity = 255;
        child.image = Some(reference.clone());
        child.rectangle = Rect {
            left: 12,
            top: 8,
            width: 168,
            height: 112,
        };
        child.image_left = -4;
        child.image_top = 6;
        let scene = Scene {
            nodes: vec![root.clone(), child],
            ..Default::default()
        };
        let children = Children::new(&scene.nodes, 127).unwrap();
        let raster = Raster::new(logical, physical, (0, 0)).unwrap();
        let signature = |region| {
            Signature::capture(&scene, &images, &children, 0, raster, (0, 0), region, false)
                .unwrap()
        };
        drop(
            gpu.scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap(),
        );
        let original = gpu
            .scene_cache
            .borrow_mut()
            .get(root.cache.as_ref(), &signature(physical.rect()))
            .unwrap()
            .0;
        let previous =
            std::rc::Rc::downgrade(&images[&reference.id].main.as_ref().unwrap().tiles[0].texture);
        let pinned = cow.then(|| images[&reference.id].shared());
        gpu.fill(
            images.get_mut(&reference.id).unwrap(),
            &[Fill {
                rectangle: Rect {
                    left: 100,
                    top: 60,
                    width: 8,
                    height: 8,
                },
                color: 0xd123a579,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )
        .unwrap();
        drop(pinned);
        if cow {
            assert!(previous.upgrade().is_none());
        }
        gpu.maintain().unwrap();
        let query = |region| Query {
            scene: &scene,
            images: &images,
            children: &children,
            root: 0,
            raster,
            origin: (0, 0),
            region,
            content: false,
        };
        let clean = Rect {
            left: 14,
            top: 12,
            width: 8,
            height: 8,
        };
        let Lookup::Hit(image, area) = gpu
            .scene_cache
            .borrow_mut()
            .lookup(root.cache.as_ref(), &query(clean))
        else {
            panic!("unmodified crop remains reusable")
        };
        let hit = (image, area);
        assert!(std::rc::Rc::ptr_eq(
            hit.0.main.as_ref().unwrap(),
            original.main.as_ref().unwrap()
        ));
        assert_eq!(hit.1, clean);
        drop(hit);
        // Hitting one clean crop must not advance the old signature and make
        // the changed part of the same cached image look current.
        let dirty = Rect {
            left: 53,
            top: 36,
            width: 8,
            height: 8,
        };
        assert!(matches!(
            gpu.scene_cache
                .borrow_mut()
                .lookup(root.cache.as_ref(), &query(dirty)),
            Lookup::Miss(Some(_))
        ));
        drop(original);

        // Rebuild, then exhaust the fixed write history outside the queried
        // crop. Unknown history must miss, never serve unproved cached pixels.
        drop(
            gpu.scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap(),
        );
        for i in 0..20 {
            gpu.fill(
                images.get_mut(&reference.id).unwrap(),
                &[Fill {
                    rectangle: Rect {
                        left: 96,
                        top: 64,
                        width: 2,
                        height: 2,
                    },
                    color: 0xff123400 + i,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )
            .unwrap();
        }
        let next = Signature::capture(&scene, &images, &children, 0, raster, (0, 0), clean, false)
            .unwrap();
        assert!(
            gpu.scene_cache
                .borrow_mut()
                .get(root.cache.as_ref(), &next)
                .is_none()
        );
        drop(
            gpu.scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap(),
        );
        let mut cache = gpu.scene_cache.borrow_mut();
        assert!(cache.available() < LIMIT);
        let available = cache.available();
        assert!(!cache.make_room(LIMIT + 1));
        assert_eq!(cache.available(), available);
        assert!(
            !cache.make_room(LIMIT),
            "a current-frame raster remains protected"
        );
        cache.begin_frame();
        assert!(cache.make_room(LIMIT));
        assert_eq!(
            cache.available(),
            LIMIT,
            "cold entries cannot block a replacement forever"
        );
    }
}

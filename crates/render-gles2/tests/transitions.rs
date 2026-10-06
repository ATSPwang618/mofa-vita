#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{Blend, DrawFace, Fill, ImageRef, Node, Rect, Scene, Size},
    transition::{Direction, Effect, Frame, SceneTransition, Stay},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::{collections::HashMap, sync::Arc};

fn read(gpu: &Gpu, image: &Image) -> Vec<u32> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
        .collect()
}

#[test]
fn tiled_crossfades_and_rules_reuse_gathers_and_discard_old_output() {
    use krkr_protocol::{
        budget::Budget,
        pixels::{Bytes, Pixels},
    };
    let run = |edge: u32, work: bool| {
        let context = support::Context::new();
        let mut gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    tile_edge: edge,
                    work_framebuffer: work,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let size = Size {
            width: 149,
            height: 87,
        };
        let upload = |offset: u32| {
            let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging).unwrap();
            for (i, pixel) in bytes
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                let n = i as u32 + offset;
                pixel.copy_from_slice(&[
                    (n * 31) as u8,
                    (n * 47) as u8,
                    (n * 79) as u8,
                    (n * 13) as u8,
                ]);
            }
            gpu.assign_bitmap(
                None,
                &Pixels {
                    size,
                    main: Some(bytes),
                    province: None,
                },
            )
            .unwrap()
        };
        let a = upload(0);
        let b = upload(19);
        let rule = upload(43);
        let mut target = gpu.create_surface_image(size).unwrap();
        gpu.collect().unwrap();
        // A tiled frame must fit exactly three patch buffers, independent of
        // the number of destination tiles. Output belongs to the old budget.
        gpu.scratch = Budget::new(3 * edge.min(256).pow(2) as usize * 4);
        let mut frames = Vec::new();
        for effect in [Effect::CrossFade, Effect::Universal { vague: 63 }] {
            for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
                gpu.collect().unwrap();
                traffic::reset();
                gpu.transition(
                    &mut target,
                    &a,
                    &b,
                    Some(&rule),
                    Frame {
                        effect,
                        face,
                        size,
                        phase: 117,
                    },
                )
                .unwrap();
                gpu.resolve().unwrap();
                if work {
                    assert!(
                        traffic::texture_allocations() <= 4,
                        "one allocation per gather, plus the rule curve"
                    );
                    assert_eq!(
                        traffic::finish_calls(),
                        0,
                        "patches must not collect earlier patch buffers"
                    );
                    if edge >= size.width {
                        assert!(
                            traffic::loaded_pixels() <= 4,
                            "a direct transition replaces all output pixels: {} loaded",
                            traffic::loaded_pixels()
                        );
                    }
                }
                frames.push(read(&gpu, &target));
            }
        }
        frames
    };
    let reference = run(256, false);
    assert_eq!(run(32, true), reference);
    assert_eq!(run(256, true), reference);
}
fn image(gpu: &Gpu, size: Size, colors: &[u32]) -> Image {
    let mut image = gpu.create_image(size, 0).unwrap();
    gpu.fill(
        &mut image,
        &colors
            .iter()
            .enumerate()
            .map(|(i, &color)| Fill {
                rectangle: Rect {
                    left: (i as u32 % size.width) as i32,
                    top: (i as u32 / size.width) as i32,
                    width: 1,
                    height: 1,
                },
                color,
                face: DrawFace::Alpha,
                hold_alpha: false,
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    image
}

#[test]
fn physical_transition_targets_keep_logical_phases_at_exact_pixel_centers() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: 5,
                ..Default::default()
            },
        )
        .unwrap()
    };
    // An odd integral reduction puts output centers on source texel centers.
    // Fractional reductions filter the endpoints before blending; comparing
    // them to a nearest sample of an already blended image is not equivalent.
    let size = Size {
        width: 21,
        height: 15,
    };
    let physical = Size {
        width: 7,
        height: 5,
    };
    let colors: Vec<_> = (0..size.width * size.height)
        .map(|i| 0xff000000 | ((i * 31) & 255) << 16 | ((i * 47) & 255) << 8 | (i * 79) & 255)
        .collect();
    let first = image(&gpu, size, &colors);
    let second = image(
        &gpu,
        size,
        &colors.iter().rev().copied().collect::<Vec<_>>(),
    );
    let rule = image(&gpu, size, &colors);
    let mut ids = slotmap::SlotMap::with_key();
    let mut reference = || ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let (a, b, r) = (reference(), reference(), reference());
    let images = HashMap::from([(a.id, first), (b.id, second), (r.id, rule)]);
    let node = |image, visible| Node {
        cache: None,
        visible,
        parent: None,
        image: Some(image),
        neutral_color: 0,
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    };
    let mut scene = Scene {
        nodes: vec![node(a, true), node(b, false)],
        ..Default::default()
    };
    let mut effects = vec![Effect::CrossFade, Effect::Universal { vague: 63 }];
    for from in [
        Direction::Left,
        Direction::Right,
        Direction::Top,
        Direction::Bottom,
    ] {
        for stay in [Stay::Neither, Stay::Destination, Stay::Source] {
            effects.push(Effect::Scroll { from, stay });
        }
    }
    for effect in effects {
        let max = effect.phases(size);
        for phase in [0, 1, max / 2, max] {
            scene.transitions = vec![SceneTransition {
                destination: 0,
                source: 1,
                with_children: false,
                frame: Frame {
                    effect,
                    face: DrawFace::Opaque,
                    size,
                    phase,
                },
                rule: Some(r.clone()),
                custom: None,
            }];
            let full = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
            let expected = read(&gpu, &full);
            let output = gpu
                .scene_surface_scaled(size, physical, &scene, &images)
                .unwrap();
            let got = read(&gpu, &output);
            for y in 0..physical.height {
                for x in 0..physical.width {
                    let sx = (x * 2 + 1) * size.width / (physical.width * 2);
                    let sy = (y * 2 + 1) * size.height / (physical.height * 2);
                    assert_eq!(
                        got[(y * physical.width + x) as usize],
                        expected[(sy * size.width + sx) as usize],
                        "{effect:?} phase {phase} at {x},{y}"
                    );
                }
            }
            drop(output);
            drop(full);
            gpu.collect().unwrap();
        }
    }
}

#[test]
fn cropped_transition_inputs_match_materialized_pixels_without_endpoint_surfaces() {
    for work in [false, true] {
        for edge in [8, 32] {
            let context = support::Context::new();
            let gpu = unsafe {
                Gpu::new(
                    context.gl(),
                    Config {
                        tile_edge: edge,
                        work_framebuffer: work,
                        ..Default::default()
                    },
                )
                .unwrap()
            };
            let logical = Size {
                width: 16,
                height: 12,
            };
            let physical = Size {
                width: 12,
                height: 9,
            };
            let padded = Size {
                width: 18,
                height: 15,
            };
            let mut ids = slotmap::SlotMap::with_key();
            let refs: Vec<_> = (0..4)
                .map(|_| ImageRef {
                    id: ids.insert(()),
                    lifetime: Arc::default(),
                })
                .collect();
            let mut images = HashMap::new();
            for n in 0..2 {
                let colors: Vec<u32> = (0..padded.width * padded.height)
                    .map(|i| 0x50304050u32.wrapping_mul(i + 1 + n * 317))
                    .collect();
                let cropped: Vec<_> = (0..physical.height)
                    .flat_map(|y| {
                        (0..physical.width)
                            .map(|x| colors[((y + 3) * padded.width + x + 3) as usize])
                            .collect::<Vec<_>>()
                    })
                    .collect();
                images.insert(
                    refs[n as usize].id,
                    gpu.logical_image(
                        image(&gpu, padded, &colors),
                        Size {
                            width: 24,
                            height: 20,
                        },
                    )
                    .unwrap(),
                );
                images.insert(
                    refs[n as usize + 2].id,
                    gpu.logical_image(image(&gpu, physical, &cropped), logical)
                        .unwrap(),
                );
            }
            let scene = |base: usize, nested: bool, face, effect, phase| {
                let mut nodes = Vec::new();
                let mut endpoints = Vec::new();
                for i in 0..2 {
                    endpoints.push(nodes.len());
                    let node = Node {
                        cache: None,
                        image: Some(refs[base + i].clone()),
                        neutral_color: 0,
                        parent: None,
                        visible: i == 0,
                        rectangle: logical.rect(),
                        image_left: if base == 0 { -4 } else { 0 },
                        image_top: if base == 0 { -4 } else { 0 },
                        blend: Blend::Opaque,
                        opacity: 255,
                    };
                    if nested {
                        nodes.push(Node {
                            image: None,
                            image_left: 0,
                            image_top: 0,
                            ..node.clone()
                        });
                        nodes.push(Node {
                            parent: Some(nodes.len() - 1),
                            visible: true,
                            ..node
                        });
                    } else {
                        nodes.push(node);
                    }
                }
                Scene {
                    nodes,
                    transitions: vec![SceneTransition {
                        destination: endpoints[0],
                        source: endpoints[1],
                        with_children: true,
                        frame: Frame {
                            effect,
                            face,
                            size: logical,
                            phase,
                        },
                        rule: None,
                        custom: None,
                    }],
                    ..Default::default()
                }
            };
            for nested in [false, true] {
                for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
                    for effect in [
                        Effect::CrossFade,
                        Effect::Scroll {
                            from: Direction::Left,
                            stay: Stay::Neither,
                        },
                    ] {
                        for phase in [0, effect.phases(logical) / 2, effect.phases(logical)] {
                            let reference = gpu
                                .scene_surface_scaled(
                                    logical,
                                    physical,
                                    &scene(2, nested, face, effect, phase),
                                    &images,
                                )
                                .unwrap();
                            let actual = gpu
                                .scene_surface_scaled(
                                    logical,
                                    physical,
                                    &scene(0, nested, face, effect, phase),
                                    &images,
                                )
                                .unwrap();
                            assert_eq!(
                                read(&gpu, &actual),
                                read(&gpu, &reference),
                                "edge={edge} work={work} nested={nested} {face:?} {effect:?} phase={phase}"
                            );
                        }
                    }
                }
            }
            if edge == 32 {
                gpu.collect().unwrap();
                let mut bounded = gpu;
                bounded.scratch =
                    krkr_protocol::budget::Budget::new(physical.rgba_bytes().unwrap());
                let output = bounded
                    .scene_surface_scaled(
                        logical,
                        physical,
                        &scene(0, false, DrawFace::Opaque, Effect::CrossFade, 128),
                        &images,
                    )
                    .unwrap();
                assert_eq!(bounded.scratch.used(), physical.rgba_bytes().unwrap());
                drop(output);
            }
        }
    }
}

#[test]
fn full_hd_nested_transition_subtrees_fit_in_the_physical_surface_allowance() {
    let context = support::Context::new();
    let mut gpu = unsafe { Gpu::new(context.gl(), Default::default()).unwrap() };
    // Two composed endpoints plus the output. A fourth transition intermediate
    // must not be required for a full opaque root.
    gpu.scratch = krkr_protocol::budget::Budget::new(3 * 960 * 540 * 4);
    let logical = Size {
        width: 1920,
        height: 1080,
    };
    let physical = Size {
        width: 960,
        height: 540,
    };
    let node = |parent, visible, color| Node {
        cache: None,
        visible,
        parent,
        image: None,
        neutral_color: color,
        rectangle: logical.rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    };
    let mut nodes = vec![
        node(None, true, 0xff0000),
        node(Some(0), true, 0x0000ff),
        node(None, false, 0x00ff00),
        node(Some(2), true, 0xffffff),
    ];
    nodes[1].rectangle.width = 960;
    nodes[3].rectangle.width = 960;
    // A full-sized nested page can overwrite the display just like a root.
    // The ancestor's initial black fill must not require another surface.
    for node in &mut nodes {
        node.parent = node.parent.map(|parent| parent + 1);
    }
    nodes[0].parent = Some(0);
    nodes.insert(0, node(None, true, 0));
    let scene = Scene {
        nodes,
        transitions: vec![SceneTransition {
            destination: 1,
            source: 3,
            with_children: true,
            frame: Frame {
                effect: Effect::CrossFade,
                face: DrawFace::Opaque,
                size: logical,
                phase: 128,
            },
            rule: None,
            custom: None,
        }],
        ..Default::default()
    };
    let output = gpu
        .scene_surface_scaled(logical, physical, &scene, &HashMap::new())
        .unwrap();
    assert_eq!(gpu.pixel(&output, 479, 270, false).unwrap(), 0x007f7fff);
    assert_eq!(gpu.pixel(&output, 480, 270, false).unwrap(), 0x007f7f00);
    gpu.collect().unwrap();
    assert_eq!(gpu.scratch.used(), physical.rgba_bytes().unwrap());
}

#[test]
fn direct_opaque_node_matches_materialized_transition_for_every_alpha_face() {
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
        let size = Size {
            width: 6,
            height: 1,
        };
        let first = image(
            &gpu,
            size,
            &[
                0x00502030, 0x01406080, 0x80123456, 0xfe2468ac, 0xffedcba9, 0x65432109,
            ],
        );
        let second = image(
            &gpu,
            size,
            &[
                0xffed6543, 0x00876543, 0x01987654, 0x7f142536, 0xfedcba98, 0xaacdeabc,
            ],
        );
        let rule = image(
            &gpu,
            size,
            &[
                0xff000000, 0xff202020, 0xff646464, 0xff808080, 0xffdcdcdc, 0xffffffff,
            ],
        );
        let mut ids = slotmap::SlotMap::with_key();
        let refs: Vec<_> = (0..4)
            .map(|_| ImageRef {
                id: ids.insert(()),
                lifetime: Arc::default(),
            })
            .collect();
        let mut images = HashMap::from([
            (refs[0].id, first),
            (refs[1].id, second),
            (refs[2].id, rule),
        ]);
        let node = |index: usize, visible| Node {
            image: Some(refs[index].clone()),
            visible,
            parent: None,
            blend: Blend::Opaque,
            opacity: 255,
            cache: None,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            neutral_color: 0,
        };
        for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
            for effect in [
                Effect::CrossFade,
                Effect::Universal { vague: 63 },
                Effect::Scroll {
                    from: Direction::Left,
                    stay: Stay::Neither,
                },
            ] {
                for phase in [0, 1, effect.phases(size) / 2, effect.phases(size)] {
                    let frame = Frame {
                        effect,
                        face,
                        size,
                        phase,
                    };
                    let mut intermediate = gpu.create_surface_image(size).unwrap();
                    gpu.transition(
                        &mut intermediate,
                        &images[&refs[0].id],
                        &images[&refs[1].id],
                        Some(&images[&refs[2].id]),
                        frame,
                    )
                    .unwrap();
                    images.insert(refs[3].id, intermediate);
                    let expected = gpu
                        .scene_surface(
                            size,
                            &Scene {
                                nodes: vec![node(3, true)],
                                ..Default::default()
                            },
                            &images,
                            (0, 0),
                        )
                        .unwrap();
                    let mut scene = Scene {
                        nodes: vec![node(0, true), node(1, false)],
                        transitions: vec![SceneTransition {
                            destination: 0,
                            source: 1,
                            with_children: false,
                            frame,
                            rule: Some(refs[2].clone()),
                            custom: None,
                        }],
                        ..Default::default()
                    };
                    let actual = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
                    assert_eq!(
                        read(&gpu, &actual),
                        read(&gpu, &expected),
                        "{face:?} {effect:?} phase={phase} work={work_framebuffer}"
                    );
                    scene.nodes[0].parent = Some(0);
                    scene.nodes.insert(0, node(2, true));
                    scene.transitions[0].destination += 1;
                    scene.transitions[0].source += 1;
                    let nested = gpu.scene_surface(size, &scene, &images, (0, 0)).unwrap();
                    let mut reference = Scene {
                        nodes: vec![node(2, true), node(3, true)],
                        ..Default::default()
                    };
                    reference.nodes[1].parent = Some(0);
                    let expected_nested = gpu
                        .scene_surface(size, &reference, &images, (0, 0))
                        .unwrap();
                    assert_eq!(
                        read(&gpu, &nested),
                        read(&gpu, &expected_nested),
                        "nested {face:?} {effect:?} phase={phase} work={work_framebuffer}"
                    );
                }
            }
        }
    }
}

#[test]
fn nested_transition_groups_fit_in_bands_after_both_endpoints_are_live() {
    let render = |work_framebuffer, bounded| {
        let context = support::Context::new();
        let size = Size {
            width: 128,
            height: 96,
        };
        let bytes = size.rgba_bytes().unwrap();
        let scratch = krkr_protocol::budget::Budget::new(if bounded {
            3 * bytes + 128 * 8 * 4 + usize::from(work_framebuffer) * 128 * 128 * 4
        } else {
            16 * 1024 * 1024
        });
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer,
                    tile_edge: 128,
                    scratch,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        // Disable optional endpoint caching so the scratch limit is exercised.
        let _pressure = gpu.resident.reserve(gpu.resident.available()).unwrap();
        let node = |parent, visible, blend, opacity, color| Node {
            parent,
            visible,
            blend,
            opacity,
            neutral_color: color,
            image: None,
            cache: None,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
        };
        let mut nodes = vec![
            node(None, true, Blend::Opaque, 255, 0x102030),
            node(Some(0), true, Blend::Alpha, 173, 0),
            node(Some(1), true, Blend::Opaque, 255, 0xabcdef),
            node(None, false, Blend::Opaque, 255, 0x708090),
            node(Some(3), true, Blend::Alpha, 93, 0),
            node(Some(4), true, Blend::Opaque, 255, 0x103050),
        ];
        nodes[2].rectangle.width = 87;
        nodes[5].rectangle.left = 23;
        nodes[5].rectangle.width = 105;
        let scene = Scene {
            nodes,
            transitions: vec![SceneTransition {
                destination: 0,
                source: 3,
                with_children: true,
                frame: Frame {
                    effect: Effect::CrossFade,
                    face: DrawFace::Opaque,
                    size,
                    phase: 117,
                },
                rule: None,
                custom: None,
            }],
            ..Default::default()
        };
        let result = gpu
            .scene_surface(size, &scene, &HashMap::new(), (0, 0))
            .unwrap();
        read(&gpu, &result)
    };
    let expected = render(false, false);
    assert_eq!(render(false, true), expected);
    assert_eq!(render(true, true), expected);
}

#[test]

fn fades_follow_integer_faces_rules_endpoints_and_aliasing() {
    for (work, edge) in [(false, 2), (true, 2), (true, 1024)] {
        check_fades(work, edge);
    }
}
fn check_fades(work: bool, edge: u32) {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                tile_edge: edge,
                work_framebuffer: work,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 6,
        height: 1,
    };
    let a = [
        0x645096dc, 0x00ff0000, 0xffff0000, 0x010000ff, 0xfeffffff, 0xffffffff,
    ];
    let b = [
        0xa0c86428, 0xff0000ff, 0x000000ff, 0xfe00ff00, 0x01000000, 0xff000000,
    ];
    let first = image(&gpu, size, &a);
    let second = image(&gpu, size, &b);
    let mut rule = gpu.create_province(size).unwrap();
    let levels = [0, 1, 63, 127, 191, 255];
    gpu.fill(
        &mut rule,
        &levels
            .iter()
            .enumerate()
            .map(|(i, &color)| Fill {
                rectangle: Rect {
                    left: i as i32,
                    top: 0,
                    width: 1,
                    height: 1,
                },
                color,
                face: DrawFace::Province,
                hold_alpha: false,
            })
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let lookup = krkr_render::blend::lookup_table();
    let fixture = Frame {
        effect: Effect::CrossFade,
        face: DrawFace::AddAlpha,
        size,
        phase: 128,
    };
    assert_eq!(
        krkr_render::transition::blend(fixture, a[0], b[0], 0, &lookup),
        0x828c7d82
    );
    for effect in [
        Effect::CrossFade,
        Effect::Universal { vague: 0 },
        Effect::Universal { vague: 64 },
        Effect::Universal { vague: 511 },
        Effect::Universal { vague: 512 },
        Effect::Universal {
            vague: i32::MAX as u32 / 255,
        },
    ] {
        for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
            for phase in [
                0,
                1,
                127,
                128,
                254,
                effect.phases(size) / 2,
                effect.phases(size) - 1,
                effect.phases(size),
            ] {
                let frame = Frame {
                    effect,
                    face,
                    size,
                    phase,
                };
                // COW preserves the original, then an in-place draw samples the
                // same allocation it replaces. Neither case may corrupt inputs.
                let mut output = first.shared();
                gpu.transition(&mut output, &first, &second, Some(&rule), frame)
                    .unwrap();
                let expected: Vec<_> = a
                    .iter()
                    .zip(b)
                    .zip(levels)
                    .map(|((&a, b), rule)| {
                        krkr_render::transition::blend(frame, a, b, rule as u8, &lookup)
                    })
                    .collect();
                assert_eq!(read(&gpu, &output), expected, "{frame:?}");
                assert_eq!(read(&gpu, &first), a);
                let input = output.shared();
                gpu.transition(&mut output, &input, &second, Some(&rule), frame)
                    .unwrap();
                let expected: Vec<_> = expected
                    .iter()
                    .zip(b)
                    .zip(levels)
                    .map(|((&a, b), rule)| {
                        krkr_render::transition::blend(frame, a, b, rule as u8, &lookup)
                    })
                    .collect();
                assert_eq!(read(&gpu, &output), expected, "aliased {frame:?}");
            }
        }
    }
}

#[test]

fn scroll_copies_both_axes_and_stay_modes_including_transparent_pixels() {
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
        width: 4,
        height: 4,
    };
    let a: Vec<_> = (0..16).map(|i| 0xff100000 + i).collect();
    let b: Vec<_> = (0..16).map(|i| 0x00200000 + i).collect();
    let first = image(&gpu, size, &a);
    let second = image(&gpu, size, &b);
    let mut output = gpu.create_image(size, 0).unwrap();
    for from in [
        Direction::Left,
        Direction::Top,
        Direction::Right,
        Direction::Bottom,
    ] {
        for stay in [Stay::Neither, Stay::Destination, Stay::Source] {
            for phase in 0..=4 {
                let frame = Frame {
                    effect: Effect::Scroll { from, stay },
                    face: DrawFace::Alpha,
                    size,
                    phase,
                };
                gpu.transition(&mut output, &first, &second, None, frame)
                    .unwrap();
                let expected: Vec<_> = (0..16)
                    .map(|i| {
                        let (second, x, y) =
                            krkr_render::transition::scroll_source(frame, i % 4, i / 4);
                        (if second { &b } else { &a })[(y * 4 + x) as usize]
                    })
                    .collect();
                assert_eq!(read(&gpu, &output), expected, "{frame:?}");
                if from == Direction::Left && phase == 2 {
                    let row = match stay {
                        Stay::Neither => [b[2], b[3], a[0], a[1]],
                        Stay::Destination => [b[2], b[3], a[2], a[3]],
                        Stay::Source => [b[0], b[1], a[0], a[1]],
                    };
                    assert_eq!(&expected[..4], &row);
                }
            }
        }
    }
}

#[test]

fn scene_can_transition_hidden_pages_with_or_without_their_children() {
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
    let mut node = |color, parent, visible| {
        let id = ids.insert(());
        images.insert(id, gpu.create_image(size, color).unwrap());
        Node {
            cache: None,
            neutral_color: 0,
            image: Some(ImageRef {
                id,
                lifetime: Arc::default(),
            }),
            parent,
            visible,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        }
    };
    let mut nodes = vec![
        node(0xff000000, None, true),
        node(0xffff0000, Some(0), true),
        node(0xff00ff00, None, false),
        node(0xff0000ff, Some(2), true),
    ];
    nodes[1].rectangle.left = 1;
    nodes[1].rectangle.width = 1;
    nodes[3].rectangle.width = 1;
    let mut scene = Scene {
        viewport: Default::default(),
        requires_op_seq: 0,
        nodes,
        transitions: vec![SceneTransition {
            destination: 0,
            source: 2,
            with_children: true,
            frame: Frame {
                effect: Effect::CrossFade,
                face: DrawFace::Opaque,
                size,
                phase: 128,
            },
            rule: None,
            custom: None,
        }],
    };
    let mut output = gpu.create_surface_image(size).unwrap();
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(read(&gpu, &output), [0xff00007f, 0xff7f7f00]);
    scene.transitions[0].with_children = false;
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(read(&gpu, &output), [0xff007f00, 0xffff0000]);
    scene.transitions[0].with_children = true;
    scene.transitions[0].frame.phase = 255;
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(read(&gpu, &output), [0xff0000ff, 0xff00ff00]);
    let id = ids.insert(());
    images.insert(id, gpu.create_image(size, 0xffffffff).unwrap());
    scene.nodes.push(Node {
        cache: None,
        neutral_color: 0,
        parent: None,
        visible: false,
        image: Some(ImageRef {
            id,
            lifetime: Arc::default(),
        }),
        rectangle: size.rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    });
    scene.transitions.push(SceneTransition {
        destination: 2,
        source: 4,
        with_children: false,
        frame: Frame {
            effect: Effect::CrossFade,
            face: DrawFace::Opaque,
            size,
            phase: 128,
        },
        rule: None,
        custom: None,
    });
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(
        read(&gpu, &output),
        [0xff0000ff, 0xff7fff7f],
        "source Complete includes its active effect and children"
    );
    scene.transitions[0].with_children = false;
    gpu.compose(&mut output, &scene, &images).unwrap();
    assert_eq!(
        read(&gpu, &output),
        [0xff00ff00, 0xffff0000],
        "without children the source is raw MainImage"
    );
}

#![cfg(target_os = "linux")]
mod support;
#[allow(dead_code)]
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    graphics::{Blend, DrawFace, Fill, ImageRef, Node, Scene, Size},
    transition::{Effect, Frame, SceneTransition},
};
use krkr_render_gles2::{Config, Gpu};
use std::{collections::HashMap, sync::Arc};

fn render(work_framebuffer: bool) -> (Vec<Vec<u8>>, Vec<usize>) {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 24,
        height: 16,
    };
    let mut ids = slotmap::SlotMap::with_key();
    let mut images = HashMap::new();
    let mut node = |color, parent, visible, blend| {
        let id = ids.insert(());
        images.insert(id, gpu.create_image(size, color).unwrap());
        Node {
            image: Some(ImageRef {
                id,
                lifetime: Arc::default(),
            }),
            parent,
            visible,
            blend,
            opacity: 255,
            cache: None,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            neutral_color: 0,
        }
    };
    let mut nodes = vec![
        node(0xff102030, None, true, Blend::Opaque),
        node(0x80e02040, Some(0), true, Blend::Alpha),
        node(0xff406080, None, false, Blend::Opaque),
        node(0x8070c020, Some(2), true, Blend::Alpha),
        node(0xffe0c0a0, None, false, Blend::Opaque),
    ];
    nodes[1].rectangle.width = 8;
    nodes[3].rectangle.left = 12;
    nodes[3].rectangle.width = 8;
    let source_child = nodes[3].image.as_ref().unwrap().id;
    // The frame face follows the destination layer (a.blend.face() in the
    // engine): an opaque face writes no alpha, so a translucent destination
    // must ask for its own face or its transition output never blends in.
    let transition = |destination, source, with_children, phase, face| SceneTransition {
        destination,
        source,
        with_children,
        frame: Frame {
            effect: Effect::CrossFade,
            face,
            size,
            phase,
        },
        rule: None,
        custom: None,
    };
    let mut scene = Scene {
        nodes,
        transitions: vec![transition(0, 2, true, 64, DrawFace::Opaque)],
        ..Default::default()
    };
    let mut frames = Vec::new();
    let mut stores = Vec::new();
    for step in 0..7 {
        scene.transitions[0].frame.phase = if step == 0 { 64 } else { 128 };
        match step {
            2 => gpu
                .fill(
                    images.get_mut(&source_child).unwrap(),
                    &[Fill {
                        rectangle: size.rect(),
                        color: 0x90c030a0,
                        face: DrawFace::Alpha,
                        hold_alpha: false,
                    }],
                )
                .unwrap(),
            3 => scene
                .transitions
                .push(transition(3, 4, false, 64, DrawFace::Alpha)),
            4 => scene.transitions[1].frame.phase = 192,
            5 => {
                scene.transitions.pop();
                scene.nodes[3].visible = false;
            }
            6 => {
                scene.nodes[3].visible = true;
                scene.nodes[3].rectangle.left = 4;
            }
            _ => {}
        }
        gpu.resolve().unwrap();
        traffic::reset();
        let output = gpu
            .scene_surface_scaled(size, size, &scene, &images)
            .unwrap();
        stores.push(traffic::store_calls());
        frames.push(
            gpu.readback(&output, size.rect(), false)
                .unwrap()
                .data
                .as_slice()
                .to_vec(),
        );
        drop(output);
        gpu.maintain().unwrap();
    }
    (frames, stores)
}

#[test]
fn transition_inputs_reuse_pixels_and_invalidate_hidden_children_and_nested_effects() {
    let (reference, _) = render(false);
    let (frames, stores) = render(true);
    assert_eq!(
        frames, reference,
        "cached inputs must match uncached composition at every phase"
    );
    assert!(
        stores[1] < stores[0],
        "unchanged inputs must eliminate transfers: {stores:?}"
    );
    assert!(
        stores[2] > stores[1],
        "a modified hidden endpoint must be recomposited: {stores:?}"
    );
    assert_ne!(
        frames[3], frames[4],
        "nested transition progress must remain live"
    );
    assert_ne!(
        frames[5], frames[6],
        "child visibility and geometry must invalidate cached content"
    );
}

#[test]
fn completed_sibling_caches_yield_to_a_larger_transition_in_the_same_frame() {
    let render = |bounded| {
        let context = support::Context::new();
        let size = Size {
            width: 64,
            height: 64,
        };
        let large = Size {
            width: 128,
            height: 128,
        };
        // The working framebuffer plus display, two large endpoints and the
        // large transition output fit. Four optional earlier sibling caches
        // must yield while the active endpoints remain intact.
        let parent = krkr_protocol::budget::Budget::new(if bounded {
            14 * size.rgba_bytes().unwrap() + large.rgba_bytes().unwrap() + 328_704
        } else {
            16 * 1024 * 1024
        });
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    resident: parent.child(parent.limit()),
                    scratch: parent.child(parent.limit()),
                    tile_edge: 128,
                    work_framebuffer: true,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let node = |size: Size, parent, visible, blend, color| Node {
            cache: None,
            image: None,
            visible,
            parent,
            blend,
            opacity: 255,
            rectangle: size.rect(),
            image_left: 0,
            image_top: 0,
            neutral_color: color,
        };
        let mut nodes = Vec::new();
        for color in [0x204060, 0x804020, 0x10c040, 0xa020c0] {
            let root = nodes.len();
            nodes.push(node(size, None, true, Blend::Alpha, 0));
            nodes.push(node(size, Some(root), true, Blend::Opaque, color));
        }
        let destination = nodes.len();
        nodes.push(node(large, None, true, Blend::Alpha, 0));
        nodes.push(node(
            large,
            Some(destination),
            true,
            Blend::Opaque,
            0x204060,
        ));
        let source = nodes.len();
        nodes.push(node(large, None, false, Blend::Alpha, 0));
        nodes.push(node(large, Some(source), true, Blend::Opaque, 0x806040));
        let scene = Scene {
            nodes,
            transitions: vec![SceneTransition {
                destination,
                source,
                with_children: true,
                frame: Frame {
                    effect: Effect::CrossFade,
                    face: DrawFace::Alpha,
                    size: large,
                    phase: 128,
                },
                rule: None,
                custom: None,
            }],
            ..Default::default()
        };
        let output = gpu
            .scene_surface_scaled(size, size, &scene, &HashMap::new())
            .unwrap();
        gpu.readback(&output, size.rect(), false)
            .unwrap()
            .data
            .as_slice()
            .to_vec()
    };
    assert_eq!(render(true), render(false));
}

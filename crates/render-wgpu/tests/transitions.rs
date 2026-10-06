use krkr_protocol::{
    graphics::{Blend, DrawFace, Fill, ImageRef, Node, Rect, Scene, Size},
    transition::{Direction, Effect, Frame, SceneTransition, Stay},
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
fn read(gpu: &Gpu, image: &Image) -> Vec<u32> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let start = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(result) = read.take() {
            return result
                .unwrap()
                .data
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]]))
                .collect();
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
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
#[ignore = "requires a real desktop GPU"]
fn fades_follow_integer_faces_rules_endpoints_and_aliasing() {
    let gpu = gpu();
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
        Effect::Universal { vague: 512 },
    ] {
        for face in [DrawFace::Opaque, DrawFace::Alpha, DrawFace::AddAlpha] {
            for phase in [0, 1, 127, 128, 254, effect.phases(size)] {
                let frame = Frame {
                    effect,
                    face,
                    size,
                    phase,
                };
                // COW preserves the original, then an in-place draw samples the
                // same allocation it replaces. Neither case may corrupt inputs.
                let mut output = first.shared();
                gpu.transition(
                    &mut output,
                    &first.source(),
                    &second.source(),
                    Some(&rule.source()),
                    frame,
                )
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
                let input = output.source();
                gpu.transition(
                    &mut output,
                    &input,
                    &second.source(),
                    Some(&rule.source()),
                    frame,
                )
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
#[ignore = "requires a real desktop GPU"]
fn scroll_copies_both_axes_and_stay_modes_including_transparent_pixels() {
    let gpu = gpu();
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
                gpu.transition(&mut output, &first.source(), &second.source(), None, frame)
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
#[ignore = "requires a real desktop GPU"]
fn scene_can_transition_hidden_pages_with_or_without_their_children() {
    let gpu = gpu();
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

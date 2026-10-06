use krkr_protocol::window::{self, Command, Event, Geometry, Input, Limits};
use std::sync::Arc;

#[test]
fn physical_modifier_sides_and_press_latches_survive_independent_release() {
    let (client, host) = window::channel(Default::default(), Arc::new(|| {}));
    let window = slotmap::SlotMap::with_key().insert(());
    for key in [160, 161] {
        host.update_key_state(key, true);
        host.post(Event {
            window,
            input: Input::KeyDown { key: 16, shift: 1 },
        })
        .unwrap();
    }
    host.update_key_state(160, false);
    host.post(Event {
        window,
        input: Input::KeyUp { key: 16, shift: 1 },
    })
    .unwrap();
    assert!(client.key_state(16, true));
    assert!(!client.key_state(160, true));
    assert!(client.key_state(161, true));
    host.update_key_state(161, false);
    host.post(Event {
        window,
        input: Input::KeyUp { key: 16, shift: 0 },
    })
    .unwrap();
    assert!(!client.key_state(16, true));
    host.post(Event {
        window,
        input: Input::MouseDown {
            x: 0,
            y: 0,
            button: 3,
            shift: 256,
        },
    })
    .unwrap();
    host.post(Event {
        window,
        input: Input::MouseUp {
            x: 0,
            y: 0,
            button: 3,
            shift: 0,
        },
    })
    .unwrap();
    assert!(client.key_state(5, false));
    assert!(!client.key_state(5, false));
    assert!(!client.key_state(5, true));
}

#[test]
fn admission_covers_queued_running_completed_and_cancelled_requests() {
    let (client, host) = window::channel(
        Limits {
            operations: 1,
            caption_bytes: 4,
            ..Default::default()
        },
        Arc::new(|| {}),
    );
    let id = slotmap::SlotMap::with_key().insert(());
    assert!(
        client
            .request(id, Command::Caption("large".into()))
            .is_err()
    );
    let ticket = client.request(id, Command::Caption("okay".into())).unwrap();
    let running = host.next_request().unwrap();
    assert!(client.request(id, Command::Visible(true)).is_err());
    drop(ticket);
    assert!(running.cancelled());
    assert!(client.request(id, Command::Visible(true)).is_err());
    drop(running);
    let ticket = client.request(id, Command::Visible(true)).unwrap();
    host.next_request()
        .unwrap()
        .complete(Ok(Geometry::default()));
    assert!(client.request(id, Command::Visible(true)).is_err());
    assert!(matches!(ticket.take().unwrap().unwrap(),
        window::Response::Geometry(g) if g == Geometry::default()));
    drop(ticket);
    let ticket = client.request(id, Command::Visible(true)).unwrap();
    drop(host);
    assert!(ticket.take().unwrap().is_err());
    assert!(client.request(id, Command::Visible(true)).is_err());
}

#[test]
fn motion_coalesces_without_crossing_key_edges_and_overflow_is_reported() {
    let (client, host) = window::channel(
        Limits {
            events: 3,
            ..Default::default()
        },
        Arc::new(|| {}),
    );
    let id = slotmap::SlotMap::with_key().insert(());
    let post = |input| host.post(Event { window: id, input });
    post(Input::MouseMove {
        x: 1,
        y: 2,
        shift: 0,
    })
    .unwrap();
    post(Input::MouseMove {
        x: 3,
        y: 4,
        shift: 0,
    })
    .unwrap();
    post(Input::KeyDown { key: 65, shift: 0 }).unwrap();
    post(Input::MouseMove {
        x: 5,
        y: 6,
        shift: 0,
    })
    .unwrap();
    assert_eq!(
        client.pop_event().unwrap().input,
        Input::MouseMove {
            x: 3,
            y: 4,
            shift: 0
        }
    );
    post(Input::KeyUp { key: 65, shift: 0 }).unwrap();
    assert_eq!(
        client.pop_event().unwrap().input,
        Input::KeyDown { key: 65, shift: 0 }
    );
    assert_eq!(
        client.pop_event().unwrap().input,
        Input::MouseMove {
            x: 5,
            y: 6,
            shift: 0
        }
    );
    assert_eq!(
        client.pop_event().unwrap().input,
        Input::KeyUp { key: 65, shift: 0 }
    );
    for _ in 0..3 {
        post(Input::Close).unwrap();
    }
    assert!(post(Input::KeyUp { key: 65, shift: 0 }).is_err());
    assert!(client.failure().unwrap().contains("capacity"));
}

#[test]
fn queued_image_writes_invalidate_only_the_affected_hit_plane() {
    use krkr_protocol::graphics::{Command as Draw, DrawFace, Fill, ImageRef, Rect};
    let (client, _host) = window::channel(
        Limits {
            operations: 1,
            ..Default::default()
        },
        Arc::new(|| {}),
    );
    let window = slotmap::SlotMap::with_key().insert(());
    let image = ImageRef {
        id: slotmap::SlotMap::with_key().insert(()),
        lifetime: Arc::default(),
    };
    let fill = |face, hold_alpha| {
        Command::Graphics(Draw::Fill {
            image: image.clone(),
            fills: vec![Fill {
                rectangle: Rect {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 1,
                },
                color: 255,
                face,
                hold_alpha,
            }],
        })
    };
    let ticket = client
        .request(window, fill(DrawFace::Alpha, false))
        .unwrap();
    assert_eq!(image.lifetime.revision(false), 1);
    assert!(
        client
            .request(window, fill(DrawFace::Alpha, false))
            .is_err()
    );
    assert_eq!(
        image.lifetime.revision(false),
        1,
        "rejected admission never invalidates a snapshot"
    );
    drop(ticket);
    // Release the cancelled queue item's capacity, as an actual host does.
    drop(_host.next_request());
    for (face, hold, expected) in [
        (DrawFace::Opaque, true, (1, 0)),
        (DrawFace::Mask, true, (2, 0)),
        (DrawFace::Province, false, (2, 1)),
        (DrawFace::Opaque, false, (3, 1)),
    ] {
        let ticket = client.request(window, fill(face, hold)).unwrap();
        assert_eq!(
            (
                image.lifetime.revision(false),
                image.lifetime.revision(true)
            ),
            expected
        );
        drop(ticket);
        drop(_host.next_request());
    }
    let _ticket = client
        .request(
            window,
            Command::Graphics(Draw::ReadHitPlane {
                image: image.clone(),
                province: false,
            }),
        )
        .unwrap();
    assert_eq!(
        image.lifetime.revision(false),
        3,
        "reading preserves the current snapshot version"
    );
    drop(_ticket);
    drop(_host.next_request());
    let rectangle = Rect {
        left: 2,
        top: 3,
        width: 1,
        height: 1,
    };
    let ticket = client
        .request(
            window,
            Command::Graphics(Draw::ReadRegion {
                image: image.clone(),
                rectangle,
            }),
        )
        .unwrap();
    assert_eq!(image.lifetime.revision(false), 3);
    drop(ticket);
    drop(_host.next_request());
    let main = krkr_protocol::pixels::Bytes::zeroed(4, &_host.staging_budget()).unwrap();
    let _ticket = client
        .request(
            window,
            Command::Graphics(Draw::PatchRegion {
                image: image.clone(),
                rectangle,
                pixels: Arc::new(krkr_protocol::pixels::Pixels {
                    size: krkr_protocol::graphics::Size {
                        width: 1,
                        height: 1,
                    },
                    main: Some(main),
                    province: None,
                }),
            }),
        )
        .unwrap();
    assert_eq!(
        (
            image.lifetime.revision(false),
            image.lifetime.revision(true)
        ),
        (4, 1)
    );
}

#[test]
fn rgb_only_text_transforms_and_adjustments_keep_cached_hit_alpha() {
    use krkr_protocol::{
        graphics::{Adjustment, Command as Draw, DrawFace, ImageRef, Size},
        text::{Glyph, PlacedGlyph, Run, Style},
        transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
    };
    let (client, host) = window::channel(Limits::default(), Arc::new(|| {}));
    let window = slotmap::SlotMap::with_key().insert(());
    let image = ImageRef {
        id: slotmap::SlotMap::with_key().insert(()),
        lifetime: Arc::default(),
    };
    let size = Size {
        width: 1,
        height: 1,
    };
    let budget = host.staging_budget();
    let send = |command, expected| {
        let ticket = client.request(window, Command::Graphics(command)).unwrap();
        assert_eq!(
            (
                image.lifetime.revision(false),
                image.lifetime.revision(true)
            ),
            expected
        );
        drop(ticket);
        drop(host.next_request());
    };
    for (face, hold_alpha, expected) in [
        (DrawFace::Opaque, true, 0),
        (DrawFace::Opaque, false, 1),
        (DrawFace::Alpha, true, 2),
        (DrawFace::AddAlpha, true, 3),
    ] {
        let mut mask = krkr_protocol::pixels::Bytes::zeroed(1, &budget).unwrap();
        mask.as_mut_slice()[0] = 128;
        let glyphs = vec![PlacedGlyph {
            glyph: Arc::new(Glyph {
                id: 1,
                size,
                origin: [0, 0],
                advance: [1, 0],
                levels: 256,
                mask,
            }),
            x: 0,
            y: 0,
            color: 0xffffff,
        }];
        let permit = budget
            .reserve(glyphs.capacity() * std::mem::size_of::<PlacedGlyph>())
            .unwrap();
        send(
            Draw::Text {
                image: image.clone(),
                run: Run { glyphs, permit },
                clip: size.rect(),
                style: Style {
                    color: 0xffffff,
                    opacity: 255,
                    antialias: true,
                    shadow_level: 0,
                    shadow_color: 0,
                    shadow_width: 0,
                    shadow_offset: [0, 0],
                    face,
                    hold_alpha,
                },
            },
            (expected, 0),
        );
    }
    for (hold_alpha, expected) in [(true, 3), (false, 4)] {
        send(
            Draw::Transform {
                image: image.clone(),
                source: image.clone(),
                rectangle: size.rect(),
                transform: Transform::Stretch(StretchRect {
                    left: 0,
                    top: 0,
                    width: 2,
                    height: 2,
                }),
                sampling: Sampling {
                    filter: Filter::FastLinear,
                    sharpness: 0.,
                    no_clip: false,
                },
                operation: ImageOperation::Copy { hold_alpha },
                clip: size.rect(),
                clear: Some(0),
            },
            (expected, 0),
        );
    }
    for operation in [
        Adjustment::GrayScale,
        Adjustment::Gamma {
            table: Arc::new(std::array::from_fn(|i| [i as u32; 4])),
            additive: true,
        },
    ] {
        send(
            Draw::Adjust {
                image: image.clone(),
                rectangle: size.rect(),
                operation,
            },
            (4, 0),
        );
    }
    send(
        Draw::Adjust {
            image: image.clone(),
            rectangle: size.rect(),
            operation: Adjustment::Flip { horizontal: true },
        },
        (5, 1),
    );
}

#[test]
fn snapshots_wait_for_prior_operations_and_replacement_releases_old_images() {
    use krkr_protocol::graphics::{Blend, ImageRef, Node, Scene, Size};
    let (client, host) = window::channel(
        Limits {
            scene_nodes: 1,
            ..Default::default()
        },
        Arc::new(|| {}),
    );
    let id = slotmap::SlotMap::with_key().insert(());
    let ticket = client.request(id, Command::Visible(true)).unwrap();
    let request = host.next_request().unwrap();
    let lifetime = Arc::default();
    let weak = Arc::downgrade(&lifetime);
    let node = Node {
        cache: None,
        neutral_color: 0,
        visible: true,
        parent: None,
        image: Some(ImageRef {
            id: slotmap::SlotMap::with_key().insert(()),
            lifetime,
        }),
        rectangle: Size {
            width: 2,
            height: 2,
        }
        .rect(),
        image_left: 0,
        image_top: 0,
        blend: Blend::Opaque,
        opacity: 255,
    };
    client
        .publish(
            id,
            Scene {
                viewport: Default::default(),
                transitions: Vec::new(),
                requires_op_seq: 0,
                nodes: vec![node.clone()],
            },
        )
        .unwrap();
    assert!(host.take_scenes(0).is_empty());
    assert!(
        client
            .publish(
                id,
                Scene {
                    viewport: Default::default(),
                    transitions: Vec::new(),
                    requires_op_seq: 0,
                    nodes: vec![node.clone(), node.clone()]
                }
            )
            .is_err()
    );
    drop(node);
    assert!(weak.upgrade().is_some());
    let scenes = host.take_scenes(request.sequence);
    assert_eq!(scenes[0].1.requires_op_seq, request.sequence);
    drop(scenes);
    assert!(weak.upgrade().is_none());
    request.complete(Ok(Geometry::default()));
    assert!(ticket.take().unwrap().is_ok());
}
#[test]
fn auto_repeat_does_not_recreate_a_consumed_press_latch() {
    let (client, host) =
        krkr_protocol::window::channel(Default::default(), std::sync::Arc::new(|| {}));
    host.update_key_state(65, true);
    assert!(client.key_state(65, false));
    host.update_key_state(65, true);
    assert!(!client.key_state(65, false));
    assert!(client.key_state(65, true));
    host.update_key_state(65, false);
    assert!(!client.key_state(65, true));
    host.update_key_state(65, true);
    assert!(client.key_state(65, false));
}

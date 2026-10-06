#![cfg(any(target_os = "linux", all(windows, feature = "windows-gles-tests")))]
mod support;
#[path = "support/traffic.rs"]
mod traffic;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Rect, Size},
    pixels::Bytes,
    text::{Glyph, PlacedGlyph, Run, Style},
};
use krkr_render_gles2::{Config, Gpu, Image};
use std::sync::Arc;
fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    gpu.readback(image, image.size.rect(), false)
        .unwrap()
        .data
        .as_slice()
        .to_vec()
}
fn style() -> Style {
    Style {
        color: 0xff0000,
        opacity: 255,
        antialias: true,
        shadow_level: 0,
        shadow_color: 0,
        shadow_width: 0,
        shadow_offset: [0, 0],
        face: DrawFace::Opaque,
        hold_alpha: true,
    }
}

#[test]
fn committing_a_dialogue_line_keeps_display_strokes() {
    use krkr_protocol::graphics::{Blend, BlendOptions, ImageRef, Node, Scene};
    use std::collections::HashMap;
    let context = support::Context::new();
    let logical = Size {
        width: 1280,
        height: 720,
    };
    let physical = Size {
        width: 960,
        height: 540,
    };
    let line_size = Size {
        width: 1288,
        height: 42,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(physical),
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(logical);
    let budget = Budget::new(1024 * 1024);
    let mut mask = Bytes::zeroed(24 * 24, &budget).unwrap();
    for y in 0..24 {
        for x in 0..24 {
            mask.as_mut_slice()[y * 24 + x] = if x % 6 == 1 || y % 6 == 1 { 255 } else { 0 };
        }
    }
    let glyph = Arc::new(Glyph {
        id: 90210,
        size: Size {
            width: 24,
            height: 24,
        },
        origin: [0, 0],
        advance: [28, 0],
        levels: 256,
        mask,
    });
    let text = run(
        (0..8)
            .map(|i| PlacedGlyph {
                glyph: glyph.clone(),
                x: 4 + i * 28,
                y: 9,
                color: 0xffffff,
            })
            .collect(),
        &budget,
    );
    for blend in [Blend::Alpha, Blend::AddAlpha] {
        let mut line = gpu.create_image(line_size, 0).unwrap();
        gpu.draw_text(
            &mut line,
            &text,
            Style {
                face: blend.face(),
                hold_alpha: false,
                ..style()
            },
            line_size.rect(),
        )
        .unwrap();
        let mut ids = slotmap::SlotMap::with_key();
        let refs: Vec<_> = (0..3)
            .map(|_| ImageRef {
                id: ids.insert(()),
                lifetime: Arc::default(),
            })
            .collect();
        let mut images = HashMap::from([
            (refs[0].id, gpu.create_image(logical, 0xff102030).unwrap()),
            (refs[1].id, gpu.create_image(logical, 0).unwrap()),
            (refs[2].id, line),
        ]);
        let base = Node {
            cache: None,
            parent: None,
            visible: true,
            image: Some(refs[0].clone()),
            neutral_color: 0,
            rectangle: logical.rect(),
            image_left: 0,
            image_top: 0,
            blend: Blend::Opaque,
            opacity: 255,
        };
        // Original aligned cases and the game's actual 50-pixel parent offset.
        for (parent_x, parent_y, x, y) in [
            (0, 0, 20, 32),
            (0, 0, 21, 33),
            (0, 0, 22, 34),
            (0, 0, 23, 35),
            (0, 50, 116, 44),
            (0, 50, 142, 76),
            (0, 50, 142, 108),
            (1, 51, 142, 172),
        ] {
            images.insert(refs[1].id, gpu.create_image(logical, 0).unwrap());
            let mut scene = Scene {
                nodes: vec![
                    base.clone(),
                    Node {
                        image: Some(refs[1].clone()),
                        blend,
                        rectangle: Rect {
                            left: parent_x,
                            top: parent_y,
                            ..logical.rect()
                        },
                        ..base.clone()
                    },
                    Node {
                        parent: Some(1),
                        image: Some(refs[2].clone()),
                        rectangle: Rect {
                            left: x,
                            top: y,
                            ..line_size.rect()
                        },
                        blend,
                        ..base.clone()
                    },
                ],
                ..Default::default()
            };
            let before = pixels(
                &gpu,
                &gpu.scene_surface_scaled(logical, physical, &scene, &images)
                    .unwrap(),
            );
            let line = images[&refs[2].id].shared();
            let target = &images[&refs[1].id];
            let estimate = gpu.operate_write_bytes(
                target,
                &line,
                Rect {
                    left: x,
                    top: y,
                    ..line_size.rect()
                },
                false,
            );
            let used = gpu.resident.used();
            gpu.operate(
                images.get_mut(&refs[1].id).unwrap(),
                &line,
                line_size.rect(),
                x,
                y,
                logical.rect(),
                BlendOptions::for_composition(blend, blend.face(), 255),
            )
            .unwrap();
            assert!(gpu.resident.used().saturating_sub(used) <= estimate);
            assert!(
                images[&refs[1].id].resident_bytes() < 300_000,
                "a committed line allocated the entire paragraph canvas"
            );
            scene.nodes[2].visible = false;
            let after = pixels(
                &gpu,
                &gpu.scene_surface_scaled(logical, physical, &scene, &images)
                    .unwrap(),
            );
            let changed = before
                .iter()
                .zip(&after)
                .filter(|(a, b)| a.abs_diff(**b) > 8)
                .count();
            let max = before
                .iter()
                .zip(&after)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            eprintln!(
                "line commit {blend:?} parent=({parent_x},{parent_y}) ({x},{y}): changed_channels={changed} max_delta={max} bytes={}",
                images[&refs[1].id].resident_bytes()
            );
            // Different texture origins can change the hardware filter's
            // final 8-bit rounding by one, but must not change stroke coverage.
            assert!(
                max <= 1,
                "committing the line changed its visible strokes at ({x},{y}) with {blend:?}"
            );
        }
    }
}

#[test]
fn text_copy_resize_and_spill_keep_the_logical_pixel_grid() {
    use krkr_protocol::graphics::Fill;
    let context = support::Context::new();
    let size = Size {
        width: 1280,
        height: 720,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                work_framebuffer: true,
                small_canvas_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let budget = Budget::new(1024 * 1024);
    // A dense paragraph exceeds the spill threshold after empty borders shrink.
    let mut coverage = Bytes::zeroed(400 * 200, &budget).unwrap();
    for (i, value) in coverage.as_mut_slice().iter_mut().enumerate() {
        *value = [0, 255, 0, 128, 255, 0][i % 6];
    }
    let glyph = Arc::new(Glyph {
        id: 90211,
        size: Size {
            width: 400,
            height: 200,
        },
        origin: [0, 0],
        advance: [400, 0],
        levels: 256,
        mask: coverage,
    });
    let text = run(
        vec![PlacedGlyph {
            glyph,
            x: 22,
            y: 33,
            color: 0xffffff,
        }],
        &budget,
    );
    let mut source = gpu.create_image(size, 0).unwrap();
    gpu.draw_text(&mut source, &text, style(), size.rect())
        .unwrap();
    let expected = pixels(&gpu, &source);
    let mut copy = gpu.create_image(size, 0).unwrap();
    let estimate = gpu.copy_write_bytes(&copy, &source, size.rect(), false, false);
    let used = gpu.resident.used();
    gpu.copy_rect(
        &mut copy,
        &source,
        size.rect(),
        0,
        0,
        size.rect(),
        DrawFace::Alpha,
        false,
    )
    .unwrap();
    assert!(gpu.resident.used().saturating_sub(used) <= estimate);
    drop(source);
    let grown_size = Size {
        width: 1290,
        height: 730,
    };
    let grown = gpu.resize(&copy, grown_size, 0).unwrap();
    let mut restored = gpu.resize(&grown, size, 0).unwrap();
    drop((copy, grown));
    // Lossless pressure compaction must retain the policy along with pixels.
    gpu.compact_canvas(&mut restored, true).unwrap();
    let spill = gpu.spill_canvas(&restored).unwrap().unwrap();
    drop(restored);
    let mut restored = gpu.restore_canvas(&spill).unwrap();
    gpu.fill(
        &mut restored,
        &[Fill {
            rectangle: Rect {
                left: 1000,
                top: 600,
                width: 1,
                height: 1,
            },
            color: 0,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(restored.stored_size(), Some(size));
    assert_eq!(pixels(&gpu, &restored), expected);
}

#[test]
fn incremental_paragraph_preserves_pixels_and_bounded_storage() {
    let context = support::Context::new();
    let size = Size {
        width: 640,
        height: 480,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                ..Default::default()
            },
        )
        .unwrap()
    };
    gpu.set_canvas_size(size);
    let budget = Budget::new(1024 * 1024);
    let mut mask = Bytes::zeroed(18 * 20, &budget).unwrap();
    mask.as_mut_slice().fill(255);
    let glyph = Arc::new(Glyph {
        id: 88888,
        size: Size {
            width: 18,
            height: 20,
        },
        origin: [0, 0],
        advance: [24, 0],
        levels: 256,
        mask,
    });
    let mut target = gpu.create_image(size, 0xff000000).unwrap();
    let mut admission = (0usize, 0usize);
    for row in 0..8 {
        for col in 0..25 {
            let text = run(
                vec![PlacedGlyph {
                    glyph: glyph.clone(),
                    x: 5 + col * 24,
                    y: 5 + row * 40,
                    color: 0xffffff,
                }],
                &budget,
            );
            let estimated = gpu.text_write_bytes(&target, &text, style(), size.rect());
            admission.0 += estimated;
            admission.1 += gpu.canvas_write_bytes(&target, false);
            let before = gpu.resident.used();
            gpu.draw_text(&mut target, &text, style(), size.rect())
                .unwrap();
            assert!(
                gpu.resident.used().saturating_sub(before) <= estimated,
                "text storage was underestimated at {col},{row}"
            );
        }
    }
    eprintln!(
        "paragraph admission: regional={} whole={}",
        admission.0, admission.1
    );
    assert!(admission.0 * 16 < admission.1);
    eprintln!("incremental paragraph: {target:?}");
    traffic::reset();
    let result = pixels(&gpu, &target);
    eprintln!("paragraph readback draws={}", traffic::draw_calls());
    assert!(
        traffic::draw_calls() < 100,
        "incremental writes fragmented the paragraph"
    );
    assert!(
        target.resident_bytes() < 850_000,
        "sparse text expanded to a full canvas"
    );
    for y in 0..size.height as usize {
        for x in 0..size.width as usize {
            let ink = x >= 5
                && y >= 5
                && (x - 5) / 24 < 25
                && (y - 5) / 40 < 8
                && (x - 5) % 24 < 18
                && (y - 5) % 40 < 20;
            let c = if ink { 254 } else { 0 };
            assert_eq!(
                &result[(y * 640 + x) * 4..][..4],
                &[c, c, c, 255],
                "{x},{y}"
            );
        }
    }
}

#[test]
fn atlas_admission_tracks_replacement_repeated_and_clipped_masks() {
    let context = support::Context::new();
    let size = Size {
        width: 64,
        height: 64,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                canvas_limit: Some(size),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let budget = Budget::new(8 * 1024 * 1024);
    let glyphs: Vec<_> = (1..=6)
        .map(|id| {
            let mut mask = Bytes::zeroed(510 * 510, &budget).unwrap();
            mask.as_mut_slice().fill(128);
            Arc::new(Glyph {
                id,
                size: Size {
                    width: 510,
                    height: 510,
                },
                origin: [-1, 1],
                advance: [24, 0],
                levels: 256,
                mask,
            })
        })
        .collect();
    let placed = |index: usize, x| PlacedGlyph {
        glyph: glyphs[index].clone(),
        x,
        y: 0,
        color: 0xffffff,
    };
    let mut target = gpu.create_image(size, 0xff000000).unwrap();
    for index in 0..4 {
        let text = run(vec![placed(index, 0)], &budget);
        let estimated = gpu.text_write_bytes(&target, &text, style(), size.rect());
        assert!(estimated >= 512 * 512 + 128);
        let before = gpu.resident.used();
        gpu.draw_text(&mut target, &text, style(), size.rect())
            .unwrap();
        assert!(gpu.resident.used().saturating_sub(before) <= estimated);
    }
    let cached = run(vec![placed(0, 0)], &budget);
    assert_eq!(
        gpu.text_write_bytes(&target, &cached, style(), size.rect()),
        0
    );
    // New page 5 evicts 1; reloading 1 evicts 2. The repeated mask stays
    // pinned, while the entirely clipped mask must not allocate a page.
    let text = run(
        vec![
            placed(4, 0),
            placed(0, 0),
            placed(4, 0),
            placed(1, 0),
            placed(5, 1000),
        ],
        &budget,
    );
    let estimate = gpu.text_write_bytes(&target, &text, style(), size.rect());
    assert_eq!(estimate, 3 * (512 * 512 + 128));
    let before = gpu.resident.used();
    gpu.draw_text(&mut target, &text, style(), size.rect())
        .unwrap();
    assert!(gpu.resident.used().saturating_sub(before) <= estimate);
    assert_eq!(
        gpu.text_write_bytes(&target, &text, style(), size.rect()),
        0
    );
    assert_eq!(
        gpu.text_write_bytes(
            &target,
            &text,
            Style {
                opacity: 0,
                ..style()
            },
            size.rect()
        ),
        0
    );
}

#[test]
fn compact_glyph_edges_do_not_sample_neighboring_atlas_masks() {
    for work in [false, true] {
        let context = support::Context::new();
        let logical = Size {
            width: 1920,
            height: 1080,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    canvas_limit: Some(Size {
                        width: 960,
                        height: 544,
                    }),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(logical);
        let budget = Budget::new(1024 * 1024);
        // Give the empty glyph nonzero immediate neighbors in the atlas.
        let solid = mask(9101, 256, &[255; 31], &budget);
        let empty = mask(9102, 256, &[0; 13], &budget);
        let trailing = mask(9103, 256, &[255; 31], &budget);
        let mut seed = gpu
            .create_image(
                Size {
                    width: 200,
                    height: 50,
                },
                0,
            )
            .unwrap();
        for glyph in [solid, empty.clone(), trailing] {
            let text = run(
                vec![PlacedGlyph {
                    glyph,
                    x: 0,
                    y: 0,
                    color: 0xffffff,
                }],
                &budget,
            );
            let clip = seed.size.rect();
            gpu.draw_text(&mut seed, &text, style(), clip).unwrap();
        }
        let mut target = gpu.create_image(logical, 0xff123456).unwrap();
        let before = pixels(&gpu, &target);
        let text = run(
            (0..100)
                .map(|i| PlacedGlyph {
                    glyph: empty.clone(),
                    x: 17 * i,
                    y: i + 10,
                    color: 0xffffff,
                })
                .collect(),
            &budget,
        );
        gpu.draw_text(&mut target, &text, style(), logical.rect())
            .unwrap();
        assert_eq!(
            pixels(&gpu, &target),
            before,
            "work={work}: empty glyph leaked atlas pixels"
        );
    }
}
fn mask(id: u64, levels: u16, values: &[u8], budget: &Budget) -> Arc<Glyph> {
    let mut data = Bytes::zeroed(values.len(), budget).unwrap();
    data.as_mut_slice().copy_from_slice(values);
    Arc::new(Glyph {
        id,
        size: Size {
            width: values.len() as u32,
            height: 1,
        },
        origin: [0, 0],
        advance: [values.len() as i32, 0],
        levels,
        mask: data,
    })
}
fn run(glyphs: Vec<PlacedGlyph>, budget: &Budget) -> Run {
    let permit = budget
        .reserve(glyphs.capacity() * std::mem::size_of::<PlacedGlyph>())
        .unwrap();
    Run { glyphs, permit }
}

#[test]
fn repeated_dialogue_glyphs_share_live_pixels_without_uploading_or_pinning_pages() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                small_canvas_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let budget = Budget::new(1024 * 1024);
    let size = Size {
        width: 16,
        height: 12,
    };
    let glyph = mask(9701, 256, &[0, 64, 128, 255, 128, 64, 0], &budget);
    let text = run(
        vec![PlacedGlyph {
            glyph,
            x: 3,
            y: 4,
            color: 0xffffff,
        }],
        &budget,
    );
    let draw = Style {
        face: DrawFace::Alpha,
        hold_alpha: false,
        ..style()
    };
    let mut first = gpu.create_image(size, 0).unwrap();
    gpu.draw_text(&mut first, &text, draw, size.rect()).unwrap();
    let expected = pixels(&gpu, &first);
    let mut second = gpu.create_image(size, 0).unwrap();
    assert_eq!(gpu.text_write_bytes(&second, &text, draw, size.rect()), 0);
    traffic::reset();
    gpu.draw_text(&mut second, &text, draw, size.rect())
        .unwrap();
    assert_eq!(traffic::texture_allocations(), 0);
    assert_eq!(traffic::loaded_pixels(), 0);
    assert_eq!(pixels(&gpu, &second), expected);
    // Editing a shared result must detach; the other character stays exact.
    gpu.fill(
        &mut first,
        &[krkr_protocol::graphics::Fill {
            rectangle: size.rect(),
            color: 0xff123456,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &second), expected);
    // Removing the last visible owner must release the reuse opportunity.
    drop(second);
    let blank = gpu.create_image(size, 0).unwrap();
    assert_ne!(gpu.text_write_bytes(&blank, &text, draw, size.rect()), 0);
}

#[test]
fn dialogue_glyph_reuse_distinguishes_clip_color_background_and_mutation() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                small_canvas_edge: 64,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let budget = Budget::new(1024 * 1024);
    let size = Size {
        width: 16,
        height: 12,
    };
    let glyph = mask(9702, 65, &[0, 16, 32, 64, 32, 16, 0], &budget);
    let make_run = |color| {
        run(
            vec![PlacedGlyph {
                glyph: glyph.clone(),
                x: 3,
                y: 4,
                color,
            }],
            &budget,
        )
    };
    let white = make_run(0xffffff);
    let draw = Style {
        face: DrawFace::Alpha,
        hold_alpha: false,
        ..style()
    };
    let mut first = gpu.create_image(size, 0).unwrap();
    gpu.draw_text(&mut first, &white, draw, size.rect())
        .unwrap();
    let expected = pixels(&gpu, &first);
    let blank = gpu.create_image(size, 0).unwrap();
    let clip = Rect {
        width: 6,
        ..size.rect()
    };
    assert_ne!(gpu.text_write_bytes(&blank, &white, draw, clip), 0);
    assert_ne!(
        gpu.text_write_bytes(&blank, &make_run(0xff0000), draw, size.rect()),
        0
    );
    let background = gpu.create_image(size, 0x40102030).unwrap();
    assert_ne!(
        gpu.text_write_bytes(&background, &white, draw, size.rect()),
        0
    );
    // A weak entry does not cause COW. Detect edits of a uniquely owned plane.
    gpu.fill(
        &mut first,
        &[krkr_protocol::graphics::Fill {
            rectangle: Rect {
                width: 1,
                height: 1,
                ..size.rect()
            },
            color: 0xffabcdef,
            face: DrawFace::Alpha,
            hold_alpha: false,
        }],
    )
    .unwrap();
    let mut restored = gpu.create_image(size, 0).unwrap();
    assert_ne!(
        gpu.text_write_bytes(&restored, &white, draw, size.rect()),
        0
    );
    gpu.draw_text(&mut restored, &white, draw, size.rect())
        .unwrap();
    assert_eq!(pixels(&gpu, &restored), expected);
    let mut held = gpu.create_image(size, 0x40102030).unwrap();
    gpu.draw_text(
        &mut held,
        &white,
        Style {
            hold_alpha: true,
            ..draw
        },
        size.rect(),
    )
    .unwrap();
    let mut normal = gpu.create_image(size, 0x40102030).unwrap();
    gpu.draw_text(&mut normal, &white, draw, size.rect())
        .unwrap();
    assert_eq!(pixels(&gpu, &held), pixels(&gpu, &normal));
}

#[test]
fn small_dialogue_layers_preserve_single_pixel_strokes_until_composition() {
    use krkr_protocol::graphics::{Blend, ImageRef, Node, Scene};
    use std::collections::HashMap;
    let context = support::Context::new();
    let logical = Size {
        width: 128,
        height: 64,
    };
    let physical = Size {
        width: 120,
        height: 60,
    };
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                canvas_limit: Some(physical),
                small_canvas_edge: 64,
                work_framebuffer: true,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let mut fonts = krkr_render::font::System::default();
    let font = krkr_protocol::text::Font {
        height: -26,
        ..Default::default()
    };
    let text = fonts
        .layout(
            &font,
            &"影".encode_utf16().collect::<Vec<_>>(),
            style(),
            2,
            2,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    let cell = Size {
        width: 32,
        height: 36,
    };
    let draw = || {
        let mut image = gpu.create_image(cell, 0xff000000).unwrap();
        gpu.draw_text(&mut image, &text, style(), cell.rect())
            .unwrap();
        image
    };
    // The reference is drawn at 1:1 before the output density is configured.
    let original = draw();
    gpu.set_canvas_size(logical);
    let compact = draw();
    let mut ids = slotmap::SlotMap::with_key();
    let reference = ImageRef {
        id: ids.insert(()),
        lifetime: Arc::default(),
    };
    let render = |image: Image| {
        let images = HashMap::from([(reference.id, image)]);
        let scene = Scene {
            nodes: [3, 38, 75]
                .map(|left| Node {
                    cache: None,
                    visible: true,
                    parent: None,
                    image: Some(reference.clone()),
                    neutral_color: 0,
                    rectangle: Rect {
                        left,
                        top: 9,
                        ..cell.rect()
                    },
                    image_left: 0,
                    image_top: 0,
                    blend: Blend::Opaque,
                    opacity: 255,
                })
                .to_vec(),
            ..Default::default()
        };
        pixels(
            &gpu,
            &gpu.scene_surface_scaled(logical, physical, &scene, &images)
                .unwrap(),
        )
    };
    assert_eq!(render(compact), render(original));
    // Two original-size glyph surfaces consume only 9 KiB, independent of
    // the screen size; full backgrounds still use compact canvas storage.
    assert!(
        gpu.create_image(logical, 0)
            .unwrap()
            .stored_size()
            .unwrap()
            .width
            < logical.width
    );
}

#[test]
fn text_batches_preserve_overlaps_pages_tiles_and_compact_sampling() {
    fn render(
        work: bool,
        singles: bool,
        compact: bool,
        face: DrawFace,
        opacity: i16,
        hold: bool,
    ) -> Vec<u8> {
        let context = support::Context::new();
        let size = Size {
            width: 97,
            height: 47,
        };
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    tile_edge: 16,
                    canvas_limit: compact.then_some(Size {
                        width: 49,
                        height: 24,
                    }),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        gpu.set_canvas_size(size);
        let budget = Budget::new(1024 * 1024);
        let make = |id, levels, width, height| {
            let size = Size { width, height };
            let mut data = Bytes::zeroed(size.rgba_bytes().unwrap() / 4, &budget).unwrap();
            for (i, byte) in data.as_mut_slice().iter_mut().enumerate() {
                *byte = ((i * 37 + i / width as usize * 19) % levels as usize) as u8;
            }
            Arc::new(Glyph {
                id,
                size,
                origin: [-1, 1],
                advance: [7, 0],
                levels,
                mask: data,
            })
        };
        // Oversize masks force two atlas pages, then switch back to page one.
        let wide = make(8001, 256, 513, 7);
        let tall = make(8002, 65, 5, 513);
        let small = make(8003, 256, 11, 9);
        let mut placements = vec![
            PlacedGlyph {
                glyph: wide.clone(),
                x: -13,
                y: 5,
                color: 0xd07030,
            },
            PlacedGlyph {
                glyph: tall,
                x: 15,
                y: -4,
                color: 0x2060e0,
            },
            PlacedGlyph {
                glyph: wide,
                x: -7,
                y: 8,
                color: 0x8040b0,
            },
        ];
        placements.extend((0..40).map(|i| PlacedGlyph {
            glyph: small.clone(),
            x: i % 10 * 8,
            y: 9 + i / 10 * 5,
            color: 0x407020 + i as u32 * 0x030103,
        }));
        let tiny = make(8004, 65, 3, 4);
        placements.extend((0..20).map(|i| PlacedGlyph {
            glyph: tiny.clone(),
            x: 4 + i % 10 * 7,
            y: 12 + i / 10 * 14,
            color: 0xd0b090,
        }));
        let text = run(placements, &budget);
        let mut target = gpu.create_image(size, 0x79406080).unwrap();
        let clip = Rect {
            left: 3,
            top: 2,
            width: 89,
            height: 41,
        };
        let style = Style {
            face,
            opacity,
            hold_alpha: hold,
            ..style()
        };
        if singles {
            for placed in &text.glyphs {
                let one = run(
                    vec![PlacedGlyph {
                        glyph: placed.glyph.clone(),
                        x: placed.x,
                        y: placed.y,
                        color: placed.color,
                    }],
                    &budget,
                );
                gpu.draw_text(&mut target, &one, style, clip).unwrap();
            }
        } else {
            gpu.draw_text(&mut target, &text, style, clip).unwrap();
        }
        pixels(&gpu, &target)
    }
    for compact in [false, true] {
        for (face, opacity) in [
            (DrawFace::Opaque, 255),
            (DrawFace::Opaque, 113),
            (DrawFace::Alpha, 255),
            (DrawFace::Alpha, 137),
            (DrawFace::Alpha, -129),
            (DrawFace::AddAlpha, 255),
            (DrawFace::AddAlpha, 91),
        ] {
            for hold in [false, true] {
                let expected = render(false, true, compact, face, opacity, hold);
                assert_eq!(
                    render(true, false, compact, face, opacity, hold),
                    expected,
                    "compact={compact} face={face:?} opacity={opacity} hold={hold}"
                );
            }
        }
    }
}

#[test]
fn character_shadows_reuse_the_quad_without_vertex_uploads() {
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
    let budget = Budget::new(32768);
    let glyph = mask(8200, 65, &[0, 32, 64, 32], &budget);
    let size = Size {
        width: 16,
        height: 8,
    };
    for same_color in [false, true] {
        let placements = || {
            vec![
                PlacedGlyph {
                    glyph: glyph.clone(),
                    x: 3,
                    y: 2,
                    color: 0x104080,
                },
                PlacedGlyph {
                    glyph: glyph.clone(),
                    x: 2,
                    y: 2,
                    color: if same_color { 0x104080 } else { 0xffffff },
                },
            ]
        };
        let text = run(placements(), &budget);
        let mut reference = gpu.create_image(size, 0x79506070).unwrap();
        for placed in placements() {
            gpu.draw_text(
                &mut reference,
                &run(vec![placed], &budget),
                style(),
                size.rect(),
            )
            .unwrap();
        }
        let expected = pixels(&gpu, &reference);
        let mut target = gpu.create_image(size, 0x79506070).unwrap();
        gpu.flush().unwrap();
        traffic::reset();
        gpu.draw_text(&mut target, &text, style(), size.rect())
            .unwrap();
        assert_eq!(
            traffic::buffer_uploads(),
            0,
            "singleton batches uploaded vertices"
        );
        assert_eq!(pixels(&gpu, &target), expected);
    }
}

#[test]
fn adjacent_glyph_batch_loads_once_without_per_glyph_backdrop_copies() {
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
    let budget = Budget::new(32768);
    let glyph = mask(8100, 65, &[0, 32, 64, 32], &budget);
    let text = run(
        (0..32)
            .map(|i| PlacedGlyph {
                glyph: glyph.clone(),
                x: i * 5,
                y: 1,
                color: 0x8040c0,
            })
            .collect(),
        &budget,
    );
    let size = Size {
        width: 192,
        height: 4,
    };
    let mut target = gpu.create_image(size, 0xff102030).unwrap();
    // Warm the atlas and program on another image, leaving this destination
    // backed but absent from the current work surface.
    let mut warm = gpu.create_image(size, 0).unwrap();
    gpu.draw_text(&mut warm, &text, style(), size.rect())
        .unwrap();
    gpu.flush().unwrap();
    traffic::reset();
    gpu.draw_text(&mut target, &text, style(), size.rect())
        .unwrap();
    assert_eq!(traffic::load_calls(), 1);
    assert_eq!(traffic::loaded_pixels(), 159);
    assert_eq!(traffic::draw_calls(), 2); // One background load, one glyph batch.
    assert_eq!(traffic::store_calls(), 0);
    gpu.flush().unwrap();
    assert_eq!(traffic::store_calls(), 1);
    assert_eq!(traffic::stored_pixels(), 159);
}
#[test]
fn fresh_text_background_is_resolved_once_before_the_glyphs() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let budget = Budget::new(32768);
    let glyph = mask(8101, 65, &[0, 32, 64, 32], &budget);
    let text = run(
        (0..32)
            .map(|i| PlacedGlyph {
                glyph: glyph.clone(),
                x: i * 5 + 8,
                y: 1,
                color: 0xff0000,
            })
            .collect(),
        &budget,
    );
    let size = Size {
        width: 192,
        height: 4,
    };
    let mut warm = gpu.create_image(size, 0).unwrap();
    gpu.draw_text(&mut warm, &text, style(), size.rect())
        .unwrap();
    gpu.flush().unwrap();
    // Unlike the backed destination above, this fill is still in the work
    // surface. Resolve the line once, not separately for every character.
    let mut target = gpu.create_image(size, 0xff102030).unwrap();
    traffic::reset();
    gpu.draw_text(&mut target, &text, style(), size.rect())
        .unwrap();
    assert_eq!(traffic::store_calls(), 1);
    assert_eq!(traffic::stored_pixels(), 159);
    let actual = pixels(&gpu, &target);
    let mut expected = [16, 32, 48, 255].repeat(192 * 4);
    for i in 0..32 {
        for (x, pixel) in [
            [16, 32, 48, 255],
            [135, 16, 24, 255],
            [255, 0, 0, 255],
            [135, 16, 24, 255],
        ]
        .iter()
        .enumerate()
        {
            let offset = (192 + 8 + i * 5 + x) * 4;
            expected[offset..offset + 4].copy_from_slice(pixel);
        }
    }
    assert_eq!(actual, expected);
}

#[test]
fn long_glyph_batches_preserve_overlaps_across_buffer_boundaries() {
    fn render(work: bool) -> Vec<u8> {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl(),
                Config {
                    work_framebuffer: work,
                    ..Config::default()
                },
            )
            .unwrap()
        };
        let budget = Budget::new(32768);
        let glyph = mask(8200, 65, &[0, 32, 64, 32], &budget);
        let mut glyphs: Vec<_> = (0..160)
            .map(|i| PlacedGlyph {
                glyph: glyph.clone(),
                x: 5 + (i % 80) * 5,
                y: 1 + (i / 80) * 3,
                color: 0x70b040,
            })
            .collect();
        // Revisit the start after several chunks, then overlap it again with
        // another color. Each pass must sample the preceding result.
        for (x, color) in [(5, 0x70b040), (6, 0xe02040)] {
            glyphs.push(PlacedGlyph {
                glyph: glyph.clone(),
                x,
                y: 1,
                color,
            });
        }
        let text = run(glyphs, &budget);
        let size = Size {
            width: 512,
            height: 8,
        };
        let mut target = gpu.create_image(size, 0x79406080).unwrap();
        gpu.draw_text(
            &mut target,
            &text,
            Style {
                face: DrawFace::AddAlpha,
                opacity: 193,
                ..style()
            },
            size.rect(),
        )
        .unwrap();
        pixels(&gpu, &target)
    }
    assert_eq!(render(true), render(false));
}

#[test]
fn cached_atlas_masks_overlap_clip_and_failed_upload_are_ordered() {
    for work in [false, true] {
        check_atlas(work);
    }
}

#[test]
fn glyph_faces_and_signed_opacity_keep_byte_results() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: true,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let budget = Budget::new(4096);
    let size = Size {
        width: 1,
        height: 1,
    };
    // Both atlas encodings represent half coverage. Expected bytes also
    // exercise the distinct /64 and /256 rounding at full coverage.
    for (levels, half, full, full_rgb) in [
        (65, 32, 64, [240, 80, 16, 255]),
        (256, 128, 255, [239, 79, 15, 255]),
    ] {
        for (coverage, face, opacity, expected) in [
            (half, DrawFace::Opaque, 255, [152, 56, 16, 0]),
            (half, DrawFace::AddAlpha, 255, [151, 55, 15, 178]),
            (half, DrawFace::AddAlpha, 128, [107, 43, 15, 139]),
            (full, DrawFace::AddAlpha, 255, full_rgb),
            (half, DrawFace::Alpha, -128, [64, 32, 16, 74]),
        ] {
            let text = run(
                vec![PlacedGlyph {
                    glyph: mask(
                        u64::from(levels) * 256 + u64::from(coverage),
                        levels,
                        &[coverage],
                        &budget,
                    ),
                    x: 0,
                    y: 0,
                    color: 0xf05010,
                }],
                &budget,
            );
            let mut target = gpu.create_image(size, 0x64402010).unwrap();
            gpu.draw_text(
                &mut target,
                &text,
                Style {
                    face,
                    opacity,
                    hold_alpha: false,
                    ..style()
                },
                size.rect(),
            )
            .unwrap();
            assert_eq!(
                pixels(&gpu, &target),
                expected,
                "levels={levels} coverage={coverage} face={face:?} opacity={opacity}"
            );
        }
    }
}
fn check_atlas(work: bool) {
    let context = support::Context::new();
    let mut gpu = unsafe {
        Gpu::new(
            context.gl(),
            Config {
                work_framebuffer: work,
                tile_edge: 16,
                ..Config::default()
            },
        )
        .unwrap()
    };
    let budget = Budget::new(4096);
    let glyph = mask(1001, 256, &[0, 128, 255], &budget);
    let text = run(
        vec![
            PlacedGlyph {
                glyph: glyph.clone(),
                x: 0,
                y: 0,
                color: 0xff0000,
            },
            PlacedGlyph {
                glyph: glyph.clone(),
                x: 3,
                y: 0,
                color: 0xff0000,
            },
        ],
        &budget,
    );
    let size = Size {
        width: 6,
        height: 1,
    };
    let mut target = gpu.create_image(size, 0x64404040).unwrap();
    let original = target.shared();
    let before = gpu.resident.used();
    gpu.draw_text(&mut target, &text, style(), size.rect())
        .unwrap();
    assert_eq!(
        pixels(&gpu, &target),
        [64, 64, 64, 100, 159, 32, 32, 100, 254, 0, 0, 100].repeat(2)
    );
    assert_eq!(pixels(&gpu, &original), [64, 64, 64, 100].repeat(6));
    let cached = gpu.resident.used();
    // One byte per atlas texel, one shared glyph entry, one detached target.
    assert_eq!(
        cached - before,
        512 * 512 + 128 + size.rgba_bytes().unwrap()
    );
    gpu.draw_text(
        &mut target,
        &text,
        style(),
        Rect {
            left: 3,
            top: 0,
            width: 3,
            height: 1,
        },
    )
    .unwrap();
    let p = pixels(&gpu, &target);
    assert_eq!(
        &p[..12],
        [64, 64, 64, 100, 159, 32, 32, 100, 254, 0, 0, 100]
    );
    assert_eq!(&p[16..20], [207, 16, 16, 100]);
    assert_eq!(gpu.resident.used(), cached);
    let half = mask(1002, 65, &[32], &budget);
    let overlap = run(
        vec![
            PlacedGlyph {
                glyph: half.clone(),
                x: 0,
                y: 0,
                color: 0xff0000,
            },
            PlacedGlyph {
                glyph: half,
                x: 0,
                y: 0,
                color: 0xff0000,
            },
        ],
        &budget,
    );
    let mut single = gpu
        .create_image(
            Size {
                width: 1,
                height: 1,
            },
            0xff000000,
        )
        .unwrap();
    gpu.draw_text(
        &mut single,
        &overlap,
        style(),
        Size {
            width: 1,
            height: 1,
        }
        .rect(),
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &single), [191, 0, 0, 255]);
    let solid = mask(1003, 65, &[64], &budget);
    let negative = run(
        vec![PlacedGlyph {
            glyph: solid,
            x: 0,
            y: 0,
            color: 0xffffff,
        }],
        &budget,
    );
    gpu.draw_text(
        &mut single,
        &negative,
        Style {
            opacity: -255,
            face: DrawFace::Alpha,
            ..style()
        },
        Size {
            width: 1,
            height: 1,
        }
        .rect(),
    )
    .unwrap();
    assert_eq!(pixels(&gpu, &single), [191, 0, 0, 0]);
    // A real atlas admission failure keeps the image unchanged.
    gpu.collect().unwrap();
    let old = gpu.resident.clone();
    gpu.resident = Budget::new(0);
    let fresh = run(
        vec![PlacedGlyph {
            glyph: mask(1004, 65, &[64], &budget),
            x: 0,
            y: 0,
            color: 0x00ff00,
        }],
        &budget,
    );
    assert!(
        gpu.draw_text(&mut single, &fresh, style(), size.rect())
            .is_err()
    );
    assert_eq!(pixels(&gpu, &single), [191, 0, 0, 0]);
    gpu.resident = old;
    let scratch = gpu.scratch.clone();
    gpu.scratch = Budget::new(0);
    // The persistent work target already supplies a backdrop. Only direct
    // FBO drawing needs to reserve an additional scratch texture per glyph.
    assert_eq!(
        gpu.draw_text(&mut single, &fresh, style(), size.rect())
            .is_ok(),
        work
    );
    gpu.scratch = scratch;
    gpu.draw_text(
        &mut single,
        &fresh,
        Style {
            face: DrawFace::Alpha,
            ..style()
        },
        size.rect(),
    )
    .unwrap();
    // The work-target probe above already applied this glyph once; the
    // second blend rounds its green channel up by the remaining byte.
    assert_eq!(
        pixels(&gpu, &single),
        if work {
            [0, 255, 0, 255]
        } else {
            [0, 254, 0, 255]
        }
    );
    // Curved outlines on a transparent destination retain the full coverage
    // mask, including counters and transparent corners (not just opaque boxes).
    let mut fonts = krkr_render::font::System::default();
    let font = krkr_protocol::text::Font {
        file: true,
        face: "fixture".into(),
        height: 32,
        ..Default::default()
    };
    let data = include_bytes!("../../krkr-render/tests/fixtures/fonts/fixture.ttf").to_vec();
    let permit = fonts.reserve(data.len()).unwrap();
    fonts.insert_face(
        &font,
        krkr_render::font::Face::from_bytes(data, 0, permit).unwrap(),
    );
    let text = fonts
        .layout(
            &font,
            &[79, 79],
            style(),
            2,
            2,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
    let size = Size {
        width: 48,
        height: 48,
    };
    let mut target = gpu.create_image(size, 0x00ffffff).unwrap();
    gpu.draw_text(
        &mut target,
        &text,
        Style {
            face: DrawFace::Alpha,
            ..style()
        },
        size.rect(),
    )
    .unwrap();
    let pixels = pixels(&gpu, &target);
    for y in 0..48 {
        for x in 0..48 {
            let expected = text
                .glyphs
                .iter()
                .find_map(|p| {
                    let g = &p.glyph;
                    let lx = x as i32 - p.x - g.origin[0];
                    let ly = y as i32 - p.y - g.origin[1];
                    if lx >= 0 && ly >= 0 && lx < g.size.width as i32 && ly < g.size.height as i32 {
                        Some(g.mask.as_slice()[ly as usize * g.size.width as usize + lx as usize])
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            assert_eq!(pixels[(y * 48 + x) * 4 + 3], expected, "mask at {x},{y}");
        }
    }
}
#[test]
fn fresh_character_tiles_match_gpu_masks_without_draws_or_readbacks() {
    let size = Size {
        width: 34,
        height: 34,
    };
    let budget = Budget::new(1024 * 1024);
    let mut expected = Vec::new();
    for fast in [false, true] {
        let context = support::Context::new();
        let gpu = unsafe {
            Gpu::new(
                context.gl_with(traffic::intercept),
                Config {
                    work_framebuffer: true,
                    small_canvas_edge: if fast { 64 } else { 0 },
                    ..Default::default()
                },
            )
            .unwrap()
        };
        let mut totals = (0, 0, 0);
        let mut case = 0;
        for levels in [65, 256] {
            let mut data = Bytes::zeroed(26 * 26, &budget).unwrap();
            for (i, p) in data.as_mut_slice().iter_mut().enumerate() {
                *p = (i % usize::from(levels)) as u8;
            }
            let glyph = Arc::new(Glyph {
                id: u64::from(levels),
                size: Size {
                    width: 26,
                    height: 26,
                },
                origin: [-2, 1],
                advance: [26, 0],
                levels,
                mask: data,
            });
            for alpha in [0, 1, 31, 127, 254, 255] {
                for clipped in [false, true] {
                    let background = (alpha << 24) | 0x795020;
                    let mut target = gpu.create_image(size, background).unwrap();
                    let blank = target.shared();
                    let text = run(
                        vec![
                            PlacedGlyph {
                                glyph: glyph.clone(),
                                x: 5,
                                y: 4,
                                color: 0x103078,
                            },
                            PlacedGlyph {
                                glyph: glyph.clone(),
                                x: 4,
                                y: 3,
                                color: 0xffffe0,
                            },
                        ],
                        &budget,
                    );
                    let clip = if clipped {
                        Rect {
                            left: 8,
                            top: 9,
                            width: 14,
                            height: 13,
                        }
                    } else {
                        size.rect()
                    };
                    let style = Style {
                        face: DrawFace::Alpha,
                        ..style()
                    };
                    let estimate = gpu.text_write_bytes(&target, &text, style, clip);
                    let before = gpu.resident.used();
                    gpu.flush().unwrap();
                    traffic::reset();
                    gpu.draw_text(&mut target, &text, style, clip).unwrap();
                    gpu.flush().unwrap();
                    assert!(gpu.resident.used().saturating_sub(before) <= estimate);
                    totals.0 += traffic::draw_calls();
                    totals.1 += traffic::store_calls();
                    totals.2 += traffic::read_calls();
                    if fast {
                        assert_eq!(
                            (
                                traffic::draw_calls(),
                                traffic::store_calls(),
                                traffic::read_calls()
                            ),
                            (0, 0, 0)
                        );
                    }
                    let actual = pixels(&gpu, &target);
                    if fast {
                        assert_eq!(
                            actual, expected[case],
                            "levels={levels} alpha={alpha} clipped={clipped}"
                        );
                    } else {
                        expected.push(actual);
                    }
                    assert_eq!(
                        pixels(&gpu, &blank),
                        [0x79, 0x50, 0x20, alpha as u8].repeat(34 * 34)
                    );
                    case += 1;
                }
            }
        }
        eprintln!("fresh character fast={fast}: draws/stores/readbacks={totals:?}");
    }
}
#[test]
fn character_tile_failures_and_empty_clips_leave_the_blank_image_unchanged() {
    let context = support::Context::new();
    let gpu = unsafe {
        Gpu::new(
            context.gl_with(traffic::intercept),
            Config {
                work_framebuffer: true,
                small_canvas_edge: 64,
                canvas_limit: Some(Size {
                    width: 960,
                    height: 544,
                }),
                ..Default::default()
            },
        )
        .unwrap()
    };
    let size = Size {
        width: 34,
        height: 34,
    };
    let budget = Budget::new(4096);
    let text = run(
        vec![PlacedGlyph {
            glyph: mask(777, 65, &[0, 32, 64], &budget),
            x: 2,
            y: 2,
            color: 0xffffff,
        }],
        &budget,
    );
    let style = Style {
        face: DrawFace::Alpha,
        ..style()
    };
    let mut target = gpu.create_image(size, 0x79123456).unwrap();
    let blank = target.shared();
    let expected = pixels(&gpu, &blank);
    let empty = Rect {
        left: 24,
        top: 24,
        width: 4,
        height: 4,
    };
    traffic::reset();
    assert_eq!(gpu.text_write_bytes(&target, &text, style, empty), 0);
    gpu.draw_text(&mut target, &text, style, empty).unwrap();
    assert_eq!(
        (traffic::texture_allocations(), traffic::draw_calls()),
        (0, 0)
    );
    // The CPU buffer is temporary and a rejected admission cannot replace the
    // plane held by this image or by the already published alias.
    let reserve = gpu.staging.reserve(gpu.staging.available()).unwrap();
    assert!(
        gpu.draw_text(&mut target, &text, style, size.rect())
            .is_err()
    );
    drop(reserve);
    assert_eq!(pixels(&gpu, &target), expected);
    assert_eq!(pixels(&gpu, &blank), expected);
    gpu.draw_text(&mut target, &text, style, size.rect())
        .unwrap();
    assert_ne!(pixels(&gpu, &target), expected);
    assert_eq!(pixels(&gpu, &blank), expected);
    // Editing an already drawn layer must fall back to blending its real
    // backdrop, not reuse the old clear color.
    let previous = pixels(&gpu, &target);
    gpu.draw_text(&mut target, &text, style, size.rect())
        .unwrap();
    let twice = pixels(&gpu, &target);
    assert_ne!(twice, previous);
    assert_eq!(&twice[(2 * 34 + 4) * 4..][..4], &[254, 254, 254, 255]);
}

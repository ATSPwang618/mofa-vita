use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Rect, Size},
    pixels::Bytes,
    text::{Glyph, PlacedGlyph, Run, Style},
};
use krkr_render_wgpu::{Gpu, Image};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
fn pixels(gpu: &Gpu, image: &Image) -> Vec<u8> {
    let mut read = gpu.readback(image, image.size.rect(), false).unwrap();
    let start = Instant::now();
    loop {
        gpu.poll().unwrap();
        if let Some(r) = read.take() {
            return r.unwrap().data.as_slice().to_vec();
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
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
#[ignore = "requires a real desktop GPU"]
fn cached_atlas_masks_overlap_clip_and_failed_upload_are_ordered() {
    let mut gpu = pollster::block_on(Gpu::new(
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
        None,
    ))
    .unwrap();
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
    gpu.draw_text(&mut target, &text, style(), size.rect())
        .unwrap();
    assert_eq!(
        pixels(&gpu, &target),
        [64, 64, 64, 100, 159, 32, 32, 100, 254, 0, 0, 100].repeat(2)
    );
    assert_eq!(pixels(&gpu, &original), [64, 64, 64, 100].repeat(6));
    assert_eq!(gpu.uploaded_glyphs(), 1);
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
    assert_eq!(gpu.uploaded_glyphs(), 1);
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
    // A failed scratch reservation must roll back atlas entries that have not
    // been uploaded; retry uploads the mask instead of reading uninitialized R8.
    gpu.trim_scratch();
    let old = gpu.scratch.clone();
    gpu.scratch = Budget::new(0);
    let fresh = run(
        vec![PlacedGlyph {
            glyph: mask(1004, 65, &[64], &budget),
            x: 0,
            y: 0,
            color: 0x00ff00,
        }],
        &budget,
    );
    let uploaded = gpu.uploaded_glyphs();
    assert!(
        gpu.draw_text(&mut single, &fresh, style(), size.rect())
            .is_err()
    );
    assert_eq!(gpu.uploaded_glyphs(), uploaded);
    gpu.scratch = old;
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
    assert_eq!(pixels(&gpu, &single), [0, 254, 0, 255]);
    assert_eq!(gpu.uploaded_glyphs(), uploaded + 1);
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

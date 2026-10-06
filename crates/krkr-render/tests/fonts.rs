use krkr_protocol::{
    budget::Budget,
    graphics::DrawFace,
    text::{Font, Style},
};
use krkr_render::font::{Face, System, prerendered};
use std::sync::{Arc, atomic::AtomicBool};
fn system() -> (System, Font) {
    let mut s = System::default();
    let f = Font {
        height: 100,
        file: true,
        face: "fixture".into(),
        ..Default::default()
    };
    let bytes = include_bytes!("fixtures/fonts/fixture.ttf").to_vec();
    let permit = s.reserve(bytes.len()).unwrap();
    s.insert_face(&f, Face::from_bytes(bytes, 0, permit).unwrap());
    (s, f)
}
fn style() -> Style {
    Style {
        color: 0xffffff,
        opacity: 255,
        antialias: true,
        shadow_level: 0,
        shadow_color: 0,
        shadow_width: 0,
        shadow_offset: [0, 0],
        face: DrawFace::Alpha,
        hold_alpha: false,
    }
}

#[test]
fn borrowed_layout_queries_distinguish_font_raster_and_shadow_settings() {
    let (mut system, base) = system();
    let stop = AtomicBool::new(false);
    let text = [65, 32, 66, 65];
    for height in [24, 40] {
        for bold in [false, true] {
            let font = Font {
                height,
                bold,
                ..base.clone()
            };
            for antialias in [false, true] {
                for shadow_width in [0, 1] {
                    let style = Style {
                        antialias,
                        shadow_width,
                        shadow_level: 128,
                        ..style()
                    };
                    assert!(
                        system
                            .layout_cached(&font, &text, style, [7, 9], None)
                            .is_none()
                    );
                    let expected = system.layout(&font, &text, style, 7, 9, &stop).unwrap();
                    let cached = system
                        .layout_cached(&font, &text, style, [7, 9], None)
                        .unwrap();
                    assert_eq!(cached.glyphs.len(), expected.glyphs.len());
                    for (a, b) in cached.glyphs.iter().zip(&expected.glyphs) {
                        assert_eq!(
                            (a.x, a.y, a.color, a.glyph.id),
                            (b.x, b.y, b.color, b.glyph.id)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn cached_bounds_follow_styles_face_replacement_and_budget_eviction() {
    let stop = AtomicBool::new(false);
    let (mut cached, base) = system();
    for height in [0, 24, 72] {
        for angle in [0, 450, 900] {
            let font = Font {
                height,
                angle,
                underline: true,
                strikeout: true,
                ..base.clone()
            };
            let (mut fresh, _) = system();
            for text in [&[65, 66, 32, 65][..], &[0xd800, 65, 0, 66][..]] {
                let expected = fresh.measure(&font, text, true, &stop).unwrap();
                for _ in 0..3 {
                    let result = cached.measure(&font, text, true, &stop).unwrap();
                    assert_eq!(
                        (result.width, result.height, result.bounds),
                        (expected.width, expected.height, expected.bounds)
                    );
                }
            }
        }
    }
    let before = cached.measure(&base, &[65], true, &stop).unwrap();
    let bytes = include_bytes!("fixtures/fonts/fixture.ttf").to_vec();
    let permit = cached.reserve(bytes.len()).unwrap();
    let mut replacement = Face::from_bytes(bytes, 0, permit).unwrap();
    replacement.ascent += 200.;
    cached.insert_face(&base, replacement);
    let after = cached.measure(&base, &[65], true, &stop).unwrap();
    assert_ne!(before.bounds, after.bounds);
    let used = cached.budget.used();
    for _ in 0..20 {
        cached.measure(&base, &[65], true, &stop).unwrap();
    }
    assert_eq!(cached.budget.used(), used);
    let pressure = cached.budget.reserve(cached.budget.available()).unwrap();
    let reclaimed = cached
        .reserve(16)
        .expect("measurement metadata can be evicted");
    drop((pressure, reclaimed));
    let rebuilt = cached.measure(&base, &[65], true, &stop).unwrap();
    assert_eq!(rebuilt.bounds, after.bounds);
}

#[test]
fn changing_one_font_mapping_preserves_other_fonts_and_their_shadows() {
    let (mut system, font) = system();
    let other = Font {
        height: 40,
        ..font.clone()
    };
    let stop = AtomicBool::new(false);
    let style = Style {
        shadow_level: 128,
        shadow_width: 1,
        ..style()
    };
    let unrelated = system
        .layout(&other, &[65, 66], style, 0, 0, &stop)
        .unwrap();
    let original = system.layout(&font, &[65], style, 0, 0, &stop).unwrap();
    let bytes = include_bytes!("fixtures/fonts/fixture-v1.tft");
    let permit = system.reserve(bytes.len()).unwrap();
    system.map(
        font.clone(),
        prerendered::Font::parse(bytes.to_vec(), permit).unwrap(),
    );
    let mapped = system.layout(&font, &[65], style, 0, 0, &stop).unwrap();
    assert!(!Arc::ptr_eq(
        &original.glyphs[1].glyph,
        &mapped.glyphs[1].glyph
    ));
    assert_eq!(mapped.glyphs[1].glyph.levels, 65);
    let cached = system
        .layout_cached(&other, &[65, 66], style, [10, 20], None)
        .expect("mapping another font must preserve all cached masks");
    for (a, b) in unrelated.glyphs.iter().zip(&cached.glyphs) {
        assert!(Arc::ptr_eq(&a.glyph, &b.glyph));
    }
    system.unmap(&font);
    let restored = system.layout(&font, &[65], style, 0, 0, &stop).unwrap();
    assert_eq!(
        restored.glyphs[1].glyph.mask.as_slice(),
        original.glyphs[1].glyph.mask.as_slice()
    );
    assert_eq!(restored.glyphs[1].glyph.levels, 256);
    // Repeated unmaps are harmless, including to the now-unmapped font itself.
    system.unmap(&font);
    let cached = system
        .layout_cached(&font, &[65], style, [0, 0], None)
        .unwrap();
    assert!(Arc::ptr_eq(
        &restored.glyphs[1].glyph,
        &cached.glyphs[1].glyph
    ));
    assert!(
        system
            .layout_cached(&other, &[65, 66], style, [0, 0], None)
            .is_some()
    );
}
#[test]
fn bold_and_shadow_masks_evict_cached_glyphs_under_byte_pressure() {
    let budget = Budget::new(96 * 1024);
    let mut limited = System::new(budget.clone());
    let (mut reference, mut font) = system();
    font.height = 40;
    font.bold = true;
    let bytes = include_bytes!("fixtures/fonts/fixture.ttf").to_vec();
    let permit = limited.reserve(bytes.len()).unwrap();
    limited.insert_face(&font, Face::from_bytes(bytes, 0, permit).unwrap());
    let stop = AtomicBool::new(false);
    let styled = Style {
        shadow_level: 180,
        shadow_width: 3,
        ..style()
    };
    // Different angles create distinct cached masks but each live run is tiny.
    // Both bold expansion and blurred shadows must use the LRU admission path.
    for angle in (0..3600).step_by(15) {
        font.angle = angle;
        let actual = limited
            .layout(&font, &[65, 66], styled, 0, 0, &stop)
            .unwrap();
        let expected = reference
            .layout(&font, &[65, 66], styled, 0, 0, &stop)
            .unwrap();
        for (a, b) in actual.glyphs.iter().zip(&expected.glyphs) {
            assert_eq!(a.glyph.mask.as_slice(), b.glyph.mask.as_slice());
            assert_eq!(a.glyph.origin, b.glyph.origin);
        }
        assert!(budget.used() <= budget.limit());
    }
}
#[test]
fn default_font_fits_kag_line_height_and_uses_typographic_baseline() {
    let mut system = System::default();
    let font = Font {
        height: 24,
        ..Default::default()
    };
    let text: Vec<_> = "存档恢复后，文字与音频继续执行。Agjpqy日本語"
        .encode_utf16()
        .collect();
    let stop = AtomicBool::new(false);
    let bounds = system.measure(&font, &text, true, &stop).unwrap().bounds;
    // KAG's unchanged MessageLayer uses y=12 in a 40px line image, leaving
    // four pixels below its 24px character cell for outlines/shadows.
    assert!(12 + bounds.top + bounds.height as i32 <= 40);
    let run = system.layout(&font, &text, style(), 4, 12, &stop).unwrap();
    assert!(
        run.glyphs
            .iter()
            .all(|g| g.y + g.glyph.origin[1] + g.glyph.size.height as i32 <= 40)
    );
    // The bundled font's OTM ascent at 24px is 21 (confirmed with GDI),
    // whereas its hhea/windows clipping ascent is 28.
    assert_eq!(bounds.top, 1);
}
#[test]
fn metrics_cached_masks_styles_and_prerendered_replacement_share_font_selection() {
    let (mut system, mut font) = system();
    let stop = AtomicBool::new(false);
    let text: Vec<u16> = "AB A\0B".encode_utf16().collect();
    let metrics = system.measure(&font, &text, false, &stop).unwrap();
    assert_eq!((metrics.width, metrics.height), (190, 100));
    assert_eq!(system.rasterized(), 0);
    let bounds = system
        .measure(&font, &[65, 66], true, &stop)
        .unwrap()
        .bounds;
    assert_eq!(
        (bounds.left, bounds.top, bounds.width, bounds.height),
        (0, 10, 90, 70)
    );
    let a = system
        .layout(&font, &[65, 32, 65], style(), 0, 0, &stop)
        .unwrap();
    assert_eq!(a.glyphs.len(), 2);
    assert!(Arc::ptr_eq(&a.glyphs[0].glyph, &a.glyphs[1].glyph));
    assert_eq!(a.glyphs[1].x, 90);
    assert!(a.glyphs[0].glyph.mask.as_slice().iter().all(|&v| v == 255));
    let count = system.rasterized();
    let b = system
        .layout(
            &font,
            &[65, 32, 65],
            Style {
                color: 0xff0000,
                ..style()
            },
            0,
            0,
            &stop,
        )
        .unwrap();
    assert_eq!(system.rasterized(), count);
    assert_eq!(a.glyphs[0].glyph.id, b.glyphs[0].glyph.id);
    let rounded = system.layout(&font, &[79], style(), 0, 0, &stop).unwrap();
    let g = &rounded.glyphs[0].glyph;
    assert_eq!(g.mask.as_slice()[35 * g.size.width as usize + 25], 0);
    assert_eq!(g.mask.as_slice()[35 * g.size.width as usize + 5], 255);
    assert!(g.mask.as_slice().iter().any(|&v| v > 0 && v < 255));
    font.underline = true;
    font.strikeout = true;
    let decorated = system
        .layout(&font, &[79, 67], style(), 0, 0, &stop)
        .unwrap();
    let left = &decorated.glyphs[0].glyph;
    let right = &decorated.glyphs[1].glyph;
    assert_eq!(left.size, right.size);
    assert!(
        left.mask
            .as_slice()
            .iter()
            .zip(right.mask.as_slice())
            .all(|(a, b)| a.abs_diff(*b) <= 1)
    );
    assert_eq!(
        left.mask.as_slice()[(56 - left.origin[1]) as usize * left.size.width as usize + 25],
        255
    );
    font.underline = false;
    font.strikeout = false;
    font.angle = 900;
    let rotated = system
        .layout(&font, &[65, 65], style(), 0, 0, &stop)
        .unwrap();
    assert_eq!(rotated.glyphs[1].y, -60);
    assert_eq!(rotated.glyphs[0].glyph.size.width, 70);
    font.angle = 0;
    let shadow = system
        .layout(
            &font,
            &[65],
            Style {
                shadow_level: 128,
                shadow_width: 1,
                shadow_offset: [2, 3],
                ..style()
            },
            0,
            0,
            &stop,
        )
        .unwrap();
    assert_eq!(shadow.glyphs.len(), 2);
    assert_eq!(shadow.glyphs[0].glyph.size.width, 52);
    assert_eq!(shadow.glyphs[0].x, 2);
    for bytes in [
        include_bytes!("fixtures/fonts/fixture-v0.tft").as_slice(),
        include_bytes!("fixtures/fonts/fixture-v1.tft").as_slice(),
    ] {
        let permit = system.reserve(bytes.len()).unwrap();
        system.map(
            font.clone(),
            prerendered::Font::parse(bytes.to_vec(), permit).unwrap(),
        );
        assert_eq!(
            system
                .measure(&font, &[65, 65], false, &stop)
                .unwrap()
                .width,
            14
        );
        assert_eq!(
            system
                .measure(&font, &[65, 65], true, &stop)
                .unwrap()
                .bounds
                .width,
            110
        );
        let run = system
            .layout(&font, &[65, 65], style(), 0, 0, &stop)
            .unwrap();
        assert_eq!(run.glyphs[1].x, 7);
        assert_eq!(run.glyphs[0].glyph.mask.as_slice(), [64, 64, 0, 0]);
        assert_eq!(run.glyphs[0].glyph.origin, [1, 78]);
        system.unmap(&font);
        assert_eq!(
            system.measure(&font, &[65], false, &stop).unwrap().width,
            60
        );
        assert_eq!(run.glyphs[0].glyph.mask.as_slice(), [64, 64, 0, 0]);
    }
    assert!(
        system
            .layout(&font, &[65], style(), 0, 0, &AtomicBool::new(true))
            .is_err()
    );
    let budget = Budget::new(100);
    let bytes = include_bytes!("fixtures/fonts/fixture-v1.tft");
    let p = prerendered::Font::parse(bytes.to_vec(), budget.reserve(bytes.len()).unwrap()).unwrap();
    assert!(prerendered::Font::parse(bytes[..40].to_vec(), budget.reserve(0).unwrap()).is_err());
    drop(p);
    assert_eq!(budget.used(), 0);
}

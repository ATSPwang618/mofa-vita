use super::*;
use ab_glyph::{
    Font as _, GlyphId, Outline, OutlineCurve, OutlinedGlyph, Point, PxScaleFactor, point,
};

pub(super) fn contains(face: &Face, code: u16) -> bool {
    face.font
        .glyph_id(char::from_u32(code as u32).unwrap_or('\u{fffd}'))
        .0
        != 0
}

pub(super) fn fallback(face: &Face, code: u16) -> Option<Arc<Face>> {
    if contains(face, code) {
        return None;
    }
    let fallback = bundled::face();
    contains(&fallback, code).then_some(fallback)
}

pub(super) fn id(face: &Face, code: u16) -> GlyphId {
    // Legacy Font consumes UTF-16 code units, including unpaired surrogates.
    let id = face
        .font
        .glyph_id(char::from_u32(code as u32).unwrap_or('\u{fffd}'));
    if id.0 != 0 {
        return id;
    }
    let fallback = face.font.glyph_id('\u{fffd}');
    if fallback.0 != 0 {
        fallback
    } else {
        face.font.glyph_id('?')
    }
}
pub(super) fn baseline(face: &Face, font: &Font) -> [i32; 2] {
    let scale = font.height.unsigned_abs() as f64 / face.font.units_per_em().unwrap_or(1.0) as f64;
    let ascent = (face.ascent as f64 * scale).round();
    let (s, c) = (font.angle as f64 * std::f64::consts::PI / 1800.0).sin_cos();
    [(s * ascent).round() as i32, (c * ascent).round() as i32]
}
pub(super) fn prepare(
    face: &Face,
    baseline_face: &Face,
    font: &Font,
    code: u16,
    rotate: bool,
) -> Result<Option<Prepared>> {
    if font.height == 0 {
        return Ok(None);
    }
    let id = id(face, code);
    let scale = font.height.unsigned_abs() as f32 / face.font.units_per_em().unwrap_or(1.0);
    let outline = face.font.outline(id);
    let mut decoration = Outline {
        bounds: Default::default(),
        curves: Vec::new(),
    };
    let advance = face.font.h_advance_unscaled(id);
    let thickness = (face.underline[1] * scale).floor().max(1.0) / scale;
    if font.underline {
        line(&mut decoration, advance, face.underline[0], thickness);
    }
    if font.strikeout {
        line(
            &mut decoration,
            advance,
            face.font.ascent_unscaled() * 0.3,
            thickness,
        );
    }
    let decoration = (!decoration.curves.is_empty()).then_some(decoration);
    if outline.is_none() && decoration.is_none() {
        return Ok(None);
    }
    let (s, c) = if rotate {
        match font.angle {
            0 => (0.0, 1.0),
            900 => (1.0, 0.0),
            1800 => (0.0, -1.0),
            2700 => (-1.0, 0.0),
            angle => (angle as f32 * std::f32::consts::PI / 1800.0).sin_cos(),
        }
    } else {
        (0.0, 1.0)
    };
    let shear = if font.italic && !face.italic {
        0.207
    } else {
        0.0
    };
    let baseline = if rotate {
        baseline(baseline_face, font)
    } else {
        let scale =
            font.height.unsigned_abs() as f32 / baseline_face.font.units_per_em().unwrap_or(1.0);
        [0, (baseline_face.ascent * scale).round() as i32]
    };
    let parts = [(outline, shear), (decoration, 0.0)].map(|(outline, shear)| {
        let outline = outline?;
        let transform = |p: Point| {
            let x = p.x + shear * p.y;
            point(c * x - s * p.y, s * x + c * p.y)
        };
        let outline = transformed(outline, transform);
        Some(OutlinedGlyph::new(
            ab_glyph::Glyph {
                id,
                scale: (font.height.unsigned_abs() as f32).into(),
                position: point(baseline[0] as f32, baseline[1] as f32),
            },
            outline,
            PxScaleFactor {
                horizontal: scale,
                vertical: scale,
            },
        ))
    });
    Ok(Some(Prepared { parts }))
}
// Rasterize decorations independently and union their coverage. Adding their
// contours to a CFF glyph can cancel strokes because its winding is opposite
// to TrueType's. No font-specific winding assumptions enter the bitmap.
pub(super) struct Prepared {
    parts: [Option<OutlinedGlyph>; 2],
}
impl Prepared {
    pub fn px_bounds(&self) -> ab_glyph::Rect {
        self.parts
            .iter()
            .flatten()
            .map(OutlinedGlyph::px_bounds)
            .reduce(|a, b| ab_glyph::Rect {
                min: point(a.min.x.min(b.min.x), a.min.y.min(b.min.y)),
                max: point(a.max.x.max(b.max.x), a.max.y.max(b.max.y)),
            })
            .unwrap_or_default()
    }
    pub fn draw(&self, mut draw: impl FnMut(u32, u32, f32)) {
        let bounds = self.px_bounds();
        for part in self.parts.iter().flatten() {
            let offset = part.px_bounds().min - bounds.min;
            part.draw(|x, y, a| draw(x + offset.x as u32, y + offset.y as u32, a));
        }
    }
}
fn transformed(mut outline: Outline, transform: impl Fn(Point) -> Point) -> Outline {
    for curve in &mut outline.curves {
        match curve {
            OutlineCurve::Line(a, b) => {
                *a = transform(*a);
                *b = transform(*b);
            }
            OutlineCurve::Quad(a, b, c) => {
                *a = transform(*a);
                *b = transform(*b);
                *c = transform(*c);
            }
            OutlineCurve::Cubic(a, b, c, d) => {
                *a = transform(*a);
                *b = transform(*b);
                *c = transform(*c);
                *d = transform(*d);
            }
        }
    }
    let b = outline.bounds;
    let corners = [
        b.min,
        b.max,
        point(b.min.x, b.max.y),
        point(b.max.x, b.min.y),
    ]
    .map(transform);
    outline.bounds = ab_glyph::Rect {
        min: point(
            corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min),
            corners
                .iter()
                .map(|p| p.y)
                .fold(f32::NEG_INFINITY, f32::max),
        ),
        max: point(
            corners
                .iter()
                .map(|p| p.x)
                .fold(f32::NEG_INFINITY, f32::max),
            corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min),
        ),
    };
    outline
}
fn line(outline: &mut Outline, width: f32, y: f32, thickness: f32) {
    let p = [
        point(0.0, y),
        point(width, y),
        point(width, y - thickness),
        point(0.0, y - thickness),
    ];
    if outline.curves.is_empty() {
        outline.bounds = ab_glyph::Rect {
            min: p[0],
            max: p[2],
        };
    } else {
        outline.bounds.min.x = outline.bounds.min.x.min(0.0);
        outline.bounds.min.y = outline.bounds.min.y.max(y);
        outline.bounds.max.x = outline.bounds.max.x.max(width);
        outline.bounds.max.y = outline.bounds.max.y.min(y - thickness);
    }
    for i in 0..4 {
        outline
            .curves
            .push(OutlineCurve::Line(p[i], p[(i + 1) % 4]));
    }
}

use super::{
    appearance, font, geometry, layer,
    matrix::Matrix,
    path::{Path, Segment},
    raster::Drawing,
    render,
};
use ab_glyph::{Font as _, OutlineCurve, Point};
use krkr_engine::extensions;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tjs_bind::Utf16;
use tjs_core::{NativeCx, NativeError, NativeResult, NativeStep, ObjId, Value};

#[derive(tjs_bind::Trace)]
enum Reply {
    Measure,
    Draw {
        owner: ObjId,
        appearance: Value,
        outline: bool,
    },
}
struct Text {
    path: Path,
    bounds: [f64; 4],
}
impl extensions::WorkContinuation<Text> for Reply {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, text: Text) -> NativeResult<NativeStep> {
        match *self {
            Self::Measure => Ok(NativeStep::Return(geometry::rectangle(cx, text.bounds)?)),
            Self::Draw {
                owner,
                appearance,
                outline: true,
            } => layer::paint(cx, owner, appearance, text.path),
            Self::Draw {
                owner,
                appearance,
                outline: false,
            } => {
                let mut appearance = appearance::snapshot(cx, appearance)?;
                appearance.retain(|draw| matches!(draw.drawing, Drawing::Fill(_)));
                let (matrix, update) = layer::state(cx, owner, |s| {
                    (
                        Matrix::new(Matrix::product(s.transform.elements, s.view.elements)),
                        s.update,
                    )
                })?;
                let result = geometry::rectangle(cx, [0.; 4])?;
                render::start(
                    cx,
                    Value::Obj(owner.into()),
                    render::Drawing {
                        path: text.path,
                        appearance,
                        matrix,
                        antialias: true,
                        update,
                    },
                    result,
                )
            }
        }
    }
}
fn build(
    face: Arc<extensions::FontFace>,
    size: f32,
    origin: [f32; 2],
    outline: bool,
    text: Vec<u16>,
    emit: bool,
    stop: &AtomicBool,
) -> NativeResult<Text> {
    let font = &face.font;
    let scale = size / font.units_per_em().unwrap_or(1.);
    let mut path = Path::default();
    let mut bounds: Option<[f32; 4]> = None;
    let mut x = origin[0];
    for ch in char::decode_utf16(text.into_iter().take_while(|&c| c != 0)) {
        if stop.load(Ordering::Relaxed) {
            return Err(NativeError::Message("text drawing cancelled"));
        }
        let id = font.glyph_id(ch.unwrap_or(char::REPLACEMENT_CHARACTER));
        if let Some(glyph) = font.outline(id) {
            let ext = [
                x + glyph.bounds.min.x * scale,
                -glyph.bounds.min.y * scale,
                x + glyph.bounds.max.x * scale,
                -glyph.bounds.max.y * scale,
            ];
            bounds = Some(bounds.map_or(ext, |b| {
                [
                    b[0].min(ext[0]),
                    b[1].min(ext[1]),
                    b[2].max(ext[2]),
                    b[3].max(ext[3]),
                ]
            }));
            if !emit {
                x += font.h_advance_unscaled(id) * scale;
                continue;
            }
            let y = origin[1] + if outline { ext[1] } else { 0. };
            let map = |p: Point| [(x + p.x * scale) as f64, (y - p.y * scale) as f64];
            let mut previous = None;
            for curve in glyph.curves {
                if path.segments.len() >= 1_000_000 {
                    return Err(NativeError::Message("text path is too large"));
                }
                let start = match curve {
                    OutlineCurve::Line(a, _)
                    | OutlineCurve::Quad(a, _, _)
                    | OutlineCurve::Cubic(a, _, _, _) => a,
                };
                if previous != Some(start) {
                    if previous.is_some() {
                        path.segments.push(Segment::Close);
                    }
                    path.segments.push(Segment::Move(map(start)));
                }
                previous = Some(match curve {
                    OutlineCurve::Line(_, b) => {
                        path.segments.push(Segment::Line(map(b)));
                        b
                    }
                    OutlineCurve::Quad(a, b, c) => {
                        let p = ab_glyph::point(
                            a.x + (b.x - a.x) * 2. / 3.,
                            a.y + (b.y - a.y) * 2. / 3.,
                        );
                        let q = ab_glyph::point(
                            c.x + (b.x - c.x) * 2. / 3.,
                            c.y + (b.y - c.y) * 2. / 3.,
                        );
                        path.segments.push(Segment::Cubic(map(p), map(q), map(c)));
                        c
                    }
                    OutlineCurve::Cubic(_, b, c, d) => {
                        path.segments.push(Segment::Cubic(map(b), map(c), map(d)));
                        d
                    }
                });
            }
            if previous.is_some() {
                path.segments.push(Segment::Close);
            }
        }
        x += font.h_advance_unscaled(id) * scale;
    }
    let bounds = bounds
        .map_or([0.; 4], |b| [b[0], b[1], b[2] - b[0], b[3] - b[1]])
        .map(f64::from);
    Ok(Text { path, bounds })
}
fn draw(
    cx: &mut NativeCx<'_>,
    font: Value,
    appearance: Value,
    x: f64,
    y: f64,
    text: Utf16,
    outline: bool,
) -> NativeResult<NativeStep> {
    let owner = cx.this();
    extensions::layer_prepare_draw(cx, Value::Obj(owner.into()))?;
    layer::state(cx, owner, |_| ())?;
    let font = font::snapshot(cx, font)?;
    appearance::snapshot(cx, appearance)?;
    let Some(face) = font.face() else {
        return Ok(NativeStep::Return(geometry::rectangle(cx, [0.; 4])?));
    };
    let size = font.size as f32;
    let y = if outline {
        let m = font.metrics();
        y as f32 + m[0] as f32 - m[1] as f32
    } else {
        y as f32
    };
    extensions::run_work(
        cx,
        move |stop| build(face, size, [x as f32, y], outline, text.0, true, stop),
        Box::new(Reply::Draw {
            owner,
            appearance,
            outline,
        }),
    )
}
#[tjs_bind::function(resumable = true)]
pub(super) fn draw_string(
    cx: &mut NativeCx<'_>,
    font: Value,
    app: Value,
    x: f64,
    y: f64,
    text: Utf16,
) -> NativeResult<NativeStep> {
    draw(cx, font, app, x, y, text, false)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn draw_path_string(
    cx: &mut NativeCx<'_>,
    font: Value,
    app: Value,
    x: f64,
    y: f64,
    text: Utf16,
) -> NativeResult<NativeStep> {
    draw(cx, font, app, x, y, text, true)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn measure(cx: &mut NativeCx<'_>, font: Value, text: Utf16) -> NativeResult<NativeStep> {
    layer::state(cx, cx.this(), |_| ())?;
    let font = font::snapshot(cx, font)?;
    let Some(face) = font.face() else {
        return Ok(NativeStep::Return(geometry::rectangle(cx, [0.; 4])?));
    };
    let size = font.size as f32;
    extensions::run_work(
        cx,
        move |stop| build(face, size, [0.; 2], false, text.0, false, stop),
        Box::new(Reply::Measure),
    )
}

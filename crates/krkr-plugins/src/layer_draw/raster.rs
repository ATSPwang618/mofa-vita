//! Portable owned drawing work. No VM handles or platform graphics objects.
use super::{
    matrix::Matrix,
    path::{Path, Segment},
};
use krkr_engine::protocol::{budget::Budget, graphics::Size, pixels::Bytes};
use std::sync::Arc;
use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Point, Shader,
    SpreadMode, Stroke, StrokeDash, Transform,
};

#[derive(Clone)]
pub enum Brush {
    Solid(u32),
    Texture {
        image: Arc<Texture>,
        matrix: Matrix,
        plain: bool,
    },
    Linear {
        start: [f32; 2],
        end: [f32; 2],
        colors: [u32; 2],
    },
    Radial {
        center: [f32; 2],
        radius: f32,
        stops: Vec<(f32, u32)>,
    },
}
pub struct Texture {
    pub bytes: Bytes,
    pub size: Size,
}
impl Texture {
    pub fn from_rgba(mut bytes: Bytes, size: Size) -> Option<Self> {
        if Some(bytes.as_slice().len()) != size.rgba_bytes() {
            return None;
        }
        premultiply(bytes.as_mut_slice());
        Some(Self { bytes, size })
    }
}
#[derive(Clone)]
pub struct Pen {
    pub brush: Brush,
    pub width: f32,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter: f32,
    pub dash_offset: f32,
    pub dashes: Vec<f32>,
    pub end_cap: Option<Path>,
    pub start_cap: Option<Path>,
}
impl Pen {
    pub fn new(brush: Brush) -> Self {
        Self {
            brush,
            width: 1.,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter: 10.,
            dash_offset: 0.,
            dashes: Vec::new(),
            end_cap: None,
            start_cap: None,
        }
    }
    fn style(&self) -> Stroke {
        Stroke {
            width: self.width,
            line_cap: self.cap,
            line_join: self.join,
            miter_limit: self.miter,
            dash: if self.dashes.is_empty() {
                None
            } else {
                StrokeDash::new(self.dashes.clone(), self.dash_offset)
            },
        }
    }
}
#[derive(Clone)]
pub enum Drawing {
    Fill(Brush),
    Stroke(Pen),
}
#[derive(Clone)]
pub struct Draw {
    pub offset: [f32; 2],
    pub drawing: Drawing,
}
fn color(argb: u32) -> Color {
    Color::from_rgba8(
        (argb >> 16) as u8,
        (argb >> 8) as u8,
        argb as u8,
        (argb >> 24) as u8,
    )
}
fn point(p: [f32; 2]) -> Point {
    Point::from_xy(p[0], p[1])
}
fn transform(m: [f32; 6]) -> Transform {
    Transform::from_row(m[0], m[1], m[2], m[3], m[4], m[5])
}
impl Brush {
    fn shader(&self) -> Option<Shader<'_>> {
        match self {
            Self::Solid(c) => Some(Shader::SolidColor(color(*c))),
            Self::Texture {
                image,
                matrix,
                plain,
            } => Some(tiny_skia::Pattern::new(
                tiny_skia::PixmapRef::from_bytes(
                    image.bytes.as_slice(),
                    image.size.width,
                    image.size.height,
                )?,
                if *plain {
                    SpreadMode::Pad
                } else {
                    SpreadMode::Repeat
                },
                tiny_skia::FilterQuality::Nearest,
                1.,
                transform(matrix.elements),
            )),
            Self::Linear { start, end, colors } => tiny_skia::LinearGradient::new(
                point(*start),
                point(*end),
                vec![
                    GradientStop::new(0., color(colors[0])),
                    GradientStop::new(1., color(colors[1])),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            ),
            Self::Radial {
                center,
                radius,
                stops,
            } => tiny_skia::RadialGradient::new(
                point(*center),
                point(*center),
                *radius,
                stops
                    .iter()
                    .map(|&(offset, c)| GradientStop::new(offset, color(c)))
                    .collect(),
                SpreadMode::Pad,
                Transform::identity(),
            ),
        }
    }
}
pub fn geometry(path: &Path) -> Option<tiny_skia::Path> {
    let mut result = PathBuilder::new();
    for segment in &path.segments {
        match *segment {
            Segment::Move([x, y]) => result.move_to(x as f32, y as f32),
            Segment::Line([x, y]) => result.line_to(x as f32, y as f32),
            Segment::Cubic(a, b, c) => result.cubic_to(
                a[0] as f32,
                a[1] as f32,
                b[0] as f32,
                b[1] as f32,
                c[0] as f32,
                c[1] as f32,
            ),
            Segment::Close => result.close(),
        }
    }
    result.finish()
}
/// Pixels are premultiplied RGBA during this operation. The caller owns their
/// budget and converts at the engine's straight-alpha pixel boundary.
#[allow(clippy::too_many_arguments)] // Raster target, geometry, style, clip and cancellation context.
pub fn draw(
    target: &mut PixmapMut<'_>,
    path: &Path,
    appearance: &[Draw],
    matrix: Matrix,
    origin: [i32; 2],
    antialias: bool,
    clip: Option<&tiny_skia::Mask>,
    cancelled: &dyn Fn() -> bool,
    budget: &Budget,
) -> Result<(), &'static str> {
    let Some(shape) = geometry(path) else {
        return Ok(());
    };
    for info in appearance {
        if cancelled() {
            return Err("vector drawing cancelled");
        }
        let [x, y] = info.offset;
        let mut m = Matrix::product([1., 0., 0., 1., x, y], matrix.elements);
        m[4] -= origin[0] as f32;
        m[5] -= origin[1] as f32;
        let brush = match &info.drawing {
            Drawing::Fill(b) => b,
            Drawing::Stroke(p) => &p.brush,
        };
        // Plain plutovg textures are transparent outside their image. Pattern's
        // Pad extends edge pixels, so restrict coverage to the transformed image.
        let plain = if let Brush::Texture {
            image,
            matrix,
            plain: true,
        } = brush
        {
            let count = (target.width() as usize)
                .checked_mul(target.height() as usize)
                .ok_or("texture mask dimensions overflow")?;
            let permit = budget
                .reserve(count)
                .map_err(|_| "texture mask exceeds image budget")?;
            let mut mask = tiny_skia::Mask::new(target.width(), target.height())
                .ok_or("texture mask allocation failed")?;
            let rect = tiny_skia::Rect::from_xywh(
                0.,
                0.,
                image.size.width as f32,
                image.size.height as f32,
            )
            .ok_or("invalid texture dimensions")?;
            let shape = PathBuilder::from_rect(rect);
            mask.fill_path(
                &shape,
                FillRule::Winding,
                false,
                transform(Matrix::product(matrix.elements, m)),
            );
            if let Some(clip) = clip {
                for (a, &b) in mask.data_mut().iter_mut().zip(clip.data()) {
                    *a = ((u16::from(*a) * u16::from(b) + 127) / 255) as u8;
                }
            }
            Some((mask, permit))
        } else {
            None
        };
        let clip = plain.as_ref().map(|(mask, _)| mask).or(clip);
        let Some(shader) = brush.shader() else {
            continue;
        };
        let paint = Paint {
            shader,
            anti_alias: antialias,
            ..Paint::default()
        };
        match &info.drawing {
            Drawing::Fill(_) => {
                target.fill_path(&shape, &paint, FillRule::Winding, transform(m), clip)
            }
            Drawing::Stroke(pen) => {
                let style = pen.style();
                target.stroke_path(&shape, &paint, &style, transform(m), clip);
                if let Some(cap) = pen.end_cap.as_ref().and_then(geometry) {
                    let mut current = [0.; 2];
                    let mut start = [0.; 2];
                    for segment in &path.segments {
                        match *segment {
                            Segment::Move(p) => {
                                current = p;
                                start = p;
                            }
                            Segment::Line(p) | Segment::Cubic(_, _, p) => current = p,
                            Segment::Close => current = start,
                        }
                    }
                    let end =
                        Matrix::product([1., 0., 0., 1., current[0] as f32, current[1] as f32], m);
                    // The reference strokes only the custom end cap, without
                    // tangent rotation; it retains but does not draw start caps.
                    target.stroke_path(&cap, &paint, &style, transform(end), clip);
                }
            }
        }
    }
    Ok(())
}
pub fn premultiply(bytes: &mut [u8]) {
    for p in bytes.as_chunks_mut::<4>().0.iter_mut() {
        let alpha = u32::from(p[3]);
        for c in &mut p[..3] {
            *c = ((u32::from(*c) * alpha + 127) / 255) as u8;
        }
    }
}
pub fn unpremultiply(bytes: &mut [u8]) {
    for p in bytes.as_chunks_mut::<4>().0.iter_mut() {
        let alpha = u32::from(p[3]);
        for c in &mut p[..3] {
            *c = (u32::from(*c) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0)
                .min(255) as u8;
        }
    }
}
pub fn hatch(
    style: i32,
    foreground: u32,
    background: u32,
    budget: &Budget,
) -> Result<Texture, &'static str> {
    let size = Size {
        width: 8,
        height: 8,
    };
    let mut bytes = Bytes::zeroed(256, budget).map_err(|_| "hatch exceeds image budget")?;
    let mut target =
        PixmapMut::from_bytes(bytes.as_mut_slice(), 8, 8).ok_or("invalid hatch dimensions")?;
    target.fill(color(background));
    let mut paint = Paint::default();
    paint.set_color(color(foreground));
    let rect = |target: &mut PixmapMut<'_>, x: f32, y: f32, w: f32, h: f32| {
        if let Some(rect) = tiny_skia::Rect::from_xywh(x, y, w, h) {
            target.fill_rect(rect, &paint, Transform::identity(), None);
        }
    };
    match style {
        0 => rect(&mut target, 0., 4., 8., 1.),
        1 => rect(&mut target, 4., 0., 1., 8.),
        4 => {
            rect(&mut target, 0., 4., 8., 1.);
            rect(&mut target, 4., 0., 1., 8.);
        }
        12 => {
            for y in (0..8).step_by(2) {
                for x in (0..8).step_by(2) {
                    rect(&mut target, x as f32, y as f32, 1., 1.);
                }
            }
        }
        48 => {
            for i in (0..8).step_by(2) {
                rect(&mut target, 0., i as f32, 8., 1.);
                rect(&mut target, i as f32, 0., 1., 8.);
            }
        }
        43 => {
            for y in (0..8).step_by(4) {
                for x in (0..8).step_by(4) {
                    rect(&mut target, x as f32, y as f32, 1., 1.);
                }
            }
        }
        _ => {
            let mut path = PathBuilder::new();
            for i in (-8..8).step_by(2) {
                if style != 3 {
                    path.move_to(i as f32, 0.);
                    path.line_to((i + 8) as f32, 8.);
                }
                if style == 3 || style == 5 {
                    path.move_to((i + 8) as f32, 0.);
                    path.line_to(i as f32, 8.);
                }
            }
            if let Some(path) = path.finish() {
                target.stroke_path(
                    &path,
                    &paint,
                    &Stroke::default(),
                    Transform::identity(),
                    None,
                );
            }
        }
    }
    Ok(Texture { bytes, size })
}

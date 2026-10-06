use crate::{Error, Rect, Result, Size};
use krkr_protocol::transform::Transform;

/// The inverse maps destination pixel centers to source pixel centers.
pub struct Mapping {
    pub inverse: [f32; 6],
    pub bounds: Rect,
}
impl Mapping {
    /// Conservative texel footprint of the visible destination, including
    /// interpolation neighbors only where they can actually be sampled.
    pub fn source_region(&self, source: Rect, sampling_bounds: Rect, linear: bool) -> Rect {
        let [a, b, tx, c, d, ty] = self.inverse.map(f64::from);
        let x0 = f64::from(self.bounds.left);
        let y0 = f64::from(self.bounds.top);
        let x1 = x0 + f64::from(self.bounds.width) - 1.0;
        let y1 = y0 + f64::from(self.bounds.height) - 1.0;
        let points = [[x0, y0], [x0, y1], [x1, y0], [x1, y1]]
            .map(|[x, y]| [a * x + b * y + tx, c * x + d * y + ty]);
        // Account for f32 evaluation/FMA differences at half-pixel edges.
        let error = [
            a.abs() * x0.abs().max(x1.abs()) + b.abs() * y0.abs().max(y1.abs()) + tx.abs(),
            c.abs() * x0.abs().max(x1.abs()) + d.abs() * y0.abs().max(y1.abs()) + ty.abs(),
        ]
        .map(|v| v * f64::from(f32::EPSILON) * 4.0);
        let axis = |index: usize, start: i32, length: u32, bound_start: i32, bound_length: u32| {
            let lo = (points
                .iter()
                .map(|p| p[index])
                .fold(f64::INFINITY, f64::min)
                - error[index])
                .max(f64::from(start) - 0.5);
            let hi = (points
                .iter()
                .map(|p| p[index])
                .fold(f64::NEG_INFINITY, f64::max)
                + error[index])
                .min(f64::from(start) + f64::from(length) - 0.5);
            let offset = if linear { 0.0 } else { 0.5 };
            let bound_end = f64::from(bound_start) + f64::from(bound_length);
            let left = (lo + offset)
                .floor()
                .clamp(f64::from(bound_start), bound_end - 1.0);
            let right = ((hi + offset).floor() + if linear { 2.0 } else { 1.0 })
                .clamp(left + 1.0, bound_end);
            (left as i32, (right - left) as u32)
        };
        let (left, width) = axis(
            0,
            source.left,
            source.width,
            sampling_bounds.left,
            sampling_bounds.width,
        );
        let (top, height) = axis(
            1,
            source.top,
            source.height,
            sampling_bounds.top,
            sampling_bounds.height,
        );
        Rect {
            left,
            top,
            width,
            height,
        }
    }
    pub fn new(source: Rect, transform: Transform, clip: Rect) -> Result<Option<Self>> {
        let points = match transform {
            Transform::Stretch(r) => [
                [f64::from(r.left) - 0.5, f64::from(r.top) - 0.5],
                [
                    f64::from(r.left) + f64::from(r.width) - 0.5,
                    f64::from(r.top) - 0.5,
                ],
                [
                    f64::from(r.left) - 0.5,
                    f64::from(r.top) + f64::from(r.height) - 0.5,
                ],
            ],
            Transform::Affine(points) => points,
        };
        if !points.iter().flatten().all(|x| x.is_finite()) {
            return Err(Error::Message("affine coordinates must be finite"));
        }
        let [origin, right, bottom] = points;
        let a = (right[0] - origin[0]) / f64::from(source.width);
        let b = (right[1] - origin[1]) / f64::from(source.width);
        let c = (bottom[0] - origin[0]) / f64::from(source.height);
        let d = (bottom[1] - origin[1]) / f64::from(source.height);
        let determinant = a * d - b * c;
        if determinant == 0.0 {
            return Ok(None);
        }
        let corners = [
            origin,
            right,
            bottom,
            [
                right[0] + bottom[0] - origin[0],
                right[1] + bottom[1] - origin[1],
            ],
        ];
        let left = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::INFINITY, f64::min)
            .ceil()
            .max(f64::from(clip.left));
        let top = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::INFINITY, f64::min)
            .ceil()
            .max(f64::from(clip.top));
        let right = corners
            .iter()
            .map(|p| p[0])
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            .min(f64::from(clip.left) + f64::from(clip.width));
        let bottom = corners
            .iter()
            .map(|p| p[1])
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            .min(f64::from(clip.top) + f64::from(clip.height));
        if right <= left || bottom <= top {
            return Ok(None);
        }
        let xx = d / determinant;
        let xy = -c / determinant;
        let yx = -b / determinant;
        let yy = a / determinant;
        let inverse = [
            xx,
            xy,
            f64::from(source.left) - 0.5 - xx * origin[0] - xy * origin[1],
            yx,
            yy,
            f64::from(source.top) - 0.5 - yx * origin[0] - yy * origin[1],
        ]
        .map(|v| v as f32);
        if !inverse.iter().all(|v| v.is_finite()) {
            return Err(Error::Message("affine mapping exceeds renderer precision"));
        }
        Ok(Some(Self {
            inverse,
            bounds: Rect {
                left: left as i32,
                top: top as i32,
                width: (right - left) as u32,
                height: (bottom - top) as u32,
            },
        }))
    }
}
pub fn validate_source(rect: Rect, size: Size) -> Result<()> {
    if rect.left < 0
        || rect.top < 0
        || i64::from(rect.left) + i64::from(rect.width) > i64::from(size.width)
        || i64::from(rect.top) + i64::from(rect.height) > i64::from(size.height)
    {
        return Err(Error::Message(
            "transformation source rectangle is outside the image",
        ));
    }
    Ok(())
}

//! Homography preparation shared by graphics backends; no pixel copies or VM.
use crate::{Error, Rect, Result, Size};
use krkr_protocol::transform::Perspective;

pub struct Mapping {
    /// Row-major destination-edge to source-edge homography.
    pub inverse: [[f32; 4]; 3],
    pub points: [[f32; 4]; 4],
}
impl Mapping {
    pub fn new(mapping: Perspective) -> Result<Self> {
        let q = mapping.destination;
        if !mapping
            .source
            .iter()
            .chain(q.iter().flatten())
            .all(|v| v.is_finite())
        {
            return Err(Error::Message("perspective coordinates must be finite"));
        }
        // Unit square -> destination, with vertices ordered LT, RT, LB, RB.
        // Translate/scale before solving so a large image offset doesn't erode
        // the precision of the four-point shape.
        let origin = q[0];
        let scale = q
            .iter()
            .flat_map(|p| [p[0] - origin[0], p[1] - origin[1]])
            .map(f64::abs)
            .fold(0.0, f64::max);
        if scale == 0.0 || !scale.is_finite() {
            return Err(Error::Message("singular perspective destination"));
        }
        let [a, b, d, c] = q.map(|p| [(p[0] - origin[0]) / scale, (p[1] - origin[1]) / scale]);
        let dx1 = b[0] - c[0];
        let dx2 = d[0] - c[0];
        let dy1 = b[1] - c[1];
        let dy2 = d[1] - c[1];
        let dx3 = a[0] - b[0] + c[0] - d[0];
        let dy3 = a[1] - b[1] + c[1] - d[1];
        let (g, h) = if dx3 == 0.0 && dy3 == 0.0 {
            (0.0, 0.0)
        } else {
            let determinant = dx1 * dy2 - dx2 * dy1;
            if determinant == 0.0 {
                return Err(Error::Message("singular perspective destination"));
            }
            (
                (dx3 * dy2 - dx2 * dy3) / determinant,
                (dx1 * dy3 - dx3 * dy1) / determinant,
            )
        };
        let [a, b, c, d, e, f, g, h, i] = [
            b[0] + g * b[0] - a[0],
            d[0] + h * d[0] - a[0],
            a[0],
            b[1] + g * b[1] - a[1],
            d[1] + h * d[1] - a[1],
            a[1],
            g,
            h,
            1.0,
        ];
        // Adjugate is sufficient: the common determinant cancels on division.
        let mut inverse = [
            [e * i - f * h, c * h - b * i, b * f - c * e],
            [f * g - d * i, a * i - c * g, c * d - a * f],
            [d * h - e * g, b * g - a * h, a * e - b * d],
        ];
        let determinant = a * inverse[0][0] + b * inverse[1][0] + c * inverse[2][0];
        if determinant == 0.0 || !determinant.is_finite() {
            return Err(Error::Message("singular perspective destination"));
        }
        for row in &mut inverse {
            row[0] /= scale;
            row[1] /= scale;
            row[2] -= row[0] * origin[0] + row[1] * origin[1];
        }
        let [left, top, right, bottom] = mapping.source;
        let denominator = inverse[2];
        for column in 0..3 {
            inverse[0][column] = (right - left) * inverse[0][column] + left * denominator[column];
            inverse[1][column] = (bottom - top) * inverse[1][column] + top * denominator[column];
        }
        let norm = inverse
            .iter()
            .flatten()
            .map(|v| v.abs())
            .fold(0.0, f64::max);
        let inverse = inverse.map(|r| {
            [
                (r[0] / norm) as f32,
                (r[1] / norm) as f32,
                (r[2] / norm) as f32,
                0.0,
            ]
        });
        let points = q.map(|p| [p[0] as f32, p[1] as f32, 0.0, 0.0]);
        if !inverse
            .iter()
            .chain(points.iter())
            .flatten()
            .all(|v| v.is_finite())
            || inverse[2][..3].iter().all(|v| *v == 0.0)
        {
            return Err(Error::Message(
                "perspective mapping exceeds renderer precision",
            ));
        }
        Ok(Self { inverse, points })
    }

    /// Conservative visible source footprint, with bilinear neighbors. If a
    /// projective horizon crosses the enclosing clip, retain the whole source.
    pub fn source_region(&self, clip: Rect, size: Size) -> Rect {
        if size.width == 0 || size.height == 0 || clip.width == 0 || clip.height == 0 {
            return size.rect();
        }
        let x0 = f64::from(clip.left) + 0.5;
        let y0 = f64::from(clip.top) + 0.5;
        let x1 = x0 + f64::from(clip.width) - 1.0;
        let y1 = y0 + f64::from(clip.height) - 1.0;
        let mut ranges = [[f64::INFINITY, f64::NEG_INFINITY]; 2];
        let r = self.inverse.map(|r| r.map(f64::from));
        let error = r.map(|r| {
            (r[0].abs() * x0.abs().max(x1.abs()) + r[1].abs() * y0.abs().max(y1.abs()) + r[2].abs())
                * f64::from(f32::EPSILON)
                * 16.0
        });
        let mut sign = 0.0f64;
        for [x, y] in [[x0, y0], [x1, y0], [x0, y1], [x1, y1]] {
            let dot = |r: [f64; 4]| r[0] * x + r[1] * y + r[2];
            let w = dot(r[2]);
            // Keep away from rounding-sensitive horizons, not just exact zero.
            if w.abs() <= error[2] || sign * w < 0.0 {
                return size.rect();
            }
            sign = w;
            for i in 0..2 {
                let p = dot(r[i]) / w;
                let uncertainty = (error[i] + p.abs() * error[2]) / (w.abs() - error[2]);
                ranges[i][0] = ranges[i][0].min(p - uncertainty);
                ranges[i][1] = ranges[i][1].max(p + uncertainty);
            }
        }
        // Enclose projection rounding before adding the bilinear neighbors.
        if ranges.iter().flatten().any(|v| !v.is_finite()) {
            return size.rect();
        }
        let axis = |i: usize, length: u32| {
            let [lo, hi] = ranges[i];
            let lo = (lo.floor() - 2.0).clamp(0.0, f64::from(length) - 1.0) as i32;
            let hi = (hi.ceil() + 2.0).clamp(f64::from(lo) + 1.0, f64::from(length)) as i32;
            (lo, (hi - lo) as u32)
        };
        let (left, width) = axis(0, size.width);
        let (top, height) = axis(1, size.height);
        Rect {
            left,
            top,
            width,
            height,
        }
    }
}

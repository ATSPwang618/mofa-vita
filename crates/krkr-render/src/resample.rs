//! Separable filter coefficients. Pixel data remains in the rendering backend.
use crate::{
    Result,
    budget::{Budget, Permit},
};
use krkr_protocol::transform::{Filter, Sampling};
use std::f32::consts::PI;
mod cache;
pub use cache::Cache;

fn radius(filter: Filter) -> f32 {
    match filter {
        Filter::Linear | Filter::Nearest | Filter::FastLinear | Filter::Area => 1.0,
        Filter::Cubic | Filter::Lanczos2 | Filter::Spline16 | Filter::Gaussian => 2.0,
        Filter::Lanczos3 | Filter::Spline36 => 3.0,
        Filter::Blackman => 4.0,
    }
}
fn weight(sampling: Sampling, x: f32) -> f32 {
    let x = x.abs();
    match sampling.filter {
        Filter::Linear | Filter::Nearest | Filter::FastLinear => (1.0 - x).max(0.0),
        Filter::Cubic => {
            let c = sampling.sharpness;
            if x <= 1.0 {
                1.0 - (c + 3.0) * x * x + (c + 2.0) * x * x * x
            } else if x <= 2.0 {
                -4.0 * c + 8.0 * c * x - 5.0 * c * x * x + c * x * x * x
            } else {
                0.0
            }
        }
        Filter::Lanczos2 | Filter::Lanczos3 => {
            let tap = radius(sampling.filter);
            if x < f32::EPSILON {
                1.0
            } else if x >= tap {
                0.0
            } else {
                (PI * x).sin() * (PI * x / tap).sin() / (PI * PI * x * x / tap)
            }
        }
        Filter::Spline16 => {
            if x <= 1.0 {
                x * x * x - x * x * 9.0 / 5.0 - x / 5.0 + 1.0
            } else if x <= 2.0 {
                -x * x * x / 3.0 + x * x * 9.0 / 5.0 - x * 46.0 / 15.0 + 8.0 / 5.0
            } else {
                0.0
            }
        }
        Filter::Spline36 => {
            if x <= 1.0 {
                x * x * x * 13.0 / 11.0 - x * x * 453.0 / 209.0 - x * 3.0 / 209.0 + 1.0
            } else if x <= 2.0 {
                -x * x * x * 6.0 / 11.0 + x * x * 612.0 / 209.0 - x * 1038.0 / 209.0 + 540.0 / 209.0
            } else if x <= 3.0 {
                x * x * x / 11.0 - x * x * 159.0 / 209.0 + x * 434.0 / 209.0 - 384.0 / 209.0
            } else {
                0.0
            }
        }
        Filter::Gaussian => (-2.0 * x * x).exp() * (2.0 / PI).sqrt(),
        Filter::Blackman => {
            if x < f32::EPSILON {
                1.0
            } else {
                (0.42 + 0.5 * (PI * x / 4.0).cos() + 0.08 * (2.0 * PI * x / 4.0).cos())
                    * (PI * x).sin()
                    / (PI * x)
            }
        }
        Filter::Area => unreachable!("area weights use pixel intersection"),
    }
}
pub struct Axis {
    /// Header per output pixel: source start, weight offset, count, reserved.
    /// Followed by scalar weights, padded to a vec4 for storage-buffer access.
    pub data: Vec<f32>,
    pub permit: Permit,
    pub range: std::ops::Range<i32>,
}
impl Axis {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        source_start: i32,
        source_len: u32,
        dest_start: i32,
        dest_len: i32,
        visible_start: i32,
        visible_len: u32,
        sampling: Sampling,
        budget: &Budget,
    ) -> Result<Self> {
        let scale = source_len as f32 / dest_len.unsigned_abs() as f32;
        let support = radius(sampling.filter) * scale.max(1.0);
        let max_taps = ((support * 2.0).ceil() as usize + 2).min(source_len as usize);
        let capacity = visible_len as usize * (max_taps + 4) + 3;
        // Charge the CPU table for its own lifetime. Backends reserve uploaded
        // storage separately: cached coefficients may serve several submissions.
        let permit = budget.reserve(capacity * size_of::<f32>())?;
        let mut data = Vec::with_capacity(capacity);
        data.resize(visible_len as usize * 4, 0.0);
        let source_end = source_start + source_len as i32;
        let mut range = source_end..source_start;
        for row in 0..visible_len {
            let position =
                (visible_start as f64 + row as f64 + 0.5 - dest_start as f64) / dest_len as f64;
            let center = source_start as f32 + (position * source_len as f64) as f32;
            let (left, right) = if sampling.filter == Filter::Area {
                (
                    (center - scale * 0.5).floor() as i32,
                    (center + scale * 0.5).ceil() as i32,
                )
            } else {
                (
                    (center - support).floor() as i32,
                    (center + support).floor() as i32,
                )
            };
            let start = left.clamp(source_start, source_end - 1);
            let end = right.clamp(start + 1, source_end);
            range.start = range.start.min(start);
            range.end = range.end.max(end);
            let offset = data.len();
            data.resize(offset + (end - start) as usize, 0.0);
            for s in left..right {
                let coefficient = if sampling.filter == Filter::Area {
                    ((s + 1) as f32).min(center + scale * 0.5)
                        - (s as f32).max(center - scale * 0.5)
                } else {
                    weight(sampling, (s as f32 + 0.5 - center) / scale.max(1.0))
                };
                data[offset + (s.clamp(start, end - 1) - start) as usize] += coefficient;
            }
            let sum: f32 = data[offset..].iter().sum();
            let normalization = if sum < f32::EPSILON { 0.0 } else { 1.0 / sum };
            for w in &mut data[offset..] {
                *w *= normalization;
            }
            data[row as usize * 4..row as usize * 4 + 4].copy_from_slice(&[
                start as f32,
                offset as f32,
                (end - start) as f32,
                0.0,
            ]);
        }
        data.resize(data.len().next_multiple_of(4), 0.0);
        Ok(Self {
            data,
            permit,
            range,
        })
    }
}

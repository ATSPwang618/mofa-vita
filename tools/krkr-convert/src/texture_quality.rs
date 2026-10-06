//! Validate the actual encoder output, not just its container and dimensions.
//! Verify opacity and compositing as well as visible RGB; low-alpha details
//! must not disappear merely because their straight color was ignored.
use krkr_assets::{ReadPlan, ReadSource, Stream};
use krkr_protocol::{budget::Budget, graphics::Size};
use std::{
    io::Cursor,
    sync::{Arc, atomic::AtomicBool},
};

struct Memory(Vec<u8>);
impl ReadSource for Memory {
    fn open(&self) -> krkr_assets::Result<Box<dyn Stream>> {
        Ok(Box::new(Cursor::new(self.0.clone())))
    }
}

pub(crate) fn check(
    size: Size,
    source: &[u8],
    encoded: &[u8],
    color: bool,
) -> Result<Option<String>, String> {
    let plan = ReadPlan::custom(
        Vec::new(),
        encoded.len() as u64,
        16 << 20,
        Arc::new(Memory(encoded.to_vec())),
    );
    let cancelled = AtomicBool::new(false);
    let decoded = krkr_image::Request::from_plans(
        plan,
        None,
        None,
        0x02ffffff,
        None,
        false,
        Budget::new(32 << 20),
    )
    .probe(&cancelled)
    .and_then(|p| p.decode_compact(&cancelled))
    .map_err(|e| e.to_string())?;
    if decoded.pixels.size != size || size.rgba_bytes() != Some(source.len()) {
        return Err("texture quality check size mismatch".into());
    }
    Ok(assess(
        source,
        decoded.pixels.main.as_ref().unwrap().as_slice(),
        color,
    ))
}

fn assess(source: &[u8], decoded: &[u8], color: bool) -> Option<String> {
    let mut opaque = 0usize;
    let mut damaged = 0usize;
    let mut error = [0f64; 4];
    let mut visible = 0usize;
    let mut composite_error = 0.0;
    let mut composite_pixels = 0usize;
    for (a, b) in source
        .as_chunks::<4>()
        .0
        .iter()
        .zip(decoded.as_chunks::<4>().0.iter())
    {
        if a[3] == 255 {
            opaque += 1;
            damaged += usize::from(b[3] < 253);
        }
        error[3] += f64::from(a[3].abs_diff(b[3])).powi(2);
        if a[3] != 0 || b[3] != 0 {
            composite_pixels += 1;
            for c in 0..3 {
                let black =
                    (f64::from(a[c]) * f64::from(a[3]) - f64::from(b[c]) * f64::from(b[3])) / 255.0;
                let white = black + f64::from(b[3]) - f64::from(a[3]);
                // Black and white bound the error over any background color.
                // Include low-alpha RGB without penalizing invisible colors.
                composite_error += black.abs().max(white.abs()).powi(2);
            }
        }
        if a[3] >= 128 {
            visible += 1;
            for c in 0..3 {
                error[c] += f64::from(a[c].abs_diff(b[c])).powi(2);
            }
        }
    }
    let alpha_rmse = (error[3] / (source.len() / 4).max(1) as f64).sqrt();
    let rgb_rmse = ((error[0] + error[1] + error[2]) / (3 * visible).max(1) as f64).sqrt();
    // Report the other measured losses too. A small damaged-opaque count can
    // coexist with severe alpha and color errors elsewhere in the same image.
    // Permit a very small interpolation fringe, never wholesale translucency.
    if damaged > 8 && damaged.saturating_mul(1000) > opaque.max(1) {
        return Some(format!(
            "compression changes opaque pixels to translucent ({damaged}/{opaque}; alpha RMSE {alpha_rmse:.2}; RGB RMSE {rgb_rmse:.2})"
        ));
    }
    if alpha_rmse > 4.0 {
        return Some(format!(
            "compression alpha error too large (RMSE {alpha_rmse:.2})"
        ));
    }
    if color && rgb_rmse > 5.0 {
        return Some(format!(
            "compression color error too large (RMSE {rgb_rmse:.2})"
        ));
    }
    let composite_rmse = (composite_error / (3 * composite_pixels).max(1) as f64).sqrt();
    if color && composite_rmse > 5.0 {
        return Some(format!(
            "compression color error too large (composite RMSE {composite_rmse:.2})"
        ));
    }
    None
}

#[cfg(test)]
#[path = "../tests/internal/texture_quality.rs"]
mod tests;

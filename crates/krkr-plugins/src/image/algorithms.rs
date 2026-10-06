//! Altered CxImage algorithms; table generation retains the reference casts.
use super::*;
use krkr_engine::protocol::{budget::Budget, pixels::Bytes};
use std::sync::Arc;

pub(super) fn make(
    cx: &mut NativeCx<'_>,
    operation: Operation,
    args: &[Value],
    rect: Rect,
    random: &Cell<u32>,
    budget: &Budget,
) -> NativeResult<Filter> {
    let integer = |i: usize| value::to_integer(cx.heap(), args[i]).map(|v| v as i32);
    let mut data = [0u32; 1024];
    let mut length = 0;
    let kind = match operation {
        Operation::Light => {
            let brightness = integer(0)?.wrapping_add(128);
            let contrast = integer(1)?.wrapping_add(100) as f32 / 100.0;
            for (i, out) in data[..256].iter_mut().enumerate() {
                *out = (((i as i32 - 128) as f32 * contrast + brightness as f32) as i32)
                    .clamp(0, 255) as u32;
            }
            length = 256;
            Kind::Lookup
        }
        Operation::Colorize => {
            let hue = integer(0)? as u8;
            let saturation = integer(1)? as u8;
            let blend = value::to_real(cx.heap(), args[2])?;
            if !blend.is_finite() {
                return Err(NativeError::Message("colorize blend must be finite"));
            }
            let blend = blend.clamp(0.0, 1.0);
            for (l, out) in data[..256].iter_mut().enumerate() {
                let [r, g, b] = color(hue, saturation, l as u8);
                *out = u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
            }
            length = 256;
            Kind::Colorize {
                amount: if blend > f64::from(0.999f32) {
                    256
                } else {
                    (256.0 * blend) as u16
                },
            }
        }
        Operation::Modulate => Kind::Modulate {
            hue: integer(0)? as f32 / 360.0,
            saturation: integer(1)? as f32 / 100.0,
            luminance: integer(2)? as f32 / 100.0,
        },
        Operation::Noise | Operation::White => {
            let level = if matches!(operation, Operation::Noise) {
                Some(integer(0)?)
            } else {
                None
            };
            let seed = random.get();
            // Same ordered MSVCRT rand stream for both APIs, independent of GPU
            // scheduling. No host CRT dependency or changes to TJS Math.random.
            random.set(advance(
                seed,
                u64::from(rect.width)
                    * u64::from(rect.height)
                    * if level.is_some() { 3 } else { 1 },
            ));
            Kind::Noise { seed, level }
        }
        Operation::Blur => {
            let radius = value::to_real(cx.heap(), args[0])? as f32;
            if !radius.is_finite() || radius.abs() > 511.0 {
                return Err(NativeError::Message(
                    "gaussianBlur radius must be finite with magnitude at most 511",
                ));
            }
            length = gaussian(radius, &mut data);
            Kind::Gaussian
        }
    };
    let mut table =
        Bytes::zeroed(length * 4, budget).map_err(|e| NativeError::Detail(e.to_string()))?;
    for (bytes, word) in table
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(data)
    {
        bytes.copy_from_slice(&word.to_le_bytes());
    }
    Ok(Filter {
        kind,
        table: Arc::new(table),
    })
}
fn hue(n1: f32, n2: f32, mut hue: f32) -> f32 {
    if hue > 360.0 {
        hue -= 360.0;
    } else if hue < 0.0 {
        hue += 360.0;
    }
    if hue < 60.0 {
        n1 + (n2 - n1) * hue / 60.0
    } else if hue < 180.0 {
        n2
    } else if hue < 240.0 {
        n1 + (n2 - n1) * (240.0 - hue) / 60.0
    } else {
        n1
    }
}
fn color(h: u8, s: u8, l: u8) -> [u8; 3] {
    let h = f32::from(h) * 360.0 / 255.0;
    let s = f32::from(s) / 255.0;
    let l = f32::from(l) / 255.0;
    if s == 0.0 {
        return [(l * 255.0) as u8; 3];
    }
    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let m1 = 2.0 * l - m2;
    [h + 120.0, h, h - 120.0].map(|h| (hue(m1, m2, h) * 255.0) as u8)
}
fn advance(mut seed: u32, mut count: u64) -> u32 {
    let (mut a, mut c) = (214013u32, 2531011u32);
    while count != 0 {
        if count & 1 != 0 {
            seed = seed.wrapping_mul(a).wrapping_add(c);
        }
        c = c.wrapping_mul(a.wrapping_add(1));
        a = a.wrapping_mul(a);
        count >>= 1;
    }
    seed
}
fn gaussian(radius: f32, output: &mut [u32; 1024]) -> usize {
    let sd = (0.5 * f64::from(radius)).abs() as f32 + 0.25;
    let radius = sd * 2.0;
    let length = (2.0 * f64::from(radius - 0.5).ceil() + 1.0) as usize;
    let mut matrix = [0f32; 1024];
    for (i, item) in matrix
        .iter_mut()
        .enumerate()
        .take(length)
        .skip(length / 2 + 1)
    {
        let base = i as f32 - (length / 2) as f32 - 0.5;
        let mut sum = 0f32;
        for j in 1..=50 {
            let x = f64::from(base) + 0.02 * f64::from(j);
            if x <= f64::from(radius) {
                sum += (-x * x / f64::from(2.0 * sd * sd)).exp() as f32;
            }
        }
        *item = sum / 50.0;
    }
    for i in 0..length / 2 {
        matrix[i] = matrix[length - 1 - i];
    }
    let mut sum = 0f32;
    for j in 0..=50 {
        let x = 0.5 + 0.02 * f64::from(j);
        sum += (-x * x / f64::from(2.0 * sd * sd)).exp() as f32;
    }
    matrix[length / 2] = sum / 51.0;
    let total: f32 = matrix[..length].iter().sum();
    for (out, &weight) in output.iter_mut().zip(&matrix[..length]) {
        *out = (weight / total).to_bits();
    }
    length
}

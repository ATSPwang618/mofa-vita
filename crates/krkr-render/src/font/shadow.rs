use super::*;
#[cfg(test)]
#[path = "../../tests/internal/shadow.rs"]
mod tests;

pub(super) fn bold(glyph: Glyph, height: i32, system: &mut System) -> Result<Glyph> {
    if glyph.size.width == 0 || glyph.size.height == 0 {
        return Ok(glyph);
    }
    let level = (height.unsigned_abs() / 50 + 1).min(8);
    let size = Size {
        width: glyph.size.width + level,
        height: glyph.size.height,
    };
    let len = size.width as usize * size.height as usize;
    let permit = system.reserve(len)?;
    let mut mask = Bytes::with_permit(vec![0; len], permit);
    for (y, row) in glyph
        .mask
        .as_slice()
        .chunks_exact(glyph.size.width as usize)
        .enumerate()
    {
        let output =
            &mut mask.as_mut_slice()[y * size.width as usize..(y + 1) * size.width as usize];
        for (x, &v) in row.iter().enumerate() {
            if v == 0 {
                continue;
            }
            for shift in 0..=level as usize {
                let out = &mut output[x + shift];
                *out = (*out).max(v);
            }
        }
    }
    Ok(Glyph {
        size,
        origin: [glyph.origin[0] - (level / 2) as i32, glyph.origin[1]],
        mask,
        ..glyph
    })
}
pub(super) fn blur(
    glyph: &Glyph,
    level: i32,
    width: i32,
    system: &mut System,
    stop: &AtomicBool,
) -> Result<Glyph> {
    let radius = width.unsigned_abs();
    let side = radius
        .checked_mul(2)
        .ok_or(Error::Message("shadow size overflow"))?;
    let size = Size {
        width: glyph
            .size
            .width
            .checked_add(side)
            .ok_or(Error::Message("shadow size overflow"))?,
        height: glyph
            .size
            .height
            .checked_add(side)
            .ok_or(Error::Message("shadow size overflow"))?,
    };
    let len = (size.width as usize)
        .checked_mul(size.height as usize)
        .ok_or(Error::Message("shadow size overflow"))?;
    let permit = system.reserve(len)?;
    let mut mask = Bytes::with_permit(vec![0; len], permit);
    let maximum = if glyph.levels == 65 { 64 } else { 255 };
    if radius == 0 && level > 0 {
        for (out, &v) in mask.as_mut_slice().iter_mut().zip(glyph.mask.as_slice()) {
            *out = ((i64::from(v) * i64::from(level)) >> 8).clamp(0, maximum) as u8;
        }
    } else if level > 0 {
        // Native radial tent uses its integer distance approximation, and
        // truncates each weighted contribution before saturated addition.
        let r = radius as i32;
        let distance = |x: i32, y: i32| {
            let a = x.unsigned_abs().max(y.unsigned_abs());
            let b = x.unsigned_abs().min(y.unsigned_abs());
            let t = b + (b >> 1);
            a - (a >> 5) - (a >> 7) + (t >> 2) + (t >> 6)
        };
        let mut sum = 0u64;
        for y in -r..=r {
            cancelled(stop)?;
            for x in -r..=r {
                let d = distance(x, y);
                if d <= radius {
                    sum += u64::from(radius - d + 1);
                }
            }
        }
        let norm = (1u64 << 18) / sum.max(1);
        let coverage = glyph.mask.as_slice().iter().copied().max().unwrap_or(0) as usize;
        for y in -r..=r {
            cancelled(stop)?;
            for x in -r..=r {
                let d = distance(x, y);
                if d > radius {
                    continue;
                }
                let weight = (i64::from(radius - d + 1) * norm as i64 * i64::from(level)) >> 8;
                // Coverage has only 256 possible values. Preserve the native
                // per-contribution truncation while taking its wide multiply
                // out of the glyph pixel loop. Nonpositive kernels add zero.
                if weight <= 0 {
                    continue;
                }
                let contributions: [u8; 256] =
                    std::array::from_fn(|value| ((value as i64 * weight) >> 18).min(maximum) as u8);
                if contributions[coverage] == 0 {
                    continue;
                }
                for sy in 0..glyph.size.height as usize {
                    let dst = (y + r) as usize * size.width as usize
                        + sy * size.width as usize
                        + (x + r) as usize;
                    let src = sy * glyph.size.width as usize;
                    let input = &glyph.mask.as_slice()[src..][..glyph.size.width as usize];
                    let output = &mut mask.as_mut_slice()[dst..][..glyph.size.width as usize];
                    for (out, &value) in output.iter_mut().zip(input) {
                        *out = out
                            .saturating_add(contributions[value as usize])
                            .min(maximum as u8);
                    }
                }
            }
        }
    }
    Ok(Glyph {
        id: glyph_id(),
        size,
        origin: [
            glyph.origin[0].saturating_sub(width),
            glyph.origin[1].saturating_sub(width),
        ],
        advance: glyph.advance,
        levels: glyph.levels,
        mask,
    })
}

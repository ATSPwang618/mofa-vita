use super::{Number, digit, skip_space};

// TJSParseNonDecimalReal composes IEEE bits directly: truncate the significand
// instead of rounding, flush subnormals to zero, and overflow to Infinity.
pub(super) fn real(
    units: &[u16],
    start: usize,
    mut cursor: usize,
    radix: u32,
    negative: bool,
) -> (Number, usize) {
    if units.get(cursor) == Some(&46) {
        cursor += 1;
        while units
            .get(cursor)
            .and_then(|&u| digit(u))
            .is_some_and(|d| d < radix)
        {
            cursor += 1;
        }
    }
    let mantissa_end = cursor;
    let mut bias = 0_i32;
    if matches!(units.get(cursor), Some(80 | 112)) {
        cursor += 1;
        skip_space(units, &mut cursor);
        let negative_bias = units.get(cursor) == Some(&45);
        if matches!(units.get(cursor), Some(43 | 45)) {
            cursor += 1;
            skip_space(units, &mut cursor);
        }
        while let Some(&unit @ 48..=57) = units.get(cursor) {
            bias = bias.wrapping_mul(10).wrapping_add(i32::from(unit - 48));
            cursor += 1;
        }
        if negative_bias {
            bias = bias.wrapping_neg();
        }
    }
    let width = radix.ilog2();
    let mut exponent = 0_i32;
    let mut significant = 0_u32;
    let mut main = 0_u64;
    let mut fraction = false;
    for &unit in &units[start..mantissa_end] {
        if unit == 46 {
            fraction = true;
            continue;
        }
        let n = digit(unit).expect("scanned radix digit");
        if significant == 0 {
            let bits = u32::BITS - n.leading_zeros();
            if bits == 0 {
                if fraction {
                    exponent = exponent.wrapping_sub(width as i32);
                }
            } else {
                significant = bits;
                main = u64::from(n) << (64 - bits);
                if fraction {
                    exponent = exponent.wrapping_sub((width - bits + 1) as i32);
                } else {
                    exponent = (bits - 1) as i32;
                }
            }
        } else {
            if significant + width < 64 {
                significant += width;
                main |= u64::from(n) << (64 - significant);
            }
            if !fraction {
                exponent = exponent.wrapping_add(width as i32);
            }
        }
    }
    exponent = exponent.wrapping_add(bias);
    let bits = if main == 0 || exponent < -1022 {
        0
    } else if exponent > 1023 {
        f64::INFINITY.to_bits()
    } else {
        ((exponent + 1023) as u64) << 52 | ((main >> 11) & ((1_u64 << 52) - 1))
    };
    (
        Number::Real(f64::from_bits(bits | (u64::from(negative) << 63))),
        cursor,
    )
}

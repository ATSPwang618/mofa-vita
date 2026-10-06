//! Shared numeric reader for source literals and TJS string conversions.
mod radix;

#[derive(Clone, Copy, Debug)]
pub enum Number {
    Int(i64),
    Real(f64),
}

fn digit(unit: u16) -> Option<u32> {
    match unit {
        48..=57 => Some(u32::from(unit - 48)),
        65..=70 => Some(u32::from(unit - 65 + 10)),
        97..=102 => Some(u32::from(unit - 97 + 10)),
        _ => None,
    }
}

fn skip_space(units: &[u16], cursor: &mut usize) {
    while units.get(*cursor).is_some_and(|u| matches!(u, 9..=13 | 32)) {
        *cursor += 1;
    }
}

/// Reads a numeric prefix. As in TJSParseNumber, whitespace is permitted after
/// a sign and in the exponent, but is not skipped before the initial token.
pub fn parse(units: &[u16]) -> Option<(Number, usize)> {
    let mut cursor = 0;
    let negative = units.first() == Some(&45);
    if matches!(units.first(), Some(43 | 45)) {
        cursor += 1;
        skip_space(units, &mut cursor);
    }
    for (word, number) in [
        ("true", Number::Int(1)),
        ("false", Number::Int(0)),
        ("NaN", Number::Real(f64::NAN)),
        ("Infinity", Number::Real(f64::INFINITY)),
    ] {
        if word
            .bytes()
            .map(u16::from)
            .eq(units[cursor..].iter().take(word.len()).copied())
            && !units
                .get(cursor + word.len())
                .is_some_and(|&u| matches!(u, 65..=90 | 95 | 97..=122) || u >= 256)
        {
            return Some((signed(number, negative), cursor + word.len()));
        }
    }
    let start = cursor;
    let mut radix = 10;
    if units.get(cursor) == Some(&48) {
        match units.get(cursor + 1) {
            Some(88 | 120) => {
                radix = 16;
                cursor += 2;
            }
            Some(66 | 98) => {
                radix = 2;
                cursor += 2;
            }
            Some(80 | 112) => return None,
            Some(46 | 69 | 101) | None => {}
            _ => radix = 8,
        }
    }
    let digits = cursor;
    let mut word = 0_u64;
    while let Some(digit) = units
        .get(cursor)
        .and_then(|&u| digit(u))
        .filter(|&d| d < radix)
    {
        // Reference integer accumulation retains the low 64 bits. Accumulate
        // while scanning so integer conversions do not traverse the text twice.
        word = word
            .wrapping_mul(u64::from(radix))
            .wrapping_add(u64::from(digit));
        cursor += 1;
    }
    if radix != 10 {
        if matches!(units.get(cursor), Some(46 | 80 | 112)) {
            return Some(radix::real(units, digits, cursor, radix, negative));
        }
        if cursor == digits {
            return None;
        }
        return Some((signed(Number::Int(word as i64), negative), cursor));
    }
    let mut has_digits = cursor > digits;
    let mut real = false;
    if units.get(cursor) == Some(&46) {
        real = true;
        cursor += 1;
        let fraction = cursor;
        while units.get(cursor).is_some_and(|u| matches!(u, 48..=57)) {
            cursor += 1;
        }
        has_digits |= cursor > fraction;
    }
    let mantissa_end = cursor;
    let mut exponent = None;
    if matches!(units.get(cursor), Some(69 | 101)) {
        real = true;
        cursor += 1;
        skip_space(units, &mut cursor);
        let sign = units.get(cursor).copied().filter(|u| matches!(u, 43 | 45));
        if sign.is_some() {
            cursor += 1;
            skip_space(units, &mut cursor);
        }
        let exp_start = cursor;
        while units.get(cursor).is_some_and(|u| matches!(u, 48..=57)) {
            cursor += 1;
        }
        if cursor > exp_start {
            exponent = Some((sign, exp_start, cursor));
        }
    }
    if real {
        // ExtractNumber accepts a point/exponent even without a mantissa.
        // wcstod then returns zero, while the Variant remains a Real.
        if !has_digits {
            return Some((signed(Number::Real(0.0), negative), cursor));
        }
        let mut text = String::with_capacity(cursor - start + 1);
        if negative {
            text.push('-');
        }
        text.extend(
            units[start..mantissa_end]
                .iter()
                .map(|&u| char::from(u as u8)),
        );
        if let Some((sign, first, last)) = exponent {
            text.push('e');
            if let Some(sign) = sign {
                text.push(char::from(sign as u8));
            }
            text.extend(units[first..last].iter().map(|&u| char::from(u as u8)));
        }
        return Some((
            Number::Real(text.parse().expect("scanned decimal number")),
            cursor,
        ));
    }
    if cursor == digits {
        return None;
    }
    Some((signed(Number::Int(word as i64), negative), cursor))
}

fn signed(number: Number, negative: bool) -> Number {
    if !negative {
        return number;
    }
    match number {
        Number::Int(value) => Number::Int(value.wrapping_neg()),
        Number::Real(value) => Number::Real(-value),
    }
}

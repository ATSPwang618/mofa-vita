use super::*;
fn legacy(text: &[u16], output: &mut Vec<u8>) -> NativeResult<()> {
    if text.len() < 4 {
        return Ok(());
    }
    let unit = |index| {
        if index == text.len() {
            Ok(0) // std::wstring's accessible C-string terminator.
        } else {
            text.get(index)
                .copied()
                .ok_or(NativeError::Message("incomplete Base64 group"))
        }
    };
    let number = |index| match unit(index)? {
        c @ 65..=90 => Ok((c - 65) as u8),
        c @ 97..=122 => Ok((c - 71) as u8),
        c @ 48..=57 => Ok((c + 4) as u8),
        43 => Ok(62),
        47 => Ok(63),
        0..=255 => Ok(0),
        _ => Err(NativeError::Message(
            "Base64 character outside the byte table",
        )),
    };
    let mut pos = 0;
    while pos < text.len() - 4 {
        let (a, b, c, d) = (
            number(pos)?,
            number(pos + 1)?,
            number(pos + 2)?,
            number(pos + 3)?,
        );
        output.extend_from_slice(&[(a << 2) | (b >> 4), (b << 4) | (c >> 2), (c << 6) | d]);
        pos += 4;
    }
    let (a, b) = (number(pos)?, number(pos + 1)?);
    output.push((a << 2) | (b >> 4));
    if unit(pos + 2)? != u16::from(b'=') {
        let c = number(pos + 2)?;
        output.push((b << 4) | (c >> 2));
        if unit(pos + 3)? != u16::from(b'=') {
            output.push((c << 6) | number(pos + 3)?);
        }
    }
    Ok(())
}

#[test]
fn base64_table_retains_legacy_padding_invalid_bytes_and_partial_errors() {
    for length in [0, 1, 2, 3, 4, 5, 6, 7, 8, 12] {
        for at in 0..length {
            for value in (0..=256).chain([0xd800, 0xffff]) {
                let mut text = vec![65; length];
                text[at] = value;
                let (mut expected, mut got) = (vec![71], vec![71]);
                let a = legacy(&text, &mut expected);
                let b = decode_base64(&text, &mut got);
                assert_eq!(
                    a.is_ok(),
                    b.is_ok(),
                    "length={length} at={at} value={value}"
                );
                assert_eq!(expected, got);
            }
        }
    }
}

use crate::{NativeError, NativeResult};

/// tjsOctPack.cpp's table maps every non-alphabet byte to zero. Only the
/// final group's third/fourth '=' terminates output; earlier '=' is data.
pub(super) fn decode_base64(text: &[u16], output: &mut Vec<u8>) -> NativeResult<()> {
    if text.len() < 4 {
        return Ok(());
    }
    // Reserve a normal block once, but keep malformed large inputs from
    // eagerly allocating their entire possible output before validation.
    output.reserve((text.len().div_ceil(4) * 3).min(64 * 1024));
    let unit = |index| {
        if index == text.len() {
            Ok(0) // std::wstring's accessible C-string terminator.
        } else {
            text.get(index)
                .copied()
                .ok_or(NativeError::Message("incomplete Base64 group"))
        }
    };
    static TABLE: [u8; 256] = {
        let mut table = [0; 256];
        let mut i = 0;
        while i < 26 {
            table[65 + i] = i as u8;
            table[97 + i] = (26 + i) as u8;
            i += 1;
        }
        i = 0;
        while i < 10 {
            table[48 + i] = (52 + i) as u8;
            i += 1;
        }
        table[43] = 62;
        table[47] = 63;
        table
    };
    let number = |index| {
        TABLE
            .get(usize::from(unit(index)?))
            .copied()
            .ok_or(NativeError::Message(
                "Base64 character outside the byte table",
            ))
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

/// BinToAscii removes zero bytes before ttstr(char*) invokes TJS_mbstowcs.
/// krkrz's CP932 table excludes WHATWG's 0x80 and user-defined mappings.
pub(super) fn decode_text(bytes: &[u8]) -> NativeResult<Vec<u16>> {
    let filtered;
    let bytes = if bytes.contains(&0) {
        filtered = bytes
            .iter()
            .copied()
            .filter(|&b| b != 0)
            .collect::<Vec<_>>();
        &filtered
    } else {
        bytes
    };
    let invalid = || NativeError::Message("invalid CP932 string");
    if bytes.is_ascii() {
        return Ok(bytes.iter().map(|&byte| u16::from(byte)).collect());
    }
    let mut pos = 0;
    while let Some(&byte) = bytes.get(pos) {
        pos += match byte {
            0x01..=0x7f | 0xa1..=0xdf => 1,
            0x81..=0x9f | 0xe0..=0xef | 0xfa..=0xfc => 2,
            _ => return Err(invalid()),
        };
    }
    let mut text = vec![0; bytes.len()];
    let (result, _, written) = encoding_rs::SHIFT_JIS
        .new_decoder_without_bom_handling()
        .decode_to_utf16_without_replacement(bytes, &mut text, true);
    if result != encoding_rs::DecoderResult::InputEmpty {
        return Err(invalid());
    }
    text.truncate(written);
    Ok(text)
}

#[cfg(test)]
#[path = "../../tests/internal/octet_base64.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/internal/octet_cp932.rs"]
mod cp932_tests;

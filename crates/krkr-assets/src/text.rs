//! Kirikiri text streams: BOMs, legacy encodings, simple cipher modes and zlib.
use crate::{
    Error, Result,
    binary::{Cursor, inflate, size},
    name,
};
use std::io::Write;

pub fn offset(mode: &[u16]) -> Result<Option<u64>> {
    let Some(at) = mode.iter().position(|&u| u == 111) else {
        return Ok(None);
    };
    let mut value = 0u64;
    for &u in &mode[at + 1..] {
        if !(48..=57).contains(&u) {
            break;
        }
        value = value
            .checked_mul(10)
            .and_then(|n| n.checked_add(u64::from(u - 48)))
            .ok_or(Error::Format("text offset overflow"))?;
    }
    Ok(Some(value))
}
fn utf16(bytes: &[u8], little: bool) -> Result<Vec<u16>> {
    if bytes.len() % 2 != 0 {
        return Err(Error::Format("incomplete UTF-16 code unit"));
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|p| {
            if little {
                u16::from_le_bytes([p[0], p[1]])
            } else {
                u16::from_be_bytes([p[0], p[1]])
            }
        })
        .collect())
}
pub fn decode(bytes: &[u8], encoding: &[u16], limit: usize) -> Result<Vec<u16>> {
    size(bytes.len() as u64, limit, "text bytes")?;
    if bytes.starts_with(&[0xfe, 0xfe]) {
        let mut cursor = Cursor(&bytes[2..]);
        let mode = cursor.take(1)?[0];
        if cursor.take(2)? != [0xff, 0xfe] {
            return Err(Error::Format("invalid encoded-text BOM"));
        }
        if mode == 2 {
            let stored = size(cursor.u64()?, limit, "compressed text")?;
            let original = size(cursor.u64()?, limit, "text bytes")?;
            return utf16(&inflate(cursor.take(stored)?, original)?, true);
        }
        let mut text = utf16(cursor.0, true)?;
        match mode {
            0 => {
                for u in &mut text {
                    if *u >= 0x20 {
                        *u ^= ((*u & 0xfe) << 8) ^ 1;
                    }
                }
            }
            1 => {
                for u in &mut text {
                    *u = ((*u & 0xaaaa) >> 1) | ((*u & 0x5555) << 1);
                }
            }
            _ => return Err(Error::Format("unsupported text cipher mode")),
        }
        return Ok(text);
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xff, 0xfe]) {
        return utf16(bytes, true);
    }
    if let Some(bytes) = bytes.strip_prefix(&[0xfe, 0xff]) {
        return utf16(bytes, false);
    }
    let (bytes, encoding) = if let Some(bytes) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        (bytes, encoding_rs::UTF_8)
    } else {
        (
            bytes,
            encoding_rs::Encoding::for_label(String::from_utf16_lossy(encoding).as_bytes())
                .ok_or(Error::Format("unknown text encoding"))?,
        )
    };
    // Decode directly to the VM's UTF-16 representation. Shift-JIS previously
    // allocated a UTF-8 string and walked it twice to count and convert units.
    let mut decoder = encoding.new_decoder();
    let capacity = decoder
        .max_utf16_buffer_length(bytes.len())
        .ok_or(Error::Limit("decoded text"))?
        .min((limit / 2).saturating_add(2));
    let mut decoded = vec![0; capacity];
    let (result, mut read, written) =
        decoder.decode_to_utf16_without_replacement(bytes, &mut decoded, true);
    match result {
        encoding_rs::DecoderResult::Malformed(..) => {
            return Err(Error::Format("invalid text encoding"));
        }
        encoding_rs::DecoderResult::OutputFull => {
            // Keep malformed-input precedence even after exceeding the output
            // limit, without allocating the rest of an oversized result.
            let mut discarded = [0; 1024];
            loop {
                let (result, count, _) = decoder.decode_to_utf16_without_replacement(
                    &bytes[read..],
                    &mut discarded,
                    true,
                );
                read += count;
                match result {
                    encoding_rs::DecoderResult::Malformed(..) => {
                        return Err(Error::Format("invalid text encoding"));
                    }
                    encoding_rs::DecoderResult::InputEmpty => break,
                    encoding_rs::DecoderResult::OutputFull => {}
                }
            }
            return Err(Error::Limit("decoded text"));
        }
        encoding_rs::DecoderResult::InputEmpty => {}
    }
    if written > limit / 2 {
        return Err(Error::Limit("decoded text"));
    }
    decoded.truncate(written);
    Ok(decoded)
}
pub fn encode(text: &[u16], mode: &[u16], limit: usize) -> Result<Vec<u8>> {
    if text.len() > limit.saturating_sub(21) / 2 {
        return Err(Error::Limit("text bytes"));
    }
    // utf-8 remains a supported modern output mode; b is the container's binary selector.
    if name::c_string(mode)
        .windows(5)
        .any(|s| matches!(s, [117 | 85, 116 | 84, 102 | 70, 45, 56]))
    {
        let bytes = String::from_utf16(text)
            .map_err(|_| Error::Format("isolated surrogate in UTF-8 output"))?
            .into_bytes();
        size(bytes.len() as u64, limit, "text bytes")?;
        return Ok(bytes);
    }
    let mut cipher = None;
    if let Some(at) = mode.iter().position(|&u| u == 99) {
        let choice = mode
            .get(at + 1)
            .filter(|u| (48..=57).contains(*u))
            .copied()
            .unwrap_or(49)
            - 48;
        if choice != 1 {
            return Err(Error::Format("unsupported output cipher mode"));
        }
        cipher = Some(1);
    }
    let mut compression = flate2::Compression::default();
    if let Some(at) = mode.iter().position(|&u| u == 122) {
        cipher = Some(2);
        if let Some(&level) = mode.get(at + 1).filter(|u| (48..=57).contains(*u)) {
            compression = flate2::Compression::new(u32::from(level - 48));
        }
    }
    let capacity = text.len() * 2 + 21;
    let mut bytes = Vec::with_capacity(if cipher == Some(2) {
        capacity.min(8192)
    } else {
        capacity
    });
    if let Some(cipher) = cipher {
        bytes.extend_from_slice(&[0xfe, 0xfe, cipher]);
    }
    bytes.extend_from_slice(&[0xff, 0xfe]);
    if cipher == Some(2) {
        // Encode into the final container; fill its length fields afterwards.
        // Compressed saves need neither an uncompressed-sized allocation nor
        // another complete copy of the compressed stream.
        bytes.resize(21, 0);
        let mut encoder = flate2::write::ZlibEncoder::new(bytes, compression);
        let mut buffer = [0; 8192];
        for chunk in text.chunks(buffer.len() / 2) {
            for (pair, &u) in buffer.chunks_exact_mut(2).zip(chunk) {
                pair.copy_from_slice(&u.to_le_bytes());
            }
            encoder.write_all(&buffer[..chunk.len() * 2])?;
        }
        bytes = encoder.finish()?;
        size(bytes.len() as u64, limit, "compressed text")?;
        let stored = bytes.len() as u64 - 21;
        bytes[5..13].copy_from_slice(&stored.to_le_bytes());
        bytes[13..21].copy_from_slice(&(text.len() as u64 * 2).to_le_bytes());
    } else {
        bytes.extend(text.iter().flat_map(|&u| {
            let u = if cipher == Some(1) {
                ((u & 0xaaaa) >> 1) | ((u & 0x5555) << 1)
            } else {
                u
            };
            u.to_le_bytes()
        }));
    }
    Ok(bytes)
}

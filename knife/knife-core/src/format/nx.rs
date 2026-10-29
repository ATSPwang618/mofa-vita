//! `NXGX` (gzip) and `NXCX` (zlib) compressed wrappers.
//!
//! Header: magic (4), `u32` uncompressed size, `u32` compressed size, 4 bytes padding.

use std::io::Read;

use anyhow::{Context, Result, bail, ensure};

pub const HEADER_LEN: usize = 16;

/// Bound a single decoded wrapper independently of the archive containing it.
pub const DEFAULT_MAX_UNPACKED_BYTES: usize = if cfg!(target_os = "vita") {
    32 * 1024 * 1024
} else {
    256 * 1024 * 1024
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NxKind {
    Gzip,
    Zlib,
}

#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub kind: NxKind,
    pub unpacked_len: u64,
    pub stored_len: u64,
}

pub fn kind(data: &[u8]) -> Option<NxKind> {
    match data.get(..4)? {
        b"NXGX" => Some(NxKind::Gzip),
        b"NXCX" => Some(NxKind::Zlib),
        _ => None,
    }
}

/// Parse the fixed header before allocating or reading its payload.
pub fn header(data: &[u8]) -> Result<Option<Header>> {
    let Some(kind) = kind(data) else {
        ensure!(
            !data.starts_with(b"NX"),
            "unsupported or truncated NX wrapper magic"
        );
        return Ok(None);
    };
    let bytes = data.get(..HEADER_LEN).context("truncated NX header")?;
    let unpacked_len = u64::from(u32::from_le_bytes(bytes[4..8].try_into().unwrap()));
    let compressed_len = u64::from(u32::from_le_bytes(bytes[8..12].try_into().unwrap()));
    Ok(Some(Header {
        kind,
        unpacked_len,
        stored_len: (HEADER_LEN as u64)
            .checked_add(compressed_len)
            .context("NX stored length overflow")?,
    }))
}

/// Complete header length, when it fits the target's address space.
/// Use `header` to preserve malformed-input errors.
pub fn stored_len(data: &[u8]) -> Option<usize> {
    usize::try_from(header(data).ok()??.stored_len).ok()
}

pub fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    decompress_bounded(data, DEFAULT_MAX_UNPACKED_BYTES)
}

pub fn decompress_bounded(data: &[u8], max_unpacked_bytes: usize) -> Result<Vec<u8>> {
    let Some(header) = header(data)? else {
        bail!("not an NX stream")
    };
    ensure!(
        header.unpacked_len <= max_unpacked_bytes as u64,
        "NX unpacked length {} exceeds limit {max_unpacked_bytes}",
        header.unpacked_len
    );
    let stored =
        usize::try_from(header.stored_len).context("NX stored length exceeds address space")?;
    let unpacked =
        usize::try_from(header.unpacked_len).context("NX unpacked length exceeds address space")?;
    let body = data
        .get(HEADER_LEN..stored)
        .context("truncated NX compressed payload")?;
    // Cursor implements BufRead without speculative input reads. Limiting each
    // output read catches false declarations without allocating a bomb's output.
    let input = std::io::Cursor::new(body);
    let (out, consumed) = match header.kind {
        NxKind::Gzip => {
            let mut decoder = flate2::bufread::GzDecoder::new(input);
            let out = read_bounded(&mut decoder, unpacked)?;
            (out, decoder.get_ref().position())
        }
        NxKind::Zlib => {
            let mut decoder = flate2::bufread::ZlibDecoder::new(input);
            let out = read_bounded(&mut decoder, unpacked)?;
            (out, decoder.get_ref().position())
        }
    };
    ensure!(
        consumed == body.len() as u64,
        "NX compressed length mismatch: declared {}, consumed {consumed}",
        body.len()
    );
    Ok(out)
}

fn read_bounded(decoder: &mut dyn Read, unpacked: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(unpacked)
        .context("allocating bounded NX output")?;
    let mut decoder = decoder.take((unpacked as u64) + 1);
    let mut chunk = [0u8; 8192];
    loop {
        let count = decoder
            .read(&mut chunk)
            .context("decompressing NX payload")?;
        if count == 0 {
            break;
        }
        ensure!(
            count <= unpacked.saturating_sub(out.len()),
            "NX expansion exceeds declared length {unpacked}"
        );
        out.extend_from_slice(&chunk[..count]);
    }
    ensure!(
        out.len() == unpacked,
        "NX unpacked length mismatch: declared {unpacked}, decoded {}",
        out.len()
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn wrap(payload: &[u8], declared: u32) -> Vec<u8> {
        let mut compressor =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        compressor.write_all(payload).unwrap();
        let compressed = compressor.finish().unwrap();
        let mut out = b"NXCX".to_vec();
        out.extend_from_slice(&declared.to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&compressed);
        out
    }

    #[test]
    fn truncated_headers_are_errors_without_panicking() {
        for len in 4..HEADER_LEN {
            let mut data = vec![0; len];
            data[..4].copy_from_slice(b"NXCX");
            let result = std::panic::catch_unwind(|| decompress(&data));
            assert!(result.is_ok(), "header length {len} panicked");
            assert!(result.unwrap().is_err(), "header length {len} was accepted");
        }
    }

    #[test]
    fn stored_length_requires_the_complete_header() {
        let mut data = vec![0; 12];
        data[..4].copy_from_slice(b"NXGX");
        assert!(stored_len(&data).is_none());
    }

    #[test]
    fn compressed_extent_must_be_present_even_when_stream_already_ended() {
        let mut data = wrap(b"valid stream", 12);
        let csz = u32::from_le_bytes(data[8..12].try_into().unwrap());
        data[8..12].copy_from_slice(&(csz + 1).to_le_bytes());
        assert!(decompress(&data).is_err());
    }

    #[test]
    fn compressed_extent_cannot_include_bytes_after_the_stream() {
        let mut data = wrap(b"valid stream", 12);
        data.push(0);
        let compressed = (data.len() - HEADER_LEN) as u32;
        data[8..12].copy_from_slice(&compressed.to_le_bytes());
        assert!(decompress(&data).is_err());
    }

    #[test]
    fn expansion_must_match_the_declared_length() {
        assert!(decompress(&wrap(b"longer than declared", 1)).is_err());
        assert!(decompress(&wrap(b"short", 6)).is_err());
    }

    #[test]
    fn valid_stream_keeps_its_bytes() {
        assert_eq!(
            decompress(&wrap(b"normal input", 12)).unwrap(),
            b"normal input"
        );
    }

    #[test]
    fn unknown_nx_magic_is_not_treated_as_an_uncompressed_entry() {
        assert!(header(b"NXZZabcdefghijkl").is_err());
    }

    #[test]
    fn incomplete_zlib_stream_is_rejected_even_with_correct_output_length() {
        for removed in 1..=4 {
            let mut data = wrap(b"complete output before checksum", 31);
            data.truncate(data.len() - removed);
            let compressed = (data.len() - HEADER_LEN) as u32;
            data[8..12].copy_from_slice(&compressed.to_le_bytes());
            assert!(
                decompress(&data).is_err(),
                "missing {removed} checksum bytes accepted"
            );
        }
    }

    #[test]
    fn empty_and_gzip_streams_are_supported() {
        assert!(decompress(&wrap(b"", 0)).unwrap().is_empty());
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(b"gzip payload").unwrap();
        let body = encoder.finish().unwrap();
        let mut data = b"NXGX".to_vec();
        data.extend_from_slice(&12u32.to_le_bytes());
        data.extend_from_slice(&(body.len() as u32).to_le_bytes());
        data.extend_from_slice(&[0; 4]);
        data.extend_from_slice(&body);
        assert_eq!(decompress(&data).unwrap(), b"gzip payload");
        data.pop();
        let compressed = (data.len() - HEADER_LEN) as u32;
        data[8..12].copy_from_slice(&compressed.to_le_bytes());
        assert!(decompress(&data).is_err());
    }
}

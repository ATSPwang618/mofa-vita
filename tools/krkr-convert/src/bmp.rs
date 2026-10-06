//! Offline recovery of a truncated uncompressed BMP32 pixel tail.
use crate::media::{Result, at};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub(crate) struct Repaired {
    pub file: tempfile::NamedTempFile,
    pub detail: String,
}

pub(crate) fn repair(source: &Path) -> Result<Option<Repaired>> {
    let mut input = File::open(source).map_err(|e| at(source, e))?;
    let length = input.metadata().map_err(|e| at(source, e))?.len();
    let mut header = [0; 54];
    if length < header.len() as u64 {
        return Ok(None);
    }
    input.read_exact(&mut header).map_err(|e| at(source, e))?;
    // Other bit depths and compressed layouts have no unambiguous transparent
    // pixel representation. Leave their validation to the normal decoder.
    if &header[..2] != b"BM"
        || header[14..18] != 40u32.to_le_bytes()
        || header[26..28] != 1u16.to_le_bytes()
        || header[28..30] != 32u16.to_le_bytes()
        || header[30..34] != [0; 4]
    {
        return Ok(None);
    }
    let width = i32::from_le_bytes(header[18..22].try_into().unwrap());
    let height = i32::from_le_bytes(header[22..26].try_into().unwrap()).unsigned_abs();
    if !(1..=65535).contains(&width) || !(1..=65535).contains(&height) {
        return Ok(None);
    }
    let offset = u64::from(u32::from_le_bytes(header[10..14].try_into().unwrap()));
    if offset < 54 || offset > length {
        return Ok(None);
    }
    let pixels = width as u64 * u64::from(height) * 4;
    let expected = offset + pixels;
    if length >= expected {
        return Ok(None);
    }
    // Match the image input limit; do not expand a tiny corrupt file to an
    // arbitrarily large temporary image before the decoder checks its budget.
    if expected > 256 * 1024 * 1024 {
        return Err(at(
            source,
            "repaired BMP would exceed the image input limit",
        ));
    }
    let missing = expected - length;
    let complete_end = offset + (length - offset) / 4 * 4;
    let repaired_pixels = (expected - complete_end) / 4;
    let mut file = tempfile::Builder::new()
        .suffix(".bmp")
        .tempfile()
        .map_err(|e| at(source, e))?;
    input.rewind().map_err(|e| at(source, e))?;
    std::io::copy(&mut input.take(complete_end), &mut file).map_err(|e| at(source, e))?;
    // A partial pixel is replaced as a whole, including alpha. All complete
    // source pixels retain their bytes and their original scanline order.
    let zeros = [0; 4096];
    let mut remaining = expected - complete_end;
    while remaining != 0 {
        let count = remaining.min(zeros.len() as u64) as usize;
        file.write_all(&zeros[..count]).map_err(|e| at(source, e))?;
        remaining -= count as u64;
    }
    for (position, value) in [(2, expected as u32), (34, pixels as u32)] {
        file.seek(SeekFrom::Start(position))
            .map_err(|e| at(source, e))?;
        file.write_all(&value.to_le_bytes())
            .map_err(|e| at(source, e))?;
    }
    Ok(Some(Repaired {
        file,
        detail: format!(
            "BMP pixel data truncated by {missing} bytes; filled {repaired_pixels} incomplete or missing pixels with transparent RGBA"
        ),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap(height: i32) -> Vec<u8> {
        let mut bytes = vec![0; 54];
        bytes[..2].copy_from_slice(b"BM");
        for (at, value) in [(2, 70u32), (10, 54), (14, 40), (18, 2), (34, 16)] {
            bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[22..26].copy_from_slice(&height.to_le_bytes());
        bytes[26..28].copy_from_slice(&1u16.to_le_bytes());
        bytes[28..30].copy_from_slice(&32u16.to_le_bytes());
        bytes.extend(1..=16);
        bytes
    }

    #[test]
    fn missing_tail_pixels_are_transparent_and_complete_pixels_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bmp");
        for height in [2, -2] {
            let original = bitmap(height);
            std::fs::write(&source, &original).unwrap();
            assert!(repair(&source).unwrap().is_none());
            for missing in [1, 4, 5, 15, 16] {
                let truncated = &original[..original.len() - missing];
                std::fs::write(&source, truncated).unwrap();
                let result = repair(&source).unwrap().unwrap();
                let output = std::fs::read(result.file.path()).unwrap();
                let complete_end = 54 + (16 - missing) / 4 * 4;
                assert_eq!(&output[..complete_end], &original[..complete_end]);
                assert_eq!(output.len(), original.len());
                assert!(output[complete_end..].iter().all(|&b| b == 0));
                assert!(result.detail.contains(&format!("{missing} bytes")));
                assert_eq!(std::fs::read(&source).unwrap(), truncated);
                assert!(repair(result.file.path()).unwrap().is_none());
            }
        }
    }

    #[test]
    fn unrelated_layouts_and_oversized_repairs_are_not_fabricated() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bmp");
        for (at, bytes) in [(28, vec![24, 0]), (30, vec![1, 0, 0, 0]), (10, vec![0; 4])] {
            let mut original = bitmap(2);
            original.truncate(60);
            original[at..at + bytes.len()].copy_from_slice(&bytes);
            std::fs::write(&source, original).unwrap();
            assert!(repair(&source).unwrap().is_none());
        }
        let mut original = bitmap(65535);
        original[18..22].copy_from_slice(&65535u32.to_le_bytes());
        std::fs::write(&source, original).unwrap();
        assert!(repair(&source).is_err());
    }
}

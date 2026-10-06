use super::{
    codec::{Mode, error},
    *,
};
use std::io::Cursor;
pub struct Header<'a> {
    pub size: Size,
    raw: &'a [u8],
    tail: &'a [u8],
    scratch: usize,
}
impl Header<'_> {
    pub(crate) fn decode_scratch_bytes(&self) -> Result<usize> {
        self.scratch
            .checked_add(
                self.size
                    .rgba_bytes()
                    .unwrap()
                    .saturating_mul(if self.raw.starts_with(b"TLG6.") { 2 } else { 1 }),
            )
            .ok_or(Error::Message("TLG decode size overflow"))
    }
}
fn u32le(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        data.get(at..at + 4)
            .ok_or(Error::Message("truncated TLG header"))?
            .try_into()
            .unwrap(),
    ))
}
pub fn probe(data: &[u8]) -> Result<Header<'_>> {
    let (raw, tail) = if data.starts_with(b"TLG0.0\x00sds\x1a") {
        let length = u32le(data, 11)? as usize;
        let end = 15usize
            .checked_add(length)
            .filter(|&n| n <= data.len())
            .ok_or(Error::Message("truncated TLG stream"))?;
        (&data[15..end], &data[end..])
    } else {
        (data, &[][..])
    };
    let (size, scratch) = if raw.starts_with(b"TLG5.0\x00raw\x1a") {
        let colors = *raw.get(11).ok_or(Error::Message("truncated TLG5 header"))? as usize;
        if !matches!(colors, 3 | 4) {
            return Err(Error::Message("unsupported TLG5 color type"));
        }
        let size = image_size(u32le(raw, 12)?, u32le(raw, 16)?)?;
        let block = u32le(raw, 20)? as usize;
        if block == 0 {
            return Err(Error::Message("invalid TLG5 block height"));
        }
        let block_bytes = block
            .checked_mul(size.width as usize)
            .and_then(|n| n.checked_add(10))
            .ok_or(Error::Message("TLG5 block size overflow"))?;
        let blocks = (size.height as usize).div_ceil(block);
        let mut at = 24usize
            .checked_add(
                blocks
                    .checked_mul(4)
                    .ok_or(Error::Message("TLG5 block count overflow"))?,
            )
            .ok_or(Error::Message("TLG5 block count overflow"))?;
        for _ in 0..blocks {
            for _ in 0..colors {
                let length = u32le(
                    raw,
                    at.checked_add(1)
                        .ok_or(Error::Message("TLG5 block overflow"))?,
                )? as usize;
                if length > block_bytes {
                    return Err(Error::Message("TLG5 compressed block exceeds its buffer"));
                }
                at = at
                    .checked_add(5)
                    .and_then(|n| n.checked_add(length))
                    .filter(|&n| n <= raw.len())
                    .ok_or(Error::Message("truncated TLG5 block"))?;
            }
        }
        let scratch = block_bytes
            .checked_mul(colors + 1)
            .and_then(|n| n.checked_add(size.width as usize * colors * 2 + 4096))
            .ok_or(Error::Message("TLG5 scratch overflow"))?;
        (size, scratch)
    } else if raw.starts_with(b"TLG6.0\x00raw\x1a") {
        let size = image_size(u32le(raw, 15)?, u32le(raw, 19)?)?;
        let bits = u32le(raw, 23)? as usize;
        let filter_bytes = u32le(raw, 27)? as usize;
        if filter_bytes > raw.len().saturating_sub(31) {
            return Err(Error::Message("truncated TLG6 filter data"));
        }
        let blocks = (size.width as usize).div_ceil(8) * (size.height as usize).div_ceil(8);
        let scratch = bits / 8 + 5 + size.width as usize * 40 + blocks + filter_bytes + 4096;
        (size, scratch)
    } else {
        return Err(Error::Message("unsupported TLG signature"));
    };
    Ok(Header {
        size,
        raw,
        tail,
        scratch,
    })
}
pub fn decode(
    data: &[u8],
    mode: Mode,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<(Bytes, Tags)> {
    if mode != Mode::Main {
        return Err(Error::Message(
            "TLG cannot be loaded as a mask or province image",
        ));
    }
    let header = probe(data)?;
    let rgba_bytes = header.size.rgba_bytes().unwrap();
    // libtlg keeps its decoded native plane and (for TLG6) a u32 work image.
    // Preflight all header-controlled allocations before entering the codec.
    let _scratch = reserve(budget, header.decode_scratch_bytes()?)?;
    let mut output = Bytes::zeroed(rgba_bytes, budget)?;
    let stream = Interruptible {
        cursor: Cursor::new(header.raw),
        cancelled,
    };
    // The external decoder indexes entropy buffers directly. A malformed
    // stream must become a catchable load error without killing the IO worker.
    let decoded =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| libtlg_rs::load_tlg(stream)))
            .map_err(|_| Error::Message("malformed TLG compressed data"))?
            .map_err(error)?;
    check(cancelled)?;
    let channels = match decoded.color {
        libtlg_rs::TlgColorType::Grayscale8 => 1,
        libtlg_rs::TlgColorType::Bgr24 => 3,
        libtlg_rs::TlgColorType::Bgra32 => 4,
    };
    for (src, dst) in decoded
        .data
        .chunks_exact(header.size.width as usize * channels)
        .zip(
            output
                .as_mut_slice()
                .chunks_exact_mut(header.size.width as usize * 4),
        )
    {
        check(cancelled)?;
        for (s, d) in src
            .chunks_exact(channels)
            .zip(dst.as_chunks_mut::<4>().0.iter_mut())
        {
            let p = if channels == 1 {
                // The original TLG6 one-channel path writes only the blue
                // byte of a zero-initialized BGRA pixel.
                [0, 0, s[0], 0]
            } else {
                [s[2], s[1], s[0], if channels == 4 { s[3] } else { 255 }]
            };
            d.copy_from_slice(&p);
        }
    }
    Ok((output, tags(header.tail)?))
}
struct Interruptible<'a> {
    cursor: Cursor<&'a [u8]>,
    cancelled: &'a AtomicBool,
}
impl std::io::Read for Interruptible<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        // read_exact retries Interrupted forever; cancellation must escape it.
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(std::io::Error::other("image decode cancelled"));
        }
        self.cursor.read(output)
    }
}
impl std::io::Seek for Interruptible<'_> {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        std::io::Seek::seek(&mut self.cursor, position)
    }
}
fn tags(mut data: &[u8]) -> Result<Tags> {
    let mut result = Vec::new();
    let mut retained = 0usize;
    // The original loader stops when fewer than four bytes remain for a chunk
    // name. Some shipped TLGs retain 1-3 bytes after their last complete chunk.
    while data.len() >= 4 {
        let (name, payload) = chunk(&mut data)?;
        if name == b"tags" {
            tag_pairs(payload, &mut result, &mut retained)?;
        }
    }
    Ok(result)
}

fn chunk<'a>(data: &mut &'a [u8]) -> Result<(&'a [u8], &'a [u8])> {
    let length = u32le(data, 4)? as usize;
    let end = 8usize
        .checked_add(length)
        .ok_or(Error::Message("TLG metadata overflow"))?;
    let payload = data
        .get(8..end)
        .ok_or(Error::Message("truncated TLG metadata"))?;
    let name = &data[..4];
    *data = &data[end..];
    Ok((name, payload))
}

fn tag_text(data: &[u8]) -> Result<(String, bool)> {
    if let Ok(text) = std::str::from_utf8(data) {
        return Ok((text.into(), false));
    }
    // Legacy Kirikiri tools wrote Windows Japanese AnsiString tags, while
    // current writers use UTF-8. Do not silently replace undecodable bytes.
    let text = encoding_rs::SHIFT_JIS
        .decode_without_bom_handling_and_without_replacement(data)
        .ok_or(Error::Message("TLG tag is neither UTF-8 nor Shift-JIS"))?;
    Ok((text.into_owned(), true))
}

fn tag_pairs(mut text: &[u8], result: &mut Tags, retained: &mut usize) -> Result<bool> {
    let mut legacy = false;
    while !text.is_empty() {
        let name = field(&mut text)?;
        if text.first() != Some(&b'=') {
            return Err(Error::Message("invalid TLG tag separator"));
        }
        text = &text[1..];
        let value = field(&mut text)?;
        if text.first() != Some(&b',') {
            return Err(Error::Message("invalid TLG tag terminator"));
        }
        text = &text[1..];
        if name.len() + value.len() > (64 * 1024usize).saturating_sub(*retained)
            || result.len() >= 1024
        {
            return Err(Error::Message("TLG metadata budget exceeded"));
        }
        let (name, legacy_name) = tag_text(name)?;
        let (value, legacy_value) = tag_text(value)?;
        *retained += name.len() + value.len();
        if *retained > 64 * 1024 {
            return Err(Error::Message("TLG metadata budget exceeded"));
        }
        legacy |= legacy_name || legacy_value;
        result.push((name, value));
    }
    Ok(legacy)
}

pub(super) fn normalize_metadata(data: &[u8]) -> Result<Option<Vec<u8>>> {
    let header = probe(data)?;
    let mut remaining = header.tail;
    let mut tail = Vec::new();
    let mut pairs = Vec::new();
    let mut retained = 0;
    let mut changed = false;
    while remaining.len() >= 4 {
        let (name, payload) = chunk(&mut remaining)?;
        let first = pairs.len();
        let legacy = name == b"tags" && tag_pairs(payload, &mut pairs, &mut retained)?;
        tail.extend_from_slice(name);
        if legacy {
            use std::fmt::Write;
            let mut text = String::new();
            for (key, value) in &pairs[first..] {
                write!(
                    &mut text,
                    "{}:{}={}:{},",
                    key.len(),
                    key,
                    value.len(),
                    value
                )
                .unwrap();
            }
            tail.extend_from_slice(&(text.len() as u32).to_le_bytes());
            tail.extend_from_slice(text.as_bytes());
            changed = true;
        } else {
            tail.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            tail.extend_from_slice(payload);
        }
    }
    if !changed && remaining.is_empty() {
        return Ok(None);
    }
    let mut output = data[..data.len() - header.tail.len()].to_vec();
    output.extend_from_slice(&tail);
    Ok(Some(output))
}

fn field<'a>(data: &mut &'a [u8]) -> Result<&'a [u8]> {
    let end = data
        .iter()
        .position(|&b| b == b':')
        .ok_or(Error::Message("invalid TLG tag length"))?;
    let length: usize = std::str::from_utf8(&data[..end])
        .map_err(error)?
        .parse()
        .map_err(error)?;
    let tail = &data[end + 1..];
    let value = tail
        .get(..length)
        .ok_or(Error::Message("truncated TLG tag"))?;
    *data = &tail[length..];
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_tag_encoding_and_truncated_chunks_still_fail() {
        let payload = b"5:names=1:\x81,"; // Incomplete Shift-JIS lead byte.
        let mut data = b"tags".to_vec();
        data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        data.extend_from_slice(payload);
        assert!(tags(&data).unwrap_err().to_string().contains("Shift-JIS"));
        assert!(tags(b"tags").is_err());
        assert!(tags(b"tags\x01\0\0\0").is_err());
        data.pop();
        assert!(tags(&data).is_err());
    }
}

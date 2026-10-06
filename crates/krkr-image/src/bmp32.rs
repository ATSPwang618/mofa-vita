//! Kirikiri treats the fourth byte in BI_RGB BMP32 as alpha. The generic BMP
//! decoder discards it, so handle this uncompressed layout without an RGB copy.
use super::{codec::Mode, *};
#[cfg(test)]
#[path = "../tests/internal/bmp32_decode.rs"]
mod tests;
pub(super) fn decode(
    data: &[u8],
    size: Size,
    mode: Mode,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Option<Bytes>> {
    let file_header = if data.starts_with(b"BM") { 14 } else { 0 };
    let Some(header) = data.get(file_header..file_header + 40) else {
        return Ok(None);
    };
    if header[..4] != 40u32.to_le_bytes()
        || header[14..16] != 32u16.to_le_bytes()
        || header[16..20] != [0; 4]
    {
        return Ok(None);
    }
    if mode == Mode::Province {
        return Err(Error::Message("province BMP must be palettized"));
    }
    let offset = if file_header == 0 {
        40
    } else {
        u32::from_le_bytes(data[10..14].try_into().unwrap()) as usize
    };
    let length = size
        .rgba_bytes()
        .ok_or(Error::Message("BMP size overflow"))?;
    let raw = offset
        .checked_add(length)
        .and_then(|end| data.get(offset..end))
        .ok_or(Error::Message("truncated BMP pixels"))?;
    let stride = size.width as usize * 4;
    let channels = if mode == Mode::Main { 4 } else { 1 };
    let bottom_up = i32::from_le_bytes(header[8..12].try_into().unwrap()) > 0;
    let mut output = Bytes::zeroed(length / 4 * channels, budget)?;
    for (y, target) in output
        .as_mut_slice()
        .chunks_exact_mut(size.width as usize * channels)
        .enumerate()
    {
        check(cancelled)?;
        let y = if bottom_up {
            size.height as usize - y - 1
        } else {
            y
        };
        let input = raw[y * stride..(y + 1) * stride].as_chunks::<4>().0;
        if mode == Mode::Main {
            for (p, out) in input.iter().zip(target.as_chunks_mut::<4>().0) {
                *out = [p[2], p[1], p[0], p[3]];
            }
        } else {
            for (p, out) in input.iter().zip(target) {
                *out = transform::gray(p[2], p[1], p[0]);
            }
        }
    }
    Ok(Some(output))
}

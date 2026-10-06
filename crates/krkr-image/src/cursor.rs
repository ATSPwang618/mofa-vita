//! CUR directory/hotspot handling; image's ICO decoder owns DIB/PNG pixels and
//! AND-mask decoding. Reading and decoding run on the existing IO worker.
use super::*;
use image::ImageDecoder;
use krkr_protocol::input_style::CursorImage;

pub fn read(plan: ReadPlan, budget: Budget, cancelled: &AtomicBool) -> Result<CursorImage> {
    check(cancelled)?;
    let length =
        usize::try_from(plan.bytes).map_err(|_| Error::Message("cursor file too large"))?;
    let mut encoded = Bytes::zeroed(length, &budget)?;
    let mut stream = plan.open()?;
    for chunk in encoded.as_mut_slice().chunks_mut(64 * 1024) {
        check(cancelled)?;
        stream.read_exact(chunk)?;
    }
    decode(encoded.as_slice(), &budget, cancelled)
}
pub fn decode(bytes: &[u8], budget: &Budget, cancelled: &AtomicBool) -> Result<CursorImage> {
    let malformed = || Error::Message("invalid CUR/ICO directory");
    let header = bytes.get(..6).ok_or_else(malformed)?;
    let u16le = |bytes: &[u8]| u16::from_le_bytes([bytes[0], bytes[1]]);
    let u32le = |bytes: &[u8]| u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let kind = u16le(&header[2..]);
    if u16le(header) != 0 || !matches!(kind, 1 | 2) {
        return Err(malformed());
    }
    let count = usize::from(u16le(&header[4..]));
    let directory = bytes.get(6..6 + count * 16).ok_or_else(malformed)?;
    let extent = |v: u8| if v == 0 { 256 } else { u16::from(v) };
    let entry = directory
        .as_chunks::<16>()
        .0
        .iter()
        .min_by_key(|entry| extent(entry[0]).abs_diff(32) + extent(entry[1]).abs_diff(32))
        .ok_or_else(malformed)?;
    let hotspot = if kind == 2 {
        (u16le(&entry[4..]), u16le(&entry[6..]))
    } else {
        (0, 0)
    };
    let (width, height) = (extent(entry[0]), extent(entry[1]));
    if hotspot.0 >= width || hotspot.1 >= height {
        return Err(Error::Message("cursor hotspot outside image"));
    }
    let (length, offset) = (u32le(&entry[8..]), u32le(&entry[12..]));
    let payload = bytes
        .get(offset..offset.checked_add(length).ok_or_else(malformed)?)
        .ok_or_else(malformed)?;
    // Present just the chosen frame as an ICO to the ecosystem decoder.
    let mut ico = Bytes::zeroed(22 + payload.len(), budget)?;
    let data = ico.as_mut_slice();
    data[..6].copy_from_slice(&[0, 0, 1, 0, 1, 0]);
    data[6..22].copy_from_slice(entry);
    data[10..14].copy_from_slice(&[1, 0, 32, 0]);
    data[18..22].copy_from_slice(&22u32.to_le_bytes());
    data[22..].copy_from_slice(payload);
    check(cancelled)?;
    let decoder = image::codecs::ico::IcoDecoder::new(std::io::Cursor::new(ico.as_slice()))
        .map_err(|e| Error::Codec(e.to_string()))?;
    if decoder.dimensions() != (u32::from(width), u32::from(height))
        || decoder.color_type() != image::ColorType::Rgba8
    {
        return Err(malformed());
    }
    let mut rgba = Bytes::zeroed(usize::from(width) * usize::from(height) * 4, budget)?;
    decoder
        .read_image(rgba.as_mut_slice())
        .map_err(|e| Error::Codec(e.to_string()))?;
    check(cancelled)?;
    Ok(CursorImage {
        width,
        height,
        hotspot,
        rgba,
    })
}

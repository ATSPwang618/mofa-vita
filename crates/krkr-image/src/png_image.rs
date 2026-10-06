use super::{
    codec::{Mode, error},
    *,
};
use std::io::Cursor;
mod metadata;
mod rows;
use metadata::tags;
pub(crate) use metadata::write_tags;
#[cfg(test)]
#[path = "../tests/png_decode/internal.rs"]
mod tests;
pub fn probe(data: &[u8]) -> Result<Size> {
    if data.len() < 33 || &data[..8] != b"\x89PNG\r\n\x1a\n" || &data[12..16] != b"IHDR" {
        return Err(Error::Message("invalid PNG header"));
    }
    image_size(
        u32::from_be_bytes(data[16..20].try_into().unwrap()),
        u32::from_be_bytes(data[20..24].try_into().unwrap()),
    )
}
pub(crate) fn scratch_bytes(size: Size) -> usize {
    (size.width as usize * 64).saturating_add(256 * 1024)
}
pub fn decode(
    data: &[u8],
    size: Size,
    mode: Mode,
    key: u32,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<(Bytes, Tags)> {
    let scratch_bytes = scratch_bytes(size);
    let _scratch = reserve(budget, scratch_bytes)?;
    let mut decoder = png::Decoder::new_with_limits(
        Cursor::new(data),
        png::Limits {
            bytes: scratch_bytes,
        },
    );
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut transformations = png::Transformations::STRIP_16;
    if mode == Mode::Main && data[25] == 0 {
        // Expand grayscale tRNS before stripping 16-bit samples, so two
        // values sharing their high byte do not become equally transparent.
        transformations |= png::Transformations::EXPAND;
    }
    decoder.set_transformations(transformations);
    let mut reader = decoder.read_info().map_err(error)?;
    let info = reader.info();
    if mode == Mode::Province
        && !matches!(
            info.color_type,
            png::ColorType::Indexed | png::ColorType::Grayscale
        )
    {
        return Err(Error::Message(
            "province PNG must be palettized or grayscale",
        ));
    }
    if mode == Mode::Province && info.bit_depth == png::BitDepth::Sixteen {
        return Err(Error::Message(
            "province PNG must have at most eight bits per sample",
        ));
    }
    let interlaced = info.interlaced;
    let (color, depth) = reader.output_color_type();
    let rows = rows::Rows::new(info, color, depth, mode, key)?;
    let channels_out = if mode == Mode::Main { 4 } else { 1 };
    let mut output = Bytes::zeroed(size.rgba_bytes().unwrap() / 4 * channels_out, budget)?;
    check(cancelled)?;
    if rows.direct() {
        // RGBA and one-byte gray/index planes already have the final layout.
        // Keep next_frame's direct destination writes, including Adam7.
        reader.next_frame(output.as_mut_slice()).map_err(error)?;
        rows.validate(output.as_slice())?;
    } else if !interlaced {
        // Convert while each decoded scanline is still hot. The decoder's
        // bounded row buffer replaces the former full raw image allocation.
        for out in output
            .as_mut_slice()
            .chunks_exact_mut(size.width as usize * channels_out)
        {
            check(cancelled)?;
            let row = reader
                .next_row()
                .map_err(error)?
                .ok_or(Error::Message("PNG has too few rows"))?;
            rows.convert(row.data(), out)?;
        }
        // Flush the image data and validate its tail, just like next_frame.
        if reader.next_row().map_err(error)?.is_some() {
            return Err(Error::Message("PNG has too many rows"));
        }
    } else {
        // The library scatters packed Adam7 samples before format conversion.
        // Preserve its exact padding/bit ordering for the less common layouts.
        let length = reader
            .output_buffer_size()
            .ok_or(Error::Message("PNG output size overflow"))?;
        let mut raw = Bytes::zeroed(length, budget)?;
        let frame = reader.next_frame(raw.as_mut_slice()).map_err(error)?;
        for (row, out) in raw.as_slice().chunks_exact(frame.line_size).zip(
            output
                .as_mut_slice()
                .chunks_exact_mut(size.width as usize * channels_out),
        ) {
            check(cancelled)?;
            rows.convert(row, out)?;
        }
    }
    check(cancelled)?;
    reader.finish().map_err(error)?;
    Ok((
        output,
        if mode == Mode::Main {
            tags(data)?
        } else {
            Vec::new()
        },
    ))
}

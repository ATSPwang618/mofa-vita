use super::{
    codec::{Mode, error},
    *,
};
use zune_jpeg::{
    JpegDecoder,
    zune_core::{bytestream::ZCursor, colorspace::ColorSpace, options::DecoderOptions},
};

fn decoder(data: &[u8], mode: Mode) -> Result<JpegDecoder<ZCursor<&[u8]>>> {
    let options = DecoderOptions::default()
        .set_max_width(65535)
        .set_max_height(65535)
        .jpeg_set_out_colorspace(if mode == Mode::Main {
            ColorSpace::RGBA
        } else {
            ColorSpace::Luma
        });
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    decoder.decode_headers().map_err(error)?;
    Ok(decoder)
}
pub fn probe(data: &[u8], budget: &Budget) -> Result<Size> {
    // APP metadata may be retained by the codec, but the encoded stream is
    // borrowed: probing and decoding do not clone the compressed file.
    let _header = reserve(budget, data.len().saturating_add(64 * 1024))?;
    let info = decoder(data, Mode::Main)?.info().unwrap();
    image_size(u32::from(info.width), u32::from(info.height))
}
pub fn decode(
    data: &[u8],
    size: Size,
    mode: Mode,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<(Bytes, Tags)> {
    if mode == Mode::Province {
        return Err(Error::Message("JPEG has no palette for a province image"));
    }
    let _header = reserve(budget, data.len().saturating_add(64 * 1024))?;
    let mut decoder = decoder(data, mode)?;
    // Allow up to four i16 coefficient planes, including padded MCU edges,
    // and row/upsampling storage. Progressive and multi-scan images need the
    // full coefficient planes; the output itself is charged separately.
    let scratch_bytes = scratch_bytes(size)?;
    let _scratch = reserve(budget, scratch_bytes)?;
    let mut output = Bytes::zeroed(
        decoder
            .output_buffer_size()
            .ok_or(Error::Message("JPEG output size overflow"))?,
        budget,
    )?;
    check(cancelled)?;
    decoder.decode_into(output.as_mut_slice()).map_err(error)?;
    check(cancelled)?;
    Ok((output, Vec::new()))
}
pub(crate) fn scratch_bytes(size: Size) -> Result<usize> {
    let padded_pixels =
        (size.width as usize).div_ceil(32) * 32 * (size.height as usize).div_ceil(32) * 32;
    padded_pixels
        .checked_mul(8)
        .and_then(|n| n.checked_add(size.width as usize * 256 + 64 * 1024))
        .ok_or(Error::Message("JPEG scratch size overflow"))
}

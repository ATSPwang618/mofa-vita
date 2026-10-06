use super::*;
use image::{ColorType, ImageDecoder};
use std::io::Cursor;
#[derive(Clone, Copy)]
pub enum Format {
    Png,
    Bmp,
    Jpeg,
    Webp,
    Tlg,
    Ktx,
    PackedBc,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Main,
    Mask,
    Province,
}
impl Format {
    pub fn detect(data: &[u8], name: &[u16]) -> Result<Self> {
        if data.starts_with(packed_bc::MAGIC) {
            return Ok(Self::PackedBc);
        }
        if data.starts_with(compressed::MAGIC) {
            return Ok(Self::Ktx);
        }
        // Converted game archives often keep the original TLG filenames for
        // PNG/WebP payloads. Select once from the bytes used for both probing
        // and decoding, including masks, provinces and transition rules.
        if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Ok(Self::Png);
        }
        if data.starts_with(b"BM") {
            return Ok(Self::Bmp);
        }
        if data.starts_with(b"\xff\xd8\xff") {
            return Ok(Self::Jpeg);
        }
        if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
            return Ok(Self::Webp);
        }
        if data.starts_with(b"TLG") {
            return Ok(Self::Tlg);
        }
        // Headerless DIB relies on the name. Unknown/truncated signatures keep
        // the named codec's diagnostic; decoding failures do not trigger retries.
        match String::from_utf16_lossy(krkr_assets::name::split_ext(name).1)
            .to_ascii_lowercase()
            .as_str()
        {
            ".png" => Ok(Self::Png),
            ".bmp" | ".dib" => Ok(Self::Bmp),
            ".jpg" | ".jpeg" | ".jif" => Ok(Self::Jpeg),
            ".tlg" | ".tlg5" | ".tlg6" => Ok(Self::Tlg),
            ".webp" => Ok(Self::Webp),
            ".ktx" => Ok(Self::Ktx),
            ".kbct" => Ok(Self::PackedBc),
            _ => Err(Error::Message("unsupported image format")),
        }
    }
}
pub fn probe(data: &[u8], format: Format, budget: &Budget) -> Result<Size> {
    match format {
        Format::Ktx => compressed::probe(data).map(|h| h.size),
        Format::PackedBc => packed_bc::probe(data).map(|h| h.size),
        Format::Png => png_image::probe(data),
        Format::Tlg => tlg::probe(data).map(|p| p.size),
        Format::Bmp => {
            let _scratch = reserve(budget, 16 * 1024)?;
            let decoder = bmp(data)?;
            let (w, h) = decoder.dimensions();
            image_size(w, h)
        }
        Format::Jpeg => jpeg::probe(data, budget),
        Format::Webp => {
            let _scratch = reserve(budget, data.len().saturating_add(64 * 1024))?;
            let decoder =
                image::codecs::webp::WebPDecoder::new(Cursor::new(data)).map_err(error)?;
            let (w, h) = decoder.dimensions();
            image_size(w, h)
        }
    }
}
fn bmp(data: &[u8]) -> Result<image::codecs::bmp::BmpDecoder<Cursor<&[u8]>>> {
    if data.starts_with(b"BM") {
        image::codecs::bmp::BmpDecoder::new(Cursor::new(data)).map_err(error)
    } else {
        image::codecs::bmp::BmpDecoder::new_without_file_header(Cursor::new(data)).map_err(error)
    }
}
pub fn decode(
    data: &[u8],
    format: Format,
    size: Size,
    mode: Mode,
    key: u32,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<(Bytes, Tags)> {
    match format {
        Format::PackedBc => Err(Error::Message("packed BC requires prepared BC blocks")),
        Format::Ktx => {
            if key & 0xff000000 == 0x03000000 {
                return Err(Error::Message(
                    "compressed texture has no color-key palette",
                ));
            }
            compressed::decode_container(data, mode, budget, cancelled)
        }
        Format::Png => png_image::decode(data, size, mode, key, budget, cancelled),
        Format::Tlg => tlg::decode(data, mode, budget, cancelled),
        Format::Bmp => {
            if let Some(pixels) = bmp32::decode(data, size, mode, budget, cancelled)? {
                return Ok((pixels, Vec::new()));
            }
            let _header = reserve(budget, 16 * 1024)?;
            let mut decoder = bmp(data)?;
            let palette = decoder.get_palette().map(|p| p.to_vec());
            let indexed =
                palette.is_some() && (mode == Mode::Province || key & 0xff000000 == 0x03000000);
            if mode == Mode::Province && !indexed {
                return Err(Error::Message("province BMP must be palettized"));
            }
            decoder.set_indexed_color(indexed);
            let result = decode_image(
                decoder,
                size,
                mode,
                if indexed { palette.as_deref() } else { None },
                key,
                budget,
                cancelled,
            )?;
            Ok((result, Vec::new()))
        }
        Format::Jpeg => jpeg::decode(data, size, mode, budget, cancelled),
        Format::Webp => {
            if mode == Mode::Province {
                return Err(Error::Message("WebP has no palette for a province image"));
            }
            let _scratch = reserve(
                budget,
                (size.width as usize)
                    .div_ceil(16)
                    .saturating_mul(16)
                    .saturating_mul((size.height as usize).div_ceil(16).saturating_mul(16))
                    .saturating_mul(8)
                    .saturating_add(64 * 1024)
                    .saturating_add(data.len()),
            )?;
            if mode == Mode::Mask {
                return webp_luma(data, size, budget, cancelled).map(|pixels| (pixels, Vec::new()));
            }
            let decoder =
                image::codecs::webp::WebPDecoder::new(Cursor::new(data)).map_err(error)?;
            decode_image(decoder, size, mode, None, key, budget, cancelled)
                .map(|pixels| (pixels, Vec::new()))
        }
    }
}
fn webp_luma(data: &[u8], size: Size, budget: &Budget, cancelled: &AtomicBool) -> Result<Bytes> {
    let mut at = 12usize;
    while let Some(header) = data.get(at..at.saturating_add(8)) {
        let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
        let start = at + 8;
        let end = start
            .checked_add(length)
            .ok_or(Error::Message("WebP chunk overflow"))?;
        let payload = data
            .get(start..end)
            .ok_or(Error::Message("truncated WebP chunk"))?;
        if &header[..4] == b"VP8 " {
            check(cancelled)?;
            let frame =
                image_webp::vp8::Vp8Decoder::decode_frame(Cursor::new(payload)).map_err(error)?;
            if u32::from(frame.width) != size.width || u32::from(frame.height) != size.height {
                return Err(Error::Message("WebP frame size mismatch"));
            }
            let mut out = Bytes::zeroed(size.width as usize * size.height as usize, budget)?;
            let stride = (size.width as usize).div_ceil(16) * 16;
            if frame.ybuf.len() < stride * size.height as usize {
                return Err(Error::Message("WebP luma size mismatch"));
            }
            for (src, dst) in frame
                .ybuf
                .chunks_exact(stride)
                .zip(out.as_mut_slice().chunks_exact_mut(size.width as usize))
            {
                dst.copy_from_slice(&src[..dst.len()]);
            }
            check(cancelled)?;
            return Ok(out);
        }
        at = end
            .checked_add(length & 1)
            .ok_or(Error::Message("WebP chunk overflow"))?;
    }
    // Lossless WebP's grayscale mode is libwebp's limited-range BT.601 Y,
    // not the engine's ordinary RGB luminance. See libwebp src/dsp/yuv.h.
    let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(data)).map_err(error)?;
    let rgba = decode_image(decoder, size, Mode::Main, None, 0, budget, cancelled)?;
    let mut out = Bytes::zeroed(size.width as usize * size.height as usize, budget)?;
    for (pixel, y) in rgba
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out.as_mut_slice())
    {
        *y = ((16839 * u32::from(pixel[0])
            + 33059 * u32::from(pixel[1])
            + 6420 * u32::from(pixel[2])
            + (1 << 15)
            + (16 << 16))
            >> 16) as u8;
    }
    Ok(out)
}
fn decode_image(
    mut decoder: impl ImageDecoder,
    size: Size,
    mode: Mode,
    palette: Option<&[[u8; 3]]>,
    key: u32,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    let color = decoder.color_type();
    let raw_bytes = usize::try_from(decoder.total_bytes())
        .map_err(|_| Error::Message("decoded image is too large"))?;
    let output_bytes = size
        .rgba_bytes()
        .ok_or(Error::Message("decoded image size overflow"))?
        / if mode == Mode::Main { 1 } else { 4 };
    // BMP needs palette and scanline storage, not full-image coefficients.
    let scratch_bytes = size.width as usize * 4 + 16 * 1024;
    let _scratch = reserve(budget, scratch_bytes)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(65535);
    limits.max_image_height = Some(65535);
    limits.max_alloc = Some((raw_bytes + scratch_bytes) as u64);
    decoder.set_limits(limits).map_err(error)?;
    let mut raw = Bytes::zeroed(raw_bytes, budget)?;
    check(cancelled)?;
    decoder.read_image(raw.as_mut_slice()).map_err(error)?;
    check(cancelled)?;
    if (mode == Mode::Main && color == ColorType::Rgba8 && palette.is_none())
        || (mode == Mode::Province && color == ColorType::L8)
    {
        return Ok(raw);
    }
    let mut output = Bytes::zeroed(output_bytes, budget)?;
    let channels = usize::from(color.channel_count());
    if !matches!(
        color,
        ColorType::L8 | ColorType::La8 | ColorType::Rgb8 | ColorType::Rgba8
    ) {
        return Err(Error::Message("unsupported decoded image color type"));
    }
    for (input_row, output_row) in raw
        .as_slice()
        .chunks_exact(size.width as usize * channels)
        .zip(
            output
                .as_mut_slice()
                .chunks_exact_mut(size.width as usize * if mode == Mode::Main { 4 } else { 1 }),
        )
    {
        check(cancelled)?;
        for (input, output) in input_row
            .chunks_exact(channels)
            .zip(output_row.chunks_exact_mut(if mode == Mode::Main { 4 } else { 1 }))
        {
            let p = if let Some(palette) = palette {
                let rgb = palette
                    .get(input[0] as usize)
                    .ok_or(Error::Message("palette index is out of range"))?;
                [
                    rgb[0],
                    rgb[1],
                    rgb[2],
                    if input[0] == key as u8 { 0 } else { 255 },
                ]
            } else {
                match color {
                    ColorType::L8 => [input[0], input[0], input[0], 255],
                    ColorType::La8 => [input[0], input[0], input[0], input[1]],
                    ColorType::Rgb8 => [input[0], input[1], input[2], 255],
                    _ => [input[0], input[1], input[2], input[3]],
                }
            };
            match mode {
                Mode::Main => output.copy_from_slice(&p),
                Mode::Mask => output[0] = transform::gray(p[0], p[1], p[2]),
                Mode::Province => output[0] = input[0],
            }
        }
    }
    Ok(output)
}
pub fn error(error: impl std::fmt::Display) -> Error {
    Error::Codec(error.to_string())
}

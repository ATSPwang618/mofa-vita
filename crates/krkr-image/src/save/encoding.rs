use super::*;
pub(super) fn png(output: &mut impl Write, rgba: &[u8], size: Size, alpha: bool) -> Result<()> {
    let mut encoder = ::png::Encoder::new(output, size.width, size.height);
    encoder.set_depth(::png::BitDepth::Eight);
    encoder.set_color(if alpha {
        ::png::ColorType::Rgba
    } else {
        ::png::ColorType::Rgb
    });
    let mut writer = encoder.write_header().map_err(error)?;
    writer
        .write_chunk(
            ::png::chunk::sBIT,
            if alpha { &[8, 8, 8, 8] } else { &[8, 8, 8] },
        )
        .map_err(error)?;
    {
        let mut stream = writer.stream_writer().map_err(error)?;
        let mut rgb = if alpha {
            Vec::new()
        } else {
            vec![0; size.width as usize * 3]
        };
        for row in rgba.chunks_exact(size.width as usize * 4) {
            if alpha {
                stream.write_all(row)?;
            } else {
                for (pixel, dest) in row
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(rgb.as_chunks_mut::<3>().0.iter_mut())
                {
                    dest.copy_from_slice(&pixel[..3]);
                }
                stream.write_all(&rgb)?;
            }
        }
        stream.finish().map_err(error)?;
    }
    writer.finish().map_err(error)
}
pub(super) fn jpeg(output: &mut impl Write, rgba: &[u8], size: Size, quality: u8) -> Result<()> {
    let mut encoder = jpeg_encoder::Encoder::new(output, quality);
    encoder.set_progressive(true);
    encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_2_0);
    encoder.set_chroma_subsampling_method(jpeg_encoder::ChromaSubsamplingMethod::Average);
    encoder
        .encode(
            rgba,
            size.width as u16,
            size.height as u16,
            jpeg_encoder::ColorType::Rgba,
        )
        .map_err(error)
}
pub(super) fn tlg(
    output: &mut (impl Write + Seek),
    main: Bytes,
    size: Size,
    six: bool,
    alpha: bool,
    tags: Tags,
) -> Result<()> {
    let (mut data, _input_permit) = main.into_parts();
    if !alpha {
        // Compact in place; the codec consumes the same allocation as readback.
        let pixels = data.len() / 4;
        for p in 0..pixels {
            data.copy_within(p * 4..p * 4 + 3, p * 3);
        }
        data.truncate(pixels * 3);
    }
    ::tlg::writer::TlgWriter::from_raw(
        data,
        tags.into_iter().collect(),
        size.width,
        size.height,
        if alpha {
            ::tlg::tlg_type::PixelLayout::Rgba
        } else {
            ::tlg::tlg_type::PixelLayout::Rgb
        },
        if six {
            ::tlg::tlg_type::TlgType::Tlg6
        } else {
            ::tlg::tlg_type::TlgType::Tlg5
        },
    )
    .write_to(output)
    .map_err(error)
}

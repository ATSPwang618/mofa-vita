use super::*;
use std::{hint::black_box, io::Write, time::Instant};
mod metadata;

fn chunk(output: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(tag);
    output.extend_from_slice(data);
    let mut crc = u32::MAX;
    for &byte in tag.iter().chain(data) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    output.extend_from_slice(&(!crc).to_be_bytes());
}

fn fixture(size: Size, color: png::ColorType, bits: u8, interlaced: bool) -> Vec<u8> {
    let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&size.width.to_be_bytes());
    header.extend_from_slice(&size.height.to_be_bytes());
    header.extend_from_slice(&[bits, color as u8, 0, 0, u8::from(interlaced)]);
    chunk(&mut output, b"IHDR", &header);
    if color == png::ColorType::Indexed {
        let count = 1usize << bits;
        let palette: Vec<_> = (0..count).flat_map(|i| rgb(i as u8)).collect();
        chunk(&mut output, b"PLTE", &palette);
        chunk(
            &mut output,
            b"tRNS",
            &(0..count).map(|i| i as u8).collect::<Vec<_>>(),
        );
    }
    chunk(&mut output, b"oFFs", &[255, 255, 255, 254, 0, 0, 0, 3, 0]);
    let passes: &[(u32, u32, u32, u32)] = if interlaced {
        &[
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
    } else {
        &[(0, 0, 1, 1)]
    };
    let mut raw = Vec::new();
    for &(left, top, dx, dy) in passes {
        if left >= size.width {
            continue;
        }
        for y in (top..size.height).step_by(dy as usize) {
            raw.push(0); // Independent no-filter scanlines, including Adam7 passes.
            let count = (size.width - left).div_ceil(dx) as usize;
            let mut row = vec![0; (count * color.samples() * bits as usize).div_ceil(8)];
            for (x_out, x) in (left..size.width).step_by(dx as usize).enumerate() {
                let value = value(x, y, bits);
                if bits < 8 {
                    row[x_out * bits as usize / 8] |=
                        value << (8 - bits as usize - x_out * bits as usize % 8);
                } else {
                    let values = samples(color, value);
                    for (channel, &sample) in values.iter().take(color.samples()).enumerate() {
                        let at = (x_out * color.samples() + channel) * (bits as usize / 8);
                        row[at] = sample;
                        if bits == 16 {
                            row[at + 1] = 255 - sample;
                        }
                    }
                }
            }
            raw.extend(row);
        }
    }
    let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zlib.write_all(&raw).unwrap();
    chunk(&mut output, b"IDAT", &zlib.finish().unwrap());
    chunk(&mut output, b"IEND", &[]);
    output
}
fn value(x: u32, y: u32, bits: u8) -> u8 {
    ((x * 37 + y * 71) & ((1 << bits.min(8)) - 1)) as u8
}
fn rgb(value: u8) -> [u8; 3] {
    [value.wrapping_mul(3), 255 - value, value ^ 0x69]
}
fn samples(color: png::ColorType, value: u8) -> [u8; 4] {
    let [r, g, b] = rgb(value);
    match color {
        png::ColorType::Grayscale | png::ColorType::Indexed => [value, 0, 0, 0],
        png::ColorType::GrayscaleAlpha => [value, 255 - value, 0, 0],
        png::ColorType::Rgb => [r, g, b, 255],
        png::ColorType::Rgba => [r, g, b, value],
    }
}
fn expected(size: Size, color: png::ColorType, bits: u8, mode: Mode, key: u32) -> Vec<u8> {
    let mut output = Vec::new();
    for y in 0..size.height {
        for x in 0..size.width {
            let value = value(x, y, bits);
            let gray = if bits < 8 {
                value * (255 / ((1u16 << bits) - 1)) as u8
            } else {
                value
            };
            let rgba = match color {
                png::ColorType::Grayscale => [gray, gray, gray, 255],
                png::ColorType::GrayscaleAlpha => [value, value, value, 255 - value],
                png::ColorType::Indexed => {
                    let [r, g, b] = rgb(value);
                    [
                        r,
                        g,
                        b,
                        if key >> 24 == 3 {
                            if value == 0 { key as u8 } else { 255 }
                        } else {
                            value
                        },
                    ]
                }
                _ => samples(color, value),
            };
            match mode {
                Mode::Main => output.extend(rgba),
                Mode::Mask => output.push(if color == png::ColorType::Indexed {
                    ((u32::from(rgba[0]) * 54 + u32::from(rgba[1]) * 183 + u32::from(rgba[2]) * 19)
                        >> 8) as u8
                } else {
                    rgba[2]
                }),
                Mode::Province => output.push(if color == png::ColorType::Indexed {
                    value
                } else {
                    gray
                }),
            }
        }
    }
    output
}

#[test]
fn png_formats_masks_indices_and_adam7_keep_exact_pixels_and_metadata() {
    let size = Size {
        width: 19,
        height: 11,
    };
    for color in [
        png::ColorType::Grayscale,
        png::ColorType::Indexed,
        png::ColorType::GrayscaleAlpha,
        png::ColorType::Rgb,
        png::ColorType::Rgba,
    ] {
        for bits in [1, 2, 4, 8, 16] {
            if (bits < 8 && !matches!(color, png::ColorType::Indexed | png::ColorType::Grayscale))
                || (bits == 16 && color == png::ColorType::Indexed)
            {
                continue;
            }
            for interlaced in [false, true] {
                let data = fixture(size, color, bits, interlaced);
                for mode in [Mode::Main, Mode::Mask, Mode::Province] {
                    if mode == Mode::Province
                        && (bits == 16
                            || !matches!(
                                color,
                                png::ColorType::Indexed | png::ColorType::Grayscale
                            ))
                    {
                        continue;
                    }
                    for key in [0x1fffffff, 0x0300006f] {
                        let budget = Budget::new(1024 * 1024);
                        let (pixels, tags) =
                            decode(&data, size, mode, key, &budget, &AtomicBool::new(false))
                                .unwrap();
                        assert_eq!(
                            pixels.as_slice(),
                            expected(size, color, bits, mode, key),
                            "{color:?}/{bits} interlaced={interlaced}"
                        );
                        if mode == Mode::Main {
                            assert_eq!(
                                tags,
                                [
                                    ("offs_x".into(), "-2".into()),
                                    ("offs_y".into(), "3".into()),
                                    ("offs_unit".into(), "pixel".into())
                                ]
                            );
                        } else {
                            assert!(tags.is_empty());
                        }
                        drop(pixels);
                        assert_eq!(budget.used(), 0);
                    }
                }
            }
        }
    }
}

#[test]
#[ignore = "manual PNG decode performance comparison"]
fn png_decode_benchmark() {
    let size = Size {
        width: 960,
        height: 544,
    };
    for (label, color, mode) in [
        ("rgb", png::ColorType::Rgb, Mode::Main),
        ("rgba", png::ColorType::Rgba, Mode::Main),
        ("gray-mask", png::ColorType::Grayscale, Mode::Mask),
        ("indexed-color", png::ColorType::Indexed, Mode::Main),
        ("indexed-mask", png::ColorType::Indexed, Mode::Mask),
        ("indexed-province", png::ColorType::Indexed, Mode::Province),
        ("rgba-mask", png::ColorType::Rgba, Mode::Mask),
    ] {
        let data = fixture(size, color, 8, false);
        let budget = Budget::new(32 * 1024 * 1024);
        let cancel = AtomicBool::new(false);
        for _ in 0..3 {
            black_box(decode(&data, size, mode, 0x1fffffff, &budget, &cancel).unwrap());
        }
        let start = Instant::now();
        for _ in 0..60 {
            black_box(decode(&data, size, mode, 0x1fffffff, &budget, &cancel).unwrap());
        }
        println!(
            "png {label}: {:.3} ms / 60 decodes",
            start.elapsed().as_secs_f64() * 1000.
        );
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn noninterlaced_pngs_fit_one_output_plane_plus_decoder_scratch() {
    let size = Size {
        width: 960,
        height: 544,
    };
    for (color, mode) in [
        (png::ColorType::Rgb, Mode::Main),
        (png::ColorType::Rgba, Mode::Main),
        (png::ColorType::Grayscale, Mode::Mask),
        (png::ColorType::Indexed, Mode::Main),
        (png::ColorType::Indexed, Mode::Mask),
        (png::ColorType::Indexed, Mode::Province),
        (png::ColorType::Rgba, Mode::Mask),
    ] {
        let data = fixture(size, color, 8, false);
        let length = size.rgba_bytes().unwrap() / if mode == Mode::Main { 1 } else { 4 };
        let limit = length + size.width as usize * 64 + 256 * 1024;
        let budget = Budget::new(limit);
        let (pixels, _) = decode(
            &data,
            size,
            mode,
            0x1fffffff,
            &budget,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            pixels.as_slice(),
            expected(size, color, 8, mode, 0x1fffffff)
        );
        assert_eq!(budget.used(), length);
        drop(pixels);
        assert_eq!(budget.used(), 0);
        let short = Budget::new(limit - 1);
        assert!(
            decode(
                &data,
                size,
                mode,
                0x1fffffff,
                &short,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(short.used(), 0);
        assert!(
            decode(
                &data,
                size,
                mode,
                0x1fffffff,
                &budget,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert_eq!(budget.used(), 0);
    }
}

fn replace_chunk(data: &[u8], name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut output = data[..8].to_vec();
    let mut offset = 8;
    while offset < data.len() {
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let tag = data[offset + 4..offset + 8].try_into().unwrap();
        chunk(
            &mut output,
            tag,
            if tag == name {
                payload
            } else {
                &data[offset + 8..offset + 8 + length]
            },
        );
        offset += 12 + length;
    }
    output
}

#[test]
fn png_short_palettes_packed_padding_and_corrupt_tails_still_validate() {
    let budget = Budget::new(1024 * 1024);
    let size = Size {
        width: 19,
        height: 11,
    };
    for bits in [1, 2, 4, 8] {
        for interlaced in [false, true] {
            let data = fixture(size, png::ColorType::Indexed, bits, interlaced);
            let data = replace_chunk(&data, b"PLTE", &[0, 0, 0]);
            let data = replace_chunk(&data, b"tRNS", &[255]);
            for mode in [Mode::Main, Mode::Mask, Mode::Province] {
                let error = match decode(
                    &data,
                    size,
                    mode,
                    0x1fffffff,
                    &budget,
                    &AtomicBool::new(false),
                ) {
                    Err(error) => error,
                    Ok(_) => panic!("invalid palette index was accepted"),
                };
                assert!(
                    error.to_string().contains("PNG palette index out of range"),
                    "{error}"
                );
                assert_eq!(budget.used(), 0);
            }
        }
    }
    // Unused low bits are not palette samples. A one-pixel, one-color image
    // must stay valid even when its byte has nonzero padding bits.
    let one = Size {
        width: 1,
        height: 1,
    };
    for bits in [1, 2, 4] {
        let data = fixture(one, png::ColorType::Indexed, bits, false);
        let data = replace_chunk(&data, b"PLTE", &[11, 22, 33]);
        let data = replace_chunk(&data, b"tRNS", &[77]);
        let mut zlib = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        zlib.write_all(&[0, (1 << (8 - bits)) - 1]).unwrap();
        let data = replace_chunk(&data, b"IDAT", &zlib.finish().unwrap());
        for mode in [Mode::Main, Mode::Mask, Mode::Province] {
            let (pixels, _) = decode(
                &data,
                one,
                mode,
                0x1fffffff,
                &budget,
                &AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(
                pixels.as_slice(),
                match mode {
                    Mode::Main => &[11, 22, 33, 77][..],
                    Mode::Mask => &[20][..],
                    Mode::Province => &[0][..],
                }
            );
        }
        assert_eq!(budget.used(), 0);
    }
    for color in [
        png::ColorType::Rgb,
        png::ColorType::Rgba,
        png::ColorType::Indexed,
    ] {
        let data = fixture(size, color, 8, false);
        let mut crc = data.clone();
        *crc.last_mut().unwrap() ^= 1;
        for bad in [&data[..data.len() - 8], &data[..data.len() / 2], &crc] {
            assert!(
                decode(
                    bad,
                    size,
                    Mode::Main,
                    0x1fffffff,
                    &budget,
                    &AtomicBool::new(false)
                )
                .is_err()
            );
            assert_eq!(budget.used(), 0);
        }
    }
}

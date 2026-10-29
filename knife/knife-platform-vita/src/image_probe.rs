//! Image-only checks that the GXM probe can call without initializing graphics.
use crate::image::{DecodedImage, ImageDecodeError};
use std::io::Write;

const PNG: &[u8] = include_bytes!("image_fixtures/alpha.png");
const JPEG: &[u8] = include_bytes!("image_fixtures/colors.jpg");
const RGBA: &[u8] = &[
    255, 17, 0, 0, 0, 255, 31, 64, 7, 11, 255, 255, 89, 45, 12, 127,
];

fn check_png() -> Result<(), String> {
    let image = DecodedImage::decode(PNG).map_err(|e| e.to_string())?;
    if (
        image.width(),
        image.height(),
        image.stride(),
        image.byte_len(),
    ) != (2, 2, 8, 16)
        || image.pixels() != RGBA
    {
        return Err("PNG straight-alpha pixel/shape mismatch".into());
    }
    // The owner releases its C allocation on every return path.
    Ok(())
}

fn check_jpeg() -> Result<(), String> {
    let image = DecodedImage::decode(JPEG).map_err(|e| e.to_string())?;
    if (
        image.width(),
        image.height(),
        image.stride(),
        image.byte_len(),
    ) != (16, 8, 64, 512)
    {
        return Err("JPEG RGBA8 shape mismatch".into());
    }
    for (index, pixel) in image.pixels().chunks_exact(4).enumerate() {
        let expected = if index % 16 < 8 {
            [240_u8, 20, 10]
        } else {
            [15, 225, 35]
        };
        if (0..3).any(|channel| pixel[channel].abs_diff(expected[channel]) > 3) || pixel[3] != 255 {
            return Err(format!(
                "JPEG RGB/opaque-alpha mismatch at pixel {index}: {pixel:?}"
            ));
        }
    }
    Ok(())
}

/// Runs only decoder/ownership checks. The caller owns the log and any GXM state.
/// This function needs no files, network, display, GPU textures or game assets.
pub fn run_image_probe(log: &mut (impl Write + ?Sized)) -> Result<(), String> {
    macro_rules! record {
        ($($arg:tt)*) => {{
            writeln!(log, $($arg)*).map_err(|e| e.to_string())?;
            log.flush().map_err(|e| e.to_string())?;
        }};
    }
    record!(
        "PNG/JPEG native decoder probe; {} embedded original fixture bytes; pointer width {}",
        PNG.len() + JPEG.len(),
        usize::BITS
    );
    check_png()?;
    record!("PASS PNG straight-alpha: 2x2 RGBA8, exact colors including alpha 0/64/127/255");
    check_jpeg()?;
    record!("PASS JPEG RGB: 16x8 RGBA8, two color blocks, opaque alpha, tolerance 3/255");
    if !matches!(DecodedImage::decode(&[]), Err(ImageDecodeError::EmptyInput))
        || !matches!(
            DecodedImage::decode(b"bad header"),
            Err(ImageDecodeError::Native(-3))
        )
    {
        return Err("invalid image header/empty input accepted or misclassified".into());
    }
    for encoded in [PNG, JPEG] {
        for length in [2, 8, encoded.len() / 2, encoded.len() - 1] {
            if DecodedImage::decode(&encoded[..length]).is_ok() {
                return Err(format!(
                    "truncated image accepted at {length}/{} bytes",
                    encoded.len()
                ));
            }
        }
    }
    record!(
        "PASS bad headers and truncated PNG/JPEG prefixes; failure output ownership retained by RAII"
    );
    for _ in 0..32 {
        check_png()?;
        check_jpeg()?;
    }
    record!(
        "PASS 32 PNG/JPEG lifetimes: decode, pixel validation, C-owned RAII drop on each iteration"
    );
    Ok(())
}

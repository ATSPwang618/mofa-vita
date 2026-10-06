//! Stock BMP32 uses a 40-byte BI_RGB header while retaining alpha. Generic BMP
//! encoders choose a different header; BMP8 also uses TVP's fixed 252 colors.
use super::*;
#[cfg(test)]
#[path = "../../tests/internal/bitmap.rs"]
mod tests;
pub(super) fn encode(
    output: &mut impl Write,
    rgba: &[u8],
    size: Size,
    depth: BitmapDepth,
) -> Result<()> {
    let bpp = match depth {
        BitmapDepth::Indexed => 8,
        BitmapDepth::Rgb => 24,
        BitmapDepth::Rgba => 32,
    };
    let channels = usize::from(bpp / 8);
    let stride = (size.width as usize * channels).div_ceil(4) * 4;
    let offset = if bpp == 8 { 54 + 1024 } else { 54 };
    let length = u32::try_from(stride as u64 * u64::from(size.height) + offset as u64)
        .map_err(|_| Error::Message("BMP file is too large"))?;
    let mut header = [0u8; 54];
    header[..2].copy_from_slice(b"BM");
    header[2..6].copy_from_slice(&length.to_le_bytes());
    header[10..14].copy_from_slice(&(offset as u32).to_le_bytes());
    header[14..18].copy_from_slice(&40u32.to_le_bytes());
    header[18..22].copy_from_slice(&size.width.to_le_bytes());
    header[22..26].copy_from_slice(&size.height.to_le_bytes());
    header[26] = 1;
    header[28] = bpp;
    output.write_all(&header)?;
    if bpp == 8 {
        let mut palette = [0u8; 1024];
        for (i, p) in palette
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .take(252)
            .enumerate()
        {
            // TVP stores this table in BMP's BGR order: blue is the slow axis.
            p[0] = (i / 42 * 255 / 5) as u8;
            p[1] = (i / 6 % 7 * 255 / 6) as u8;
            p[2] = (i % 6 * 255 / 5) as u8;
        }
        output.write_all(&palette)?;
    }
    let mut row = vec![0; stride];
    match depth {
        BitmapDepth::Indexed => rows::<_, 1>(output, rgba, size.width as usize, &mut row),
        BitmapDepth::Rgb => rows::<_, 3>(output, rgba, size.width as usize, &mut row),
        BitmapDepth::Rgba => rows::<_, 4>(output, rgba, size.width as usize, &mut row),
    }
}
// Specialize the complete row loop: indexed quantization must not inhibit
// vectorization of the RGB/RGBA channel shuffle.
fn rows<W: Write, const CHANNELS: usize>(
    output: &mut W,
    rgba: &[u8],
    width: usize,
    row: &mut [u8],
) -> Result<()> {
    for (y, source) in rgba.chunks_exact(width * 4).enumerate().rev() {
        for (x, (p, out)) in source
            .as_chunks::<4>()
            .0
            .iter()
            .zip(row.as_chunks_mut::<CHANNELS>().0)
            .enumerate()
        {
            if CHANNELS == 1 {
                out[0] = index(p, x, y);
            } else {
                out[..3].copy_from_slice(&[p[2], p[1], p[0]]);
                if CHANNELS == 4 {
                    out[3] = p[3];
                }
            }
        }
        output.write_all(row)?;
    }
    Ok(())
}
fn index(p: &[u8], x: usize, y: usize) -> u8 {
    const DITHER: [[usize; 4]; 4] = [[0, 12, 2, 14], [8, 4, 10, 6], [3, 15, 1, 13], [11, 7, 9, 5]];
    static FIVE: [[u8; 256]; 16] = levels(5);
    static SIX: [[u8; 256]; 16] = levels(6);
    let (x, y) = (x & 3, y & 3);
    // Preserve the stock table's [channel][y][x] lookup and asymmetric phase.
    FIVE[DITHER[(x + 1) % 2][(y + 1) % 2]][usize::from(p[0])]
        + SIX[DITHER[x][(y + 1) % 2]][usize::from(p[1])] * 6
        + FIVE[DITHER[x][y]][usize::from(p[2])] * 42
}

// Two 4 KiB tables replace three quantizations per indexed pixel. They are
// shared by all encoders and require no allocation or runtime initialization.
const fn levels(count: u16) -> [[u8; 256]; 16] {
    let mut table = [[0; 256]; 16];
    let mut threshold = 0;
    while threshold < 16 {
        let mut channel = 0;
        while channel < 256 {
            let scaled = channel as u16 * count;
            table[threshold][channel] =
                (scaled / 255 + (scaled % 255 * 16 > threshold as u16 * 255) as u16) as u8;
            channel += 1;
        }
        threshold += 1;
    }
    table
}

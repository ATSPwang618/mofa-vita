//! Reference BC1 RGB / BC3 decoder for CPU image operations and quality checks.
//! Native Vita blocks are addressed directly, without a deswizzled copy.
use super::*;

fn color(word: u16) -> [u8; 3] {
    let r = (word >> 11) as u8;
    let g = ((word >> 5) & 63) as u8;
    let b = (word & 31) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

pub(super) fn decode(
    data: &[u8],
    size: Size,
    format: Format,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    if format.byte_len(size) != Some(data.len()) {
        return Err(Error::Message("BC payload size mismatch"));
    }
    check(cancelled)?;
    let mut out = Bytes::zeroed(
        size.rgba_bytes()
            .ok_or(Error::Message("BC image too large"))?,
        budget,
    )?;
    let alpha = format.linear() == Format::Bc3Rgba;
    let block_bytes = if alpha { 16 } else { 8 };
    let (width, height) = (size.width.div_ceil(4), size.height.div_ceil(4));
    let native_masks = format.is_vita().then(|| {
        (
            krkr_protocol::texture::vita_block_index(width - 1, 0, width, height),
            krkr_protocol::texture::vita_block_index(0, height - 1, width, height),
        )
    });
    let mut native_row = 0usize;
    for y in 0..height {
        check(cancelled)?;
        let mut native_column = 0usize;
        for x in 0..width {
            let index = if let Some((x_mask, _)) = native_masks {
                // Advance only this axis's Morton bits, including the tail of
                // rectangular grids. No per-block bit loop or address table.
                let index = native_row | native_column;
                native_column = native_column.wrapping_sub(x_mask) & x_mask;
                index
            } else {
                (y * width + x) as usize
            };
            let block = &data[index * block_bytes..(index + 1) * block_bytes];
            let rgb = &block[if alpha { 8 } else { 0 }..];
            let c0 = u16::from_le_bytes(rgb[..2].try_into().unwrap());
            let c1 = u16::from_le_bytes(rgb[2..4].try_into().unwrap());
            let mut colors = [color(c0), color(c1), [0; 3], [0; 3]];
            let [first, second, third, fourth] = &mut colors;
            for (((a, b), c), d) in first.iter().zip(second.iter()).zip(third).zip(fourth) {
                let (a, b) = (u16::from(*a), u16::from(*b));
                if alpha || c0 > c1 {
                    *c = ((2 * a + b) / 3) as u8;
                    *d = ((a + 2 * b) / 3) as u8;
                } else {
                    *c = ((a + b) / 2) as u8;
                }
            }
            let indices = u32::from_le_bytes(rgb[4..8].try_into().unwrap());
            let mut alphas = [255; 8];
            let mut alpha_bits = 0u64;
            if alpha {
                let (a, b) = (u16::from(block[0]), u16::from(block[1]));
                alphas[0] = a as u8;
                alphas[1] = b as u8;
                let steps = if a > b { 7 } else { 5 };
                for i in 1..steps {
                    alphas[(i + 1) as usize] = (((steps - i) * a + i * b) / steps) as u8;
                }
                if a <= b {
                    alphas[6] = 0;
                    alphas[7] = 255;
                }
                for (i, &byte) in block[2..8].iter().enumerate() {
                    alpha_bits |= u64::from(byte) << (8 * i);
                }
            }
            for py in 0..4 {
                for px in 0..4 {
                    let (dx, dy) = (x * 4 + px, y * 4 + py);
                    if dx >= size.width || dy >= size.height {
                        continue;
                    }
                    let bit = py * 4 + px;
                    let at = (dy as usize * size.width as usize + dx as usize) * 4;
                    let pixel = &mut out.as_mut_slice()[at..at + 4];
                    pixel[..3].copy_from_slice(&colors[((indices >> (2 * bit)) & 3) as usize]);
                    pixel[3] = alphas[((alpha_bits >> (3 * bit)) & 7) as usize];
                }
            }
        }
        if let Some((_, y_mask)) = native_masks {
            native_row = native_row.wrapping_sub(y_mask) & y_mask;
        }
    }
    Ok(out)
}

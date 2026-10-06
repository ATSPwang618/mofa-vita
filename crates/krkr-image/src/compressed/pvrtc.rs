//! PVRTC-I 4bpp fallback. Endpoint grids interpolate across block boundaries;
//! modulation remains local to each 4x4 word. No RGBA copy is kept on the GPU path.
use super::*;

fn morton(x: u32, y: u32, width: u32, height: u32) -> usize {
    let bits = width.min(height).trailing_zeros();
    let mut index = 0;
    for bit in 0..bits {
        index |= ((y >> bit) & 1) << (bit * 2);
        index |= ((x >> bit) & 1) << (bit * 2 + 1);
    }
    index |= (if width > height { x } else { y } >> bits) << (bits * 2);
    index as usize
}

// Endpoints interpolate in RGB555 / A4 precision. Endpoint A sacrifices its
// low blue bit for the modulation mode; translucent endpoints use RGB443/444.
fn endpoint(color: u16, a: bool) -> [u32; 4] {
    let color = u32::from(color);
    if color & 0x8000 != 0 {
        let blue = if a {
            ((color >> 1) & 15) * 2 + ((color >> 4) & 1)
        } else {
            color & 31
        };
        [(color >> 10) & 31, (color >> 5) & 31, blue, 15]
    } else {
        let r = (color >> 8) & 15;
        let g = (color >> 4) & 15;
        let b = if a {
            ((color >> 1) & 7) * 4 + ((color >> 2) & 3)
        } else {
            (color & 15) * 2 + ((color >> 3) & 1)
        };
        [r * 2 + (r >> 3), g * 2 + (g >> 3), b, (color >> 11) & 14]
    }
}

pub(super) fn decode(
    data: &[u8],
    size: Size,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    if Format::Pvrtc1Rgba4.byte_len(size) != Some(data.len()) {
        return Err(Error::Message("invalid PVRTC1 RGBA4 layout or payload"));
    }
    check(cancelled)?;
    let mut output = Bytes::zeroed(
        size.rgba_bytes()
            .ok_or(Error::Message("PVRTC size overflow"))?,
        budget,
    )?;
    let (width, height) = (size.width / 4, size.height / 4);
    let word = |x: u32, y: u32| {
        let at = morton(x & (width - 1), y & (height - 1), width, height) * 8;
        (
            u32::from_le_bytes(data[at..at + 4].try_into().unwrap()),
            u32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()),
        )
    };
    for y in 0..size.height {
        check(cancelled)?;
        for x in 0..size.width {
            let gx = x.wrapping_sub(2);
            let gy = y.wrapping_sub(2);
            let (bx, by, fx, fy) = (gx / 4, gy / 4, gx & 3, gy & 3);
            let colors = [
                word(bx, by).1,
                word(bx + 1, by).1,
                word(bx, by + 1).1,
                word(bx + 1, by + 1).1,
            ];
            let weights = [(4 - fx) * (4 - fy), fx * (4 - fy), (4 - fx) * fy, fx * fy];
            let mut interpolated = [[0; 4]; 2];
            for (i, values) in interpolated.iter_mut().enumerate() {
                for (color, weight) in colors.into_iter().zip(weights) {
                    let end = endpoint((color >> (i * 16)) as u16, i == 0);
                    for c in 0..4 {
                        values[c] += end[c] * weight;
                    }
                }
                for value in &mut values[..3] {
                    *value = (*value >> 1) + (*value >> 6);
                }
                values[3] += values[3] >> 4;
            }
            let (modulation, color) = word(x / 4, y / 4);
            let code = ((modulation >> (2 * ((y & 3) * 4 + (x & 3)))) & 3) as usize;
            let punch = color & 1 != 0;
            let weight = if punch {
                [0, 4, 4, 8][code]
            } else {
                [0, 3, 5, 8][code]
            };
            let at = ((y * size.width + x) * 4) as usize;
            for (c, pixel) in output.as_mut_slice()[at..at + 4].iter_mut().enumerate() {
                *pixel = if c == 3 && punch && code == 2 {
                    0
                } else {
                    ((interpolated[0][c] * (8 - weight) + interpolated[1][c] * weight) / 8) as u8
                };
            }
        }
    }
    Ok(output)
}

use super::tables;
use crate::{Error, Result};

pub(super) struct Bits<'a> {
    bytes: &'a [u8],
    pos: usize,
}
impl<'a> Bits<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }
    fn read(&mut self, count: usize) -> Result<i32> {
        let mut value = 0;
        for _ in 0..count {
            let byte = self
                .bytes
                .get(self.pos / 8)
                .ok_or(Error::Message("truncated AMV Huffman stream"))?;
            value = (value << 1) | i32::from((byte >> (7 - self.pos % 8)) & 1);
            self.pos += 1;
        }
        Ok(value)
    }
    fn symbol(&mut self, table: &([u8; 16], &[u8])) -> Result<u8> {
        let mut code = 0;
        let mut first = 0;
        let mut offset = 0;
        for &count in &table.0 {
            code = (code << 1) | self.read(1)?;
            if code < first + i32::from(count) {
                return table
                    .1
                    .get(offset + (code - first) as usize)
                    .copied()
                    .ok_or(Error::Message("invalid AMV Huffman symbol"));
            }
            offset += count as usize;
            first = (first + i32::from(count)) << 1;
        }
        Err(Error::Message("invalid AMV Huffman code"))
    }
    fn amplitude(&mut self, width: usize) -> Result<i16> {
        if width == 0 {
            return Ok(0);
        }
        let value = self.read(width)?;
        Ok(if value < 1 << (width - 1) {
            value - ((1 << width) - 1)
        } else {
            value
        } as i16)
    }
    pub fn block(&mut self, chroma: bool, last: &mut i32, quant: &[u8; 64]) -> Result<[u8; 64]> {
        let dc = if chroma {
            &tables::CHROMA_DC
        } else {
            &tables::LUMA_DC
        };
        let ac = if chroma {
            &tables::CHROMA_AC
        } else {
            &tables::LUMA_AC
        };
        let width = self.symbol(dc)? as usize;
        *last = last.wrapping_add(i32::from(self.amplitude(width)?));
        let mut coefficients = [0i16; 64];
        coefficients[0] = *last as i16;
        let mut index = 1;
        while index < 64 {
            let symbol = self.symbol(ac)?;
            let run = (symbol >> 4) as usize;
            let width = (symbol & 15) as usize;
            if width == 0 {
                if run != 15 {
                    break;
                }
                index += 16;
                continue;
            }
            index += run;
            if index >= 64 {
                return Err(Error::Message("AMV coefficient run overflow"));
            }
            coefficients[ZIGZAG[index]] = self.amplitude(width)?;
            index += 1;
        }
        // Preserve the reference's separate, truncating DC-only path.
        if index == 1 {
            let dc = i32::from(coefficients[0]) * i32::from(quant[0]);
            return Ok([(dc / 8 + 128).clamp(0, 255) as u8; 64]);
        }
        Ok(idct(&coefficients, quant))
    }
}
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

// IJG integer IDCT, CONST_BITS=13 and PASS1_BITS=1, as in AlphaMovie.
// Widen intermediates to avoid platform-dependent C long overflow.
fn transform(x: [i64; 8]) -> [i64; 8] {
    let z1 = (x[2] + x[6]) * 4433;
    let even2 = z1 - x[6] * 15137;
    let even3 = z1 + x[2] * 6270;
    let even0 = (x[0] + x[4]) << 13;
    let even1 = (x[0] - x[4]) << 13;
    let a = even0 + even3;
    let b = even1 + even2;
    let c = even1 - even2;
    let d = even0 - even3;
    let z1 = -(x[7] + x[1]) * 7373;
    let z2 = -(x[5] + x[3]) * 20995;
    let z5 = (x[7] + x[3] + x[5] + x[1]) * 9633;
    let z3 = -(x[7] + x[3]) * 16069 + z5;
    let z4 = -(x[5] + x[1]) * 3196 + z5;
    let p = x[7] * 2446 + z1 + z3;
    let q = x[5] * 16819 + z2 + z4;
    let r = x[3] * 25172 + z2 + z3;
    let s = x[1] * 12299 + z1 + z4;
    [a + s, b + r, c + q, d + p, d - p, c - q, b - r, a - s]
}
fn range(value: i64) -> u8 {
    match value & 1023 {
        n @ 0..=127 => (n + 128) as u8,
        128..=511 => 255,
        512..=895 => 0,
        n => (n - 896) as u8,
    }
}
fn idct(coefficients: &[i16; 64], quant: &[u8; 64]) -> [u8; 64] {
    let mut workspace = [0i64; 64];
    for column in 0..8 {
        let input = std::array::from_fn(|row| {
            i64::from(coefficients[row * 8 + column]) * i64::from(quant[row * 8 + column])
        });
        let output = if input[1..].iter().all(|&n| n == 0) {
            [input[0] << 1; 8]
        } else {
            transform(input).map(|n| (n + (1 << 11)) >> 12)
        };
        for row in 0..8 {
            workspace[row * 8 + column] = output[row];
        }
    }
    let mut pixels = [0; 64];
    for row in 0..8 {
        let input = std::array::from_fn(|column| workspace[row * 8 + column]);
        let output = if input[1..].iter().all(|&n| n == 0) {
            [range((input[0] + 8) >> 4); 8]
        } else {
            transform(input).map(|n| range((n + (1 << 16)) >> 17))
        };
        pixels[row * 8..row * 8 + 8].copy_from_slice(&output);
    }
    pixels
}

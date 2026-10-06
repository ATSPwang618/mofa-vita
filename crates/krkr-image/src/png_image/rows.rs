//! Select a scanline conversion once, keeping format tests out of pixel loops.
use super::*;
use png::{BitDepth, ColorType};

pub(super) struct Rows {
    color: ColorType,
    bits: usize,
    mode: Mode,
    palette: [[u8; 4]; 256],
    gray: [u8; 256],
    colors: usize,
}
impl Rows {
    pub fn new(
        info: &png::Info<'_>,
        color: ColorType,
        depth: BitDepth,
        mode: Mode,
        key: u32,
    ) -> Result<Self> {
        let mut rows = Self {
            color,
            bits: depth as usize,
            mode,
            palette: [[0; 4]; 256],
            gray: [0; 256],
            colors: 0,
        };
        if color == ColorType::Indexed {
            let palette = info
                .palette
                .as_deref()
                .ok_or(Error::Message("PNG has no palette"))?;
            rows.colors = (palette.len() / 3).min(256);
            for (i, rgb) in palette
                .as_chunks::<3>()
                .0
                .iter()
                .take(rows.colors)
                .enumerate()
            {
                // Legacy LoadPNG replaces tRNS with a single keyidx byte.
                let alpha = if key & 0xff000000 == 0x03000000 {
                    if i == 0 { key as u8 } else { 255 }
                } else {
                    info.trns
                        .as_deref()
                        .and_then(|t| t.get(i))
                        .copied()
                        .unwrap_or(255)
                };
                rows.palette[i] = [rgb[0], rgb[1], rgb[2], alpha];
                rows.gray[i] = transform::gray(rgb[0], rgb[1], rgb[2]);
            }
        }
        Ok(rows)
    }
    pub fn direct(&self) -> bool {
        self.bits == 8
            && matches!(
                (self.mode, self.color),
                (Mode::Main, ColorType::Rgba)
                    | (Mode::Mask | Mode::Province, ColorType::Grayscale)
                    | (Mode::Province, ColorType::Indexed)
            )
    }
    pub fn validate(&self, data: &[u8]) -> Result<()> {
        if self.color == ColorType::Indexed
            && self.colors < 256
            && data.iter().any(|&i| i as usize >= self.colors)
        {
            return Err(Error::Message("PNG palette index out of range"));
        }
        Ok(())
    }
    fn indices(
        &self,
        row: &[u8],
        count: usize,
        mut apply: impl FnMut(u8) -> Result<()>,
    ) -> Result<()> {
        if self.bits == 8 {
            for &value in &row[..count] {
                apply(value)?;
            }
        } else {
            let mask = (1 << self.bits) - 1;
            let mut remaining = count;
            for &byte in row {
                let units = remaining.min(8 / self.bits);
                let mut shift = 8;
                for _ in 0..units {
                    shift -= self.bits;
                    apply((byte >> shift) & mask)?;
                }
                remaining -= units;
                if remaining == 0 {
                    break;
                }
            }
        }
        Ok(())
    }
    pub fn convert(&self, row: &[u8], out: &mut [u8]) -> Result<()> {
        let channels = if self.mode == Mode::Main { 4 } else { 1 };
        let count = out.len() / channels;
        if row.len() != (count * self.color.samples() * self.bits).div_ceil(8) {
            return Err(Error::Message("PNG row size mismatch"));
        }
        match (self.mode, self.color) {
            (Mode::Main, ColorType::Indexed) => {
                let mut pixels = out.as_chunks_mut::<4>().0.iter_mut();
                self.indices(row, count, |index| {
                    *pixels.next().unwrap() = *self.palette[..self.colors]
                        .get(index as usize)
                        .ok_or(Error::Message("PNG palette index out of range"))?;
                    Ok(())
                })?;
            }
            (Mode::Mask | Mode::Province, ColorType::Indexed) => {
                let mut pixels = out.iter_mut();
                if self.mode == Mode::Province {
                    self.indices(row, count, |index| {
                        if index as usize >= self.colors {
                            return Err(Error::Message("PNG palette index out of range"));
                        }
                        *pixels.next().unwrap() = index;
                        Ok(())
                    })?;
                } else {
                    self.indices(row, count, |index| {
                        *pixels.next().unwrap() = *self.gray[..self.colors]
                            .get(index as usize)
                            .ok_or(Error::Message("PNG palette index out of range"))?;
                        Ok(())
                    })?;
                }
            }
            (mode, ColorType::Grayscale) => {
                let factor = (255 / ((1u16 << self.bits) - 1)) as u8;
                let mut pixels = out.chunks_exact_mut(channels);
                self.indices(row, count, |value| {
                    let gray = value * factor;
                    let pixel = pixels.next().unwrap();
                    if mode == Mode::Main {
                        pixel.copy_from_slice(&[gray, gray, gray, 255]);
                    } else {
                        pixel[0] = gray;
                    }
                    Ok(())
                })?;
            }
            (Mode::Main, ColorType::Rgb) => {
                for (source, target) in row
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .zip(out.as_chunks_mut::<4>().0)
                {
                    *target = [source[0], source[1], source[2], 255];
                }
            }
            (Mode::Main, ColorType::GrayscaleAlpha) => {
                for (source, target) in row
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .zip(out.as_chunks_mut::<4>().0)
                {
                    *target = [source[0], source[0], source[0], source[1]];
                }
            }
            (Mode::Main, ColorType::Rgba) => out.copy_from_slice(row),
            // Legacy truecolor masks use blue; palette masks use luminance.
            (Mode::Mask, ColorType::Rgb) => {
                for (source, target) in row.as_chunks::<3>().0.iter().zip(out) {
                    *target = source[2];
                }
            }
            (Mode::Mask, ColorType::Rgba) => {
                for (source, target) in row.as_chunks::<4>().0.iter().zip(out) {
                    *target = source[2];
                }
            }
            (Mode::Mask, ColorType::GrayscaleAlpha) => {
                for (source, target) in row.as_chunks::<2>().0.iter().zip(out) {
                    *target = source[0];
                }
            }
            (Mode::Province, _) => {
                return Err(Error::Message(
                    "province PNG must be palettized or grayscale",
                ));
            }
        }
        Ok(())
    }
}

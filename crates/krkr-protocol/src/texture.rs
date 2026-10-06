//! Owned, budgeted GPU blocks. Container parsing and software decoding belong
//! to the image service; backends choose native sampling or an RGBA fallback.
use crate::{graphics::Size, pixels::Bytes};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Etc1,
    Pvrtc1Rgba4,
    Bc1Rgb,
    Bc3Rgba,
    /// BC blocks in SGX543 Y-first Morton order, not standard S3TC byte order.
    Bc1RgbVita,
    Bc3RgbaVita,
}
impl Format {
    pub fn gl_internal(self) -> u32 {
        match self {
            Self::Etc1 => 0x8d64,
            Self::Pvrtc1Rgba4 => 0x8c02,
            Self::Bc1Rgb => 0x83f0,
            Self::Bc3Rgba => 0x83f3,
            // Private GL_KRKR_texture_compression_bc formats. Never submit
            // native block order under a standard S3TC format enum.
            Self::Bc1RgbVita => 0x6000_0001,
            Self::Bc3RgbaVita => 0x6000_0002,
        }
    }
    pub fn opaque(self) -> bool {
        matches!(self, Self::Etc1 | Self::Bc1Rgb | Self::Bc1RgbVita)
    }
    pub fn is_vita(self) -> bool {
        matches!(self, Self::Bc1RgbVita | Self::Bc3RgbaVita)
    }
    pub fn linear(self) -> Self {
        match self {
            Self::Bc1RgbVita => Self::Bc1Rgb,
            Self::Bc3RgbaVita => Self::Bc3Rgba,
            other => other,
        }
    }
    pub fn vita(self) -> Option<Self> {
        match self {
            Self::Bc1Rgb | Self::Bc1RgbVita => Some(Self::Bc1RgbVita),
            Self::Bc3Rgba | Self::Bc3RgbaVita => Some(Self::Bc3RgbaVita),
            _ => None,
        }
    }
    pub fn byte_len(self, size: Size) -> Option<usize> {
        if size.width == 0 || size.height == 0 || size.width > 65535 || size.height > 65535 {
            return None;
        }
        if self.is_vita()
            && (size.width < 8
                || size.height < 8
                || !size.width.is_power_of_two()
                || !size.height.is_power_of_two())
        {
            return None;
        }
        match self {
            Self::Etc1 | Self::Bc1Rgb | Self::Bc3Rgba | Self::Bc1RgbVita | Self::Bc3RgbaVita => {
                (size.width as usize)
                    .div_ceil(4)
                    .checked_mul((size.height as usize).div_ceil(4))?
                    .checked_mul(if self.linear() == Self::Bc3Rgba {
                        16
                    } else {
                        8
                    })
            }
            Self::Pvrtc1Rgba4 => {
                if size.width < 8
                    || size.height < 8
                    || !size.width.is_power_of_two()
                    || !size.height.is_power_of_two()
                {
                    return None;
                }
                (size.width as usize)
                    .checked_mul(size.height as usize)
                    .map(|n| n / 2)
            }
        }
    }
}

/// SGX block address for a POT block grid. Y supplies the least significant
/// bit; once the shorter axis is exhausted, append the longer axis's bits.
pub fn vita_block_index(x: u32, y: u32, width: u32, height: u32) -> usize {
    debug_assert!(width.is_power_of_two() && height.is_power_of_two());
    let (mut index, mut bit) = (0usize, 0);
    for axis_bit in 0..width.ilog2().max(height.ilog2()) {
        if axis_bit < height.ilog2() {
            index |= (((y >> axis_bit) & 1) as usize) << bit;
            bit += 1;
        }
        if axis_bit < width.ilog2() {
            index |= (((x >> axis_bit) & 1) as usize) << bit;
            bit += 1;
        }
    }
    index
}

/// Reorder compressed blocks only; no color decoding and no RGBA allocation.
pub fn reorder_bc(
    size: Size,
    format: Format,
    source: &[u8],
    target: &mut [u8],
    to_vita: bool,
) -> Result<(), &'static str> {
    let native = format.vita().ok_or("not a BC1/BC3 texture")?;
    if native.byte_len(size) != Some(source.len()) || target.len() != source.len() {
        return Err("invalid native BC texture dimensions or payload");
    }
    let (width, height) = (size.width / 4, size.height / 4);
    match (native == Format::Bc3RgbaVita, to_vita) {
        (false, false) => reorder_blocks::<8, false>(width, height, source, target),
        (false, true) => reorder_blocks::<8, true>(width, height, source, target),
        (true, false) => reorder_blocks::<16, false>(width, height, source, target),
        (true, true) => reorder_blocks::<16, true>(width, height, source, target),
    }
    Ok(())
}

fn reorder_blocks<const BYTES: usize, const TO_VITA: bool>(
    width: u32,
    height: u32,
    source: &[u8],
    target: &mut [u8],
) {
    let x_mask = vita_block_index(width - 1, 0, width, height) * BYTES;
    let y_mask = vita_block_index(0, height - 1, width, height) * BYTES;
    let (mut linear, mut row) = (0, 0usize);
    // Increment only the bits belonging to one axis. Rectangular grids append
    // the longer axis after interleaving, which is already encoded in the masks.
    // No address table or per-block bit loop is needed on the loading thread.
    for _ in 0..height {
        let mut column = 0usize;
        for _ in 0..width {
            let native = row | column;
            let (src, dst) = if TO_VITA {
                (linear, native)
            } else {
                (native, linear)
            };
            target[dst..dst + BYTES].copy_from_slice(&source[src..src + BYTES]);
            linear += BYTES;
            column = column.wrapping_sub(x_mask) & x_mask;
        }
        row = row.wrapping_sub(y_mask) & y_mask;
    }
}

#[derive(Debug)]
pub struct Compressed {
    pub size: Size,
    pub format: Format,
    pub tile_size: Size,
    bytes: Bytes,
    offset: usize,
}
impl Compressed {
    /// Retain the container allocation without copying its block payload.
    pub fn new(
        size: Size,
        format: Format,
        bytes: Bytes,
        offset: usize,
    ) -> Result<Self, &'static str> {
        Self::tiled(size, size, format, bytes, offset)
    }
    /// Equal-size compressed tiles in row-major order; each tile remains a
    /// native texture. The assembled canvas need not be a power of two.
    pub fn tiled(
        size: Size,
        tile_size: Size,
        format: Format,
        bytes: Bytes,
        offset: usize,
    ) -> Result<Self, &'static str> {
        let length = Self::payload_len(size, tile_size, format);
        if length.and_then(|n| offset.checked_add(n)) != Some(bytes.as_slice().len()) {
            return Err("compressed texture byte count differs from dimensions");
        }
        Ok(Self {
            size,
            format,
            tile_size,
            bytes,
            offset,
        })
    }
    pub fn data(&self) -> &[u8] {
        &self.bytes.as_slice()[self.offset..]
    }
    pub fn payload_len(size: Size, tile: Size, format: Format) -> Option<usize> {
        size.rgba_bytes()?;
        if size.width == 0
            || size.height == 0
            || size.width > 65535
            || size.height > 65535
            || tile.width == 0
            || tile.height == 0
        {
            return None;
        }
        // A single POT backing may contain a smaller visible canvas. Native
        // BC storage keeps the padded blocks while image coordinates crop it.
        if size.width <= tile.width && size.height <= tile.height {
            return format.byte_len(tile);
        }
        if !size.width.is_multiple_of(tile.width) || !size.height.is_multiple_of(tile.height) {
            return None;
        }
        let count = (size.width as usize / tile.width as usize)
            .checked_mul(size.height as usize / tile.height as usize)?;
        format.byte_len(tile)?.checked_mul(count)
    }
    pub fn tiles(&self) -> impl Iterator<Item = (crate::graphics::Rect, &[u8])> {
        let columns = self.size.width.div_ceil(self.tile_size.width);
        let bytes = self
            .format
            .byte_len(self.tile_size)
            .expect("validated tile size");
        self.data()
            .chunks_exact(bytes)
            .enumerate()
            .map(move |(index, data)| {
                (
                    crate::graphics::Rect {
                        left: (index as u32 % columns * self.tile_size.width) as i32,
                        top: (index as u32 / columns * self.tile_size.height) as i32,
                        width: self.tile_size.width,
                        height: self.tile_size.height,
                    },
                    data,
                )
            })
    }
}

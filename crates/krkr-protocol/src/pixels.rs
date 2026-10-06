use crate::{
    budget::{Budget, BudgetError, Permit},
    graphics::Size,
};

/// Storage that cannot grow without acquiring another byte permit. Moving it
/// between decoding, a completion and an upload preserves the same charge.
#[derive(Debug)]
pub struct Bytes {
    data: Vec<u8>,
    _permit: Permit,
}
impl Bytes {
    /// Adopt an allocation whose full capacity was reserved by its producer.
    pub fn with_permit(data: Vec<u8>, permit: Permit) -> Self {
        Self {
            data,
            _permit: permit,
        }
    }
    pub fn zeroed(length: usize, budget: &Budget) -> Result<Self, BudgetError> {
        let permit = budget.reserve(length)?;
        Ok(Self {
            data: vec![0; length],
            _permit: permit,
        })
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }
    /// Transfer storage to an owned-buffer codec. Keep the returned permit
    /// alive until that codec has finished using the vector.
    pub fn into_parts(self) -> (Vec<u8>, Permit) {
        (self.data, self._permit)
    }
}
#[derive(Debug)]
pub struct Pixels {
    pub size: Size,
    pub main: Option<Bytes>,
    pub province: Option<Bytes>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Yuv420Layout {
    /// Y followed by separate V and U planes.
    Yv12,
    /// Y followed by interleaved U,V pairs, as returned by Vita AvPlayer.
    Nv12,
}

/// Tightly packed 8-bit 4:2:0, BT.601 limited range, with explicit plane order.
/// The PSV converter normalizes this matrix/range. Decoders copy into
/// budgeted storage before releasing native buffers; no native pointer escapes.
#[derive(Debug)]
pub struct Yuv420 {
    pub size: Size,
    pub layout: Yuv420Layout,
    pub data: Bytes,
}
impl Yuv420 {
    pub fn byte_len(size: Size) -> Option<usize> {
        if size.width == 0
            || size.height == 0
            || !size.width.is_multiple_of(2)
            || !size.height.is_multiple_of(2)
        {
            return None;
        }
        (size.width as usize)
            .checked_mul(size.height as usize)?
            .checked_mul(3)
            .map(|n| n / 2)
    }
}

#[derive(Clone, Debug)]
pub enum VideoPixels {
    Rgba(std::sync::Arc<Pixels>),
    Yuv420(std::sync::Arc<Yuv420>),
}
impl VideoPixels {
    pub fn size(&self) -> Size {
        match self {
            Self::Rgba(p) => p.size,
            Self::Yuv420(p) => p.size,
        }
    }
}
impl From<std::sync::Arc<Pixels>> for VideoPixels {
    fn from(pixels: std::sync::Arc<Pixels>) -> Self {
        Self::Rgba(pixels)
    }
}

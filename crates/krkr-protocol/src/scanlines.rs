//! Ordered main-plane scanline copies, with optional 8-bit horizontal interpolation.
use crate::{budget::Permit, graphics::Rect};
#[derive(Debug)]
pub struct Scanlines {
    pub rectangle: Rect,
    /// Eight-word header followed by one eight-word record per destination row.
    /// Header: top, count, mode, source width/height, previous top, source origin x/y.
    /// Modes: 0 copy, 1 linear, 2 filter half-pixel copy, 3 filter half-pixel blend.
    /// Row: destination x, width, source x/y, fractional x (0..255), three reserved.
    pub words: Vec<i32>,
    pub _permit: Permit,
}

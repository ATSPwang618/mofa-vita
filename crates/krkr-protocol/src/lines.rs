//! Ordered line descriptions and a tile index; no destination pixels cross the VM boundary.
use crate::{budget::Permit, graphics::Rect};

#[derive(Debug)]
pub struct Lines {
    pub rectangle: Rect,
    /// Header, tile ranges, eight-word line records, then ordered record indices.
    pub words: Vec<u32>,
    pub _permit: Permit,
}

pub const TILE_SIZE: u32 = 32;

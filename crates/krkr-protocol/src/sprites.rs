//! Ordered affine sprites sharing one atlas. Backends may bin these by tile.
use crate::{budget::Permit, graphics::Rect};
#[derive(Clone, Copy, Debug)]
pub struct Sprite {
    pub source: Rect,
    pub points: [[f64; 2]; 3],
    pub opacity: u8,
}
#[derive(Debug)]
pub struct Sprites {
    pub clear: Vec<Rect>,
    pub sprites: Vec<Sprite>,
    pub _permit: Permit,
}

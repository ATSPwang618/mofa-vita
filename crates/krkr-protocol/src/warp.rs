//! Source-coordinate effects with native integer interpolation, not alpha compositing.
use crate::{graphics::Rect, pixels::Bytes};
use std::sync::Arc;
#[derive(Debug)]
pub enum Warp {
    Lens {
        radius: f32,
        power: u32,
        table: Arc<Bytes>,
    },
    Vortex {
        radians: f32,
    },
    Stretch {
        source: Rect,
        destination: Rect,
        opacity: i32,
    },
}

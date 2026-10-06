//! Owned main-image filters; coordinates and isolation are supplied by Adjust.
use crate::pixels::Bytes;
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Lookup,
    Colorize {
        amount: u16,
    },
    Modulate {
        hue: f32,
        saturation: f32,
        luminance: f32,
    },
    Noise {
        seed: u32,
        level: Option<i32>,
    },
    Gaussian,
    /// Eight-neighbour average with missing neighbours replaced by the centre.
    Smudge {
        passes: u32,
    },
    Xor {
        color: u32,
    },
    Dither {
        width: u32,
        height: u32,
    },
    /// layerExFilter's scanline-packed LCG, independent of the CxImage noise API.
    RandomFill {
        /// filter.dll uses a different LCG and distinct packed-row tail rules.
        legacy: bool,
        seed: u32,
        under: i32,
        range: i32,
        monochrome: bool,
        hold_alpha: bool,
        rectangle: crate::graphics::Rect,
    },
}
#[derive(Debug)]
pub struct Filter {
    pub kind: Kind,
    /// Lookup/colorize: 256 packed words; Gaussian: odd, normalized f32 kernel.
    /// At most 4096 bytes, already charged to the producer's staging budget.
    pub table: Arc<Bytes>,
}

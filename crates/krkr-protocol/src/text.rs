//! Immutable glyph masks can be cached by either renderer without TJS roots.
use crate::{
    budget::Permit,
    graphics::{DrawFace, Rect, Size},
    pixels::Bytes,
};
use std::sync::Arc;

pub const DEFAULT_FONT_FACE: &str = "QiushuiShotai";

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Font {
    pub face: String,
    pub height: i32,
    pub angle: i32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub file: bool,
}
impl Default for Font {
    fn default() -> Self {
        Self {
            face: DEFAULT_FONT_FACE.into(),
            height: -12,
            angle: 0,
            bold: false,
            italic: false,
            underline: false,
            strikeout: false,
            file: false,
        }
    }
}
#[derive(Debug)]
pub struct Glyph {
    pub id: u64,
    pub size: Size,
    pub origin: [i32; 2],
    pub advance: [i32; 2],
    /// 65-level masks use values 0..64; ordinary masks use 0..255.
    pub levels: u16,
    pub mask: Bytes,
}
#[derive(Debug)]
pub struct PlacedGlyph {
    pub glyph: Arc<Glyph>,
    pub x: i32,
    pub y: i32,
    pub color: u32,
}
#[derive(Debug)]
pub struct Run {
    pub glyphs: Vec<PlacedGlyph>,
    pub permit: Permit,
}
#[derive(Clone, Copy, Debug)]
pub struct Style {
    pub color: u32,
    pub opacity: i16,
    pub antialias: bool,
    pub shadow_level: i32,
    pub shadow_color: u32,
    pub shadow_width: i32,
    pub shadow_offset: [i32; 2],
    pub face: DrawFace,
    pub hold_alpha: bool,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Metrics {
    pub width: i32,
    pub height: i32,
    pub bounds: Rect,
}

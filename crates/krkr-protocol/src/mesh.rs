//! Portable indexed textured triangles. Coordinates are normalized, with Y
//! increasing downwards; backends translate this to their clip-space convention.
use crate::{budget::Permit, graphics::ImageRef, pixels::Pixels};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    pub position: [f32; 2],
    pub uv: [f32; 2],
}
#[derive(Debug)]
pub struct Geometry {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u16>,
    pub _permit: Permit,
}
#[derive(Clone, Debug)]
pub enum Texture {
    /// Immutable decoded asset. Upload once and retain while the asset is used.
    Pixels(Arc<Pixels>),
    Image(ImageRef),
}
#[derive(Clone, Copy, Debug)]
pub enum Blend {
    /// RGB source-alpha blend; alpha is the maximum of source and destination.
    AlphaMax,
    /// RGB source*destination + destination; preserve destination alpha.
    MultiplyAdd,
    /// Source-alpha blend on all channels.
    Alpha,
    /// Layer-manager alpha quantization followed by all-channel alpha blend.
    LayerAlpha,
}
#[derive(Debug)]
pub struct Draw {
    pub geometry: Geometry,
    pub texture: Texture,
    pub blend: Blend,
    pub opacity: f32,
    pub color: [f32; 4],
    /// Replace texture RGB with color RGB, retaining texture alpha.
    pub solid_color: bool,
    /// Draw indices forming an alpha mask. Nested masks are suppressed.
    /// A mask passes at alpha >= 128/255.
    pub masks: Vec<usize>,
    pub visible: bool,
}
#[derive(Debug, Default)]
pub struct Batch {
    pub draws: Vec<Draw>,
    /// Explicit painter order; mask-only draws need not occur in this list.
    pub order: Vec<usize>,
    /// None preserves the existing target. Some clears before drawing.
    pub clear: Option<[f32; 4]>,
}

//! Owned image transformation requests, independent of a graphics API.
use crate::graphics::{Blend, BlendOptions};

/// Projective sampling from source image edges to destination LT, RT, LB, RB.
/// Coordinates are image edges; destination pixel centers are at (x+.5, y+.5).
#[derive(Clone, Copy, Debug)]
pub struct Perspective {
    pub source: [f64; 4], // left, top, right, bottom; reversed/zero spans are valid
    pub destination: [[f64; 2]; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct StretchRect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}
#[derive(Clone, Copy, Debug)]
pub enum Transform {
    Stretch(StretchRect),
    /// Destination edges corresponding to source top-left, top-right and
    /// bottom-left. Pixel centers use integer coordinates, as in TJS Layer.
    Affine([[f64; 2]; 3]),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    FastLinear,
    Linear,
    Cubic,
    Lanczos2,
    Lanczos3,
    Spline16,
    Spline36,
    Area,
    Gaussian,
    Blackman,
}
impl Filter {
    pub fn from_legacy(value: i32) -> Option<Self> {
        Some(match value & 0xffff {
            0 => Self::Nearest,
            1 => Self::FastLinear,
            2 | 4 => Self::Linear,
            3 | 5 => Self::Cubic,
            6 | 7 => Self::Lanczos2,
            8 | 9 => Self::Lanczos3,
            10 | 11 => Self::Spline16,
            12 | 13 => Self::Spline36,
            14 | 15 => Self::Area,
            16 | 17 => Self::Gaussian,
            18 | 19 => Self::Blackman,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Sampling {
    pub filter: Filter,
    pub sharpness: f32,
    pub no_clip: bool,
}
#[derive(Clone, Copy, Debug)]
pub enum ImageOperation {
    /// A copy preserves source alpha, unlike omOpaque on an alpha target.
    Copy {
        hold_alpha: bool,
    },
    Blend(BlendOptions),
}
impl ImageOperation {
    pub fn is_noop(self) -> bool {
        matches!(self, Self::Blend(options) if options.is_noop())
    }
    pub fn affine_supported(self) -> bool {
        // Stock affine/nearest stretch dispatch only implements these modes.
        matches!(
            self,
            Self::Copy { .. }
                | Self::Blend(BlendOptions {
                    mode: Blend::Opaque | Blend::Alpha | Blend::AddAlpha,
                    ..
                })
        )
    }
    pub fn affine_linear(self) -> bool {
        use crate::graphics::DrawFace;
        matches!(
            self,
            Self::Copy { hold_alpha: false }
                | Self::Blend(BlendOptions {
                    mode: Blend::Opaque | Blend::AddAlpha,
                    face: DrawFace::Opaque,
                    hold_alpha: false,
                    ..
                })
        )
    }
}

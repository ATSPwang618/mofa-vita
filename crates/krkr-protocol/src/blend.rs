//! Drawable layer types and operation modes share their legacy numeric IDs.
use crate::graphics::DrawFace;
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    Opaque = 1,
    Alpha = 2,
    Additive = 3,
    Subtractive = 4,
    Multiplicative = 5,
    Dodge = 8,
    Darken = 9,
    Lighten = 10,
    Screen = 11,
    AddAlpha = 12,
    PsNormal = 13,
    PsAdditive = 14,
    PsSubtractive = 15,
    PsMultiplicative = 16,
    PsScreen = 17,
    PsOverlay = 18,
    PsHardLight = 19,
    PsSoftLight = 20,
    PsColorDodge = 21,
    PsColorDodge5 = 22,
    PsColorBurn = 23,
    PsLighten = 24,
    PsDarken = 25,
    PsDifference = 26,
    PsDifference5 = 27,
    PsExclusion = 28,
}
impl Blend {
    pub fn from_legacy(value: i32) -> Option<Self> {
        Some(match value {
            1 => Self::Opaque,
            2 => Self::Alpha,
            3 => Self::Additive,
            4 => Self::Subtractive,
            5 => Self::Multiplicative,
            8 => Self::Dodge,
            9 => Self::Darken,
            10 => Self::Lighten,
            11 => Self::Screen,
            12 => Self::AddAlpha,
            13 => Self::PsNormal,
            14 => Self::PsAdditive,
            15 => Self::PsSubtractive,
            16 => Self::PsMultiplicative,
            17 => Self::PsScreen,
            18 => Self::PsOverlay,
            19 => Self::PsHardLight,
            20 => Self::PsSoftLight,
            21 => Self::PsColorDodge,
            22 => Self::PsColorDodge5,
            23 => Self::PsColorBurn,
            24 => Self::PsLighten,
            25 => Self::PsDarken,
            26 => Self::PsDifference,
            27 => Self::PsDifference5,
            28 => Self::PsExclusion,
            _ => return None,
        })
    }
    pub fn face(self) -> DrawFace {
        match self {
            Self::AddAlpha => DrawFace::AddAlpha,
            Self::Opaque
            | Self::Additive
            | Self::Subtractive
            | Self::Multiplicative
            | Self::Dodge
            | Self::Darken
            | Self::Lighten
            | Self::Screen => DrawFace::Opaque,
            _ => DrawFace::Alpha,
        }
    }
    pub fn neutral(self) -> u32 {
        match self {
            Self::Opaque
            | Self::Alpha
            | Self::Subtractive
            | Self::Multiplicative
            | Self::Darken
            | Self::PsSubtractive
            | Self::PsMultiplicative
            | Self::PsColorBurn
            | Self::PsDarken => 0x00ffffff,
            Self::PsOverlay | Self::PsHardLight | Self::PsSoftLight => 0x00808080,
            _ => 0,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Opaque => "opaque",
            Self::Alpha => "alpha",
            Self::Additive => "add",
            Self::Subtractive => "sub",
            Self::Multiplicative => "mul",
            Self::Dodge => "dodge",
            Self::Darken => "darken",
            Self::Lighten => "lighten",
            Self::Screen => "screen",
            Self::AddAlpha => "addalpha",
            Self::PsNormal => "psnormal",
            Self::PsAdditive => "psadd",
            Self::PsSubtractive => "pssub",
            Self::PsMultiplicative => "psmul",
            Self::PsScreen => "psscreen",
            Self::PsOverlay => "psoverlay",
            Self::PsHardLight => "pshlight",
            Self::PsSoftLight => "psslight",
            Self::PsColorDodge => "psdodge",
            Self::PsColorDodge5 => "psdodge5",
            Self::PsColorBurn => "psburn",
            Self::PsLighten => "pslighten",
            Self::PsDarken => "psdarken",
            Self::PsDifference => "psdiff",
            Self::PsDifference5 => "psdiff5",
            Self::PsExclusion => "psexcl",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct BlendOptions {
    pub mode: Blend,
    pub face: DrawFace,
    pub opacity: u8,
    pub hold_alpha: bool,
}
impl BlendOptions {
    /// Layer-tree composition follows BaseLayer::BltImage, independently of
    /// the script's draw face/holdAlpha settings for editing image pixels.
    pub fn for_composition(mode: Blend, face: DrawFace, opacity: u8) -> Self {
        Self {
            mode,
            face,
            opacity,
            // Color effects preserve an alpha-bearing parent's mask. Ordinary
            // copy/alpha modes instead update it through their over operation.
            hold_alpha: !matches!(mode, Blend::Opaque | Blend::Alpha | Blend::AddAlpha)
                && matches!(face, DrawFace::Alpha | DrawFace::AddAlpha),
        }
    }
    pub fn accepts_face(self) -> bool {
        !matches!(self.mode, Blend::Opaque | Blend::Alpha | Blend::AddAlpha)
            || matches!(
                self.face,
                DrawFace::Opaque | DrawFace::Alpha | DrawFace::AddAlpha
            )
    }
    pub fn is_noop(self) -> bool {
        self.opacity == 0 || (self.mode == Blend::AddAlpha && self.face == DrawFace::Alpha)
    }
}

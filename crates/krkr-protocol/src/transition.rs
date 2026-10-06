//! Portable transition parameters. Pixel kernels and script scheduling consume
//! the same integer phase, without depending on a desktop graphics API.
use crate::graphics::{DrawFace, Size};
pub mod custom;

#[derive(Clone, Debug)]
pub struct SceneTransition {
    pub destination: usize,
    pub source: usize,
    pub with_children: bool,
    pub frame: Frame,
    pub rule: Option<crate::graphics::ImageRef>,
    pub custom: Option<custom::Frame>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    CrossFade,
    Universal { vague: u32 },
    Scroll { from: Direction, stay: Stay },
    Custom,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Direction {
    Left,
    Top,
    Right,
    Bottom,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Stay {
    Neither,
    Destination,
    Source,
}
impl Direction {
    pub fn from_legacy(value: i32) -> Option<Self> {
        Some(match value {
            0 => Self::Left,
            1 => Self::Top,
            2 => Self::Right,
            3 => Self::Bottom,
            _ => return None,
        })
    }
}
impl Stay {
    pub fn from_legacy(value: i32) -> Option<Self> {
        Some(match value {
            0 => Self::Neither,
            1 => Self::Destination,
            2 => Self::Source,
            _ => return None,
        })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub effect: Effect,
    pub face: DrawFace,
    pub size: Size,
    pub phase: u32,
}
impl Effect {
    pub fn phases(self, size: Size) -> u32 {
        match self {
            Self::Custom => u32::MAX,
            Self::CrossFade => 255,
            Self::Universal { vague } => 255u32.saturating_add(vague),
            Self::Scroll {
                from: Direction::Left | Direction::Right,
                ..
            } => size.width,
            Self::Scroll { .. } => size.height,
        }
    }
    pub fn frame(self, face: DrawFace, size: Size, elapsed: u64, duration: u64) -> Frame {
        let max = self.phases(size);
        let phase = (u128::from(elapsed) * u128::from(max) / u128::from(duration.max(2)))
            .min(u128::from(max)) as u32;
        Frame {
            effect: self,
            face,
            size,
            phase,
        }
    }
}

//! Portable presentation state for the focused layer and pointer target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub cursor: i32,
    pub ime: i32,
    pub attention: Option<crate::graphics::Rect>,
}

#[derive(Debug)]
pub struct CursorImage {
    pub width: u16,
    pub height: u16,
    pub hotspot: (u16, u16),
    pub rgba: crate::pixels::Bytes,
}

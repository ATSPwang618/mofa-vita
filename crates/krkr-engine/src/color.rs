//! Stable portable system palette for legacy script color identifiers.
//! Pixel colors remain 0xRRGGBB on every backend.
pub(crate) const SYSTEM_COLORS: &[(&str, u32)] = &[
    ("clScrollBar", 0xc8c8c8),
    ("clBackground", 0x000000),
    ("clActiveCaption", 0x99b4d1),
    ("clInactiveCaption", 0xbfcddb),
    ("clMenu", 0xf0f0f0),
    ("clWindow", 0xffffff),
    ("clWindowFrame", 0x646464),
    ("clMenuText", 0x000000),
    ("clWindowText", 0x000000),
    ("clCaptionText", 0x000000),
    ("clActiveBorder", 0xb4b4b4),
    ("clInactiveBorder", 0xf4f7fc),
    ("clAppWorkSpace", 0xababab),
    ("clHighlight", 0x0078d7),
    ("clHighlightText", 0xffffff),
    ("clBtnFace", 0xf0f0f0),
    ("clBtnShadow", 0xa0a0a0),
    ("clGrayText", 0x6d6d6d),
    ("clBtnText", 0x000000),
    ("clInactiveCaptionText", 0x434e54),
    ("clBtnHighlight", 0xffffff),
    ("cl3DDkShadow", 0x696969),
    ("cl3DLight", 0xe3e3e3),
    ("clInfoText", 0x000000),
    ("clInfoBk", 0xffffe1),
];

pub(crate) fn actual(color: u32) -> u32 {
    if color & 0xffffff00 == 0x80000000 {
        SYSTEM_COLORS
            .get((color & 0xff) as usize)
            .map_or(0, |(_, rgb)| *rgb)
    } else {
        color & 0x00ffffff
    }
}

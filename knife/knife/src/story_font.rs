//! Vita story font size. Shortcut handling lives in vita_controls.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size(u8);

impl Default for Size {
    fn default() -> Self {
        Self(100)
    }
}

impl Size {
    pub const MIN: u8 = 50;
    pub const MAX: u8 = 150;
    const STEP: i16 = 5;

    pub fn percent(self) -> u8 {
        self.0
    }

    pub fn factor(self) -> f32 {
        f32::from(self.0) / 100.0
    }

    pub fn step(self, direction: i16) -> Self {
        let value = i16::from(self.0) + Self::STEP * direction;
        Self(value.clamp(i16::from(Self::MIN), i16::from(Self::MAX)) as u8)
    }

    pub fn parse(value: &str) -> Self {
        value.trim().parse::<u8>().ok()
            .filter(|p| (Self::MIN..=Self::MAX).contains(p) && p % Self::STEP as u8 == 0)
            .map(Self).unwrap_or_default()
    }
}



/// Scale layout metrics, retaining the text box and its top edge. 100% is exact.
pub fn scale_layout(size: Size, lay: crate::text::Layout, baseline: f32) -> (crate::text::Layout, f32) {
    let k = size.factor();
    if k == 1.0 { return (lay, baseline); }
    let ascent = lay.ascent * k;
    (crate::text::Layout {
        size: lay.size * k, spacing: lay.spacing * k, line_height: lay.line_height * k,
        ascent, ruby_y: lay.ruby_y * k, ..lay
    }, baseline - lay.ascent + ascent)
}

/// Follow only revealed lines, never jump ahead to an undisplayed paragraph.
pub fn scroll_offset(size: Size, revealed: f32, offset_y: f32, bottom: f32,
    chars: impl Iterator<Item = (usize, f32)>) -> f32 {
    if size.factor() <= 1.0 { return 0.0; }
    chars.filter(|(order, _)| (*order as f32) < revealed)
        .fold(0.0, |dy, (_, baseline)| dy.max(baseline + offset_y - bottom))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_steps_and_config_bounds() {
        let normal = Size::default();
        assert_eq!(normal.percent(), 100);
        let mut size = Size::parse("50");
        assert_eq!(size.step(-1).percent(), 50);
        for percent in (50..=150).step_by(5) {
            assert_eq!(size.percent(), percent);
            assert_eq!(Size::parse(&format!("{percent}\n")), size);
            size = size.step(1);
        }
        assert_eq!(size.factor(), 1.5);
        for bad in ["", "149", "49", "151", "-50", "999", "NaN"] {
            assert_eq!(Size::parse(bad), normal);
        }
    }

    #[test]
    fn layout_scales_metrics_but_keeps_box_and_top() {
        let base = crate::text::Layout { left: 264.0, right: 1656.0, size: 45.6,
            spacing: 4.0, sx: 0.85, line_height: 81.0, ascent: 40.0, ruby_y: -6.75 };
        for percent in [50, 100, 150] {
            let size = Size::parse(&percent.to_string());
            let (lay, baseline) = scale_layout(size, base, 235.0);
            assert_eq!((lay.left, lay.right, lay.sx), (base.left, base.right, base.sx));
            assert_eq!(baseline - lay.ascent, 195.0);
            assert_eq!(lay.size, base.size * size.factor());
            assert_eq!(lay.line_height, base.line_height * size.factor());
            assert_eq!(lay.spacing, base.spacing * size.factor());
            assert_eq!(lay.ruby_y, base.ruby_y * size.factor());
        }
        assert_eq!(scale_layout(Size::default(), base, 235.0).1.to_bits(), 235.0f32.to_bits());
    }

    #[test]
    fn scroll_follows_revealed_line_and_resets_for_normal_size() {
        let chars = [(0, 235.0), (1, 950.0), (2, 1100.0), (3, 1300.0)];
        let large = Size::parse("150");
        let scroll = |size, shown| scroll_offset(size, shown, 10.0, 1000.0, chars.into_iter());
        assert_eq!(scroll(large, 0.0), 0.0);
        assert_eq!(scroll(large, 2.0), 0.0);
        assert_eq!(scroll(large, 2.5), 110.0);
        assert_eq!(scroll(large, 4.0), 310.0);
        assert_eq!(scroll(Size::default(), 4.0), 0.0);
        assert_eq!(scroll(Size::parse("50"), 4.0), 0.0);
        assert_eq!(scroll_offset(large, 10.0, 0.0, 1000.0, std::iter::empty()), 0.0);
    }
}

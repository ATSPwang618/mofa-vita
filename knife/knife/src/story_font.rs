//! Vita reading-size presets and the L + Square shortcut.

pub const L: u32 = 0x0100;
pub const SQUARE: u32 = 0x8000;
pub const CHORD: u32 = L | SQUARE;

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

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Toggle,
    Tap(u32),
}

#[derive(Default)]
pub struct Shortcut {
    held: u32,
    captured: bool,
}

impl Shortcut {
    pub fn update(&mut self, buttons: u32) -> Option<Event> {
        let held = buttons & CHORD;
        let released = std::mem::replace(&mut self.held, held) & !held;
        // A chord owns both buttons until both are up. A lone button acts on release,
        // so either button can be pressed first without leaking its game action.
        if self.captured {
            self.captured = held != 0;
            None
        } else if held == CHORD {
            self.captured = true;
            Some(Event::Toggle)
        } else {
            (released != 0).then_some(Event::Tap(released))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chord_fires_once_and_swallows_both_releases_in_either_order() {
        for (first, second) in [(L, SQUARE), (SQUARE, L)] {
            let mut chord = Shortcut::default();
            assert_eq!(chord.update(first), None);
            assert_eq!(chord.update(first | second), Some(Event::Toggle));
            assert_eq!(chord.update(first | second), None);
            assert_eq!(chord.update(second), None);
            assert_eq!(chord.update(first | second), None);
            assert_eq!(chord.update(0), None);
            assert_eq!(chord.update(first | second), Some(Event::Toggle));
        }
    }

    #[test]
    fn individual_buttons_still_produce_one_tap() {
        let mut chord = Shortcut::default();
        for button in [L, SQUARE] {
            assert_eq!(chord.update(button), None);
            assert_eq!(chord.update(button), None);
            assert_eq!(chord.update(0), Some(Event::Tap(button)));
            assert_eq!(chord.update(0), None);
        }
    }

    #[test]
    fn five_percent_steps_include_both_limits_and_reject_invalid_config() {
        let normal = Size::default();
        assert_eq!(normal.percent(), 100);
        assert_eq!(normal.step(-1).percent(), 95);
        assert_eq!(normal.step(1).factor(), 1.05);
        let mut size = Size::parse("50");
        for percent in (50..=150).step_by(5) {
            assert_eq!(size.percent(), percent);
            assert_eq!(Size::parse(&format!("{percent}\n")), size);
            size = size.step(1);
        }
        assert_eq!(size.factor(), 1.5);
        assert_eq!(size.step(1), size);
        assert_eq!(Size::parse("50").step(-1).factor(), 0.5);
        for bad in ["", "149", "0", "200"] {
            assert_eq!(Size::parse(bad), normal);
        }
    }
}

//! Shared settings shortcut and rear-panel bounds; story skip policy is unchanged.
pub const L: u32 = 0x0100;
pub const SQUARE: u32 = 0x8000;
pub const CHORD: u32 = L | SQUARE;

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

}

/// Select a rear-panel half only inside the reported active display area.
pub fn rear_side(mode: u8, bounds: [i16; 4], point: [i16; 2]) -> Option<usize> {
    let [x0, y0, x1, y1] = bounds.map(i32::from);
    let [x, y] = point.map(i32::from);
    if x1 <= x0 || y1 <= y0 || x < x0 || x > x1 || y < y0 || y > y1 {
        return None;
    }
    let row = match mode {
        1 => true,
        2 => y < (y0 + y1) / 2,
        3 => y >= (y0 + y1) / 2,
        _ => false,
    };
    row.then_some(usize::from(x >= (x0 + x1) / 2))
}

#[cfg(test)]
mod rear_tests {
    use super::rear_side;
    #[test]
    fn modes_boundaries_and_disabled_regions() {
        let b = [100, 200, 1900, 900];
        for (p, side) in [([100, 200], 0), ([1900, 900], 1), ([1000, 550], 1)] {
            assert_eq!(rear_side(0, b, p), None);
            assert_eq!(rear_side(1, b, p), Some(side));
        }
        assert_eq!(rear_side(2, b, [999, 549]), Some(0));
        assert_eq!(rear_side(2, b, [1000, 550]), None);
        assert_eq!(rear_side(3, b, [1000, 550]), Some(1));
        assert_eq!(rear_side(3, b, [999, 549]), None);
        for p in [[99, 300], [500, 199], [1901, 500], [500, 901]] {
            assert_eq!(rear_side(1, b, p), None);
        }
        assert_eq!(rear_side(1, [1, 2, 1, 9], [1, 3]), None);
        assert_eq!(rear_side(4, b, [200, 300]), None);
    }
}

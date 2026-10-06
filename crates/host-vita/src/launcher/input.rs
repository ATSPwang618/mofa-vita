//! Maps sampled Vita input to Nivora without performing device I/O.
use super::ui::Action;
use crate::input::{CIRCLE, CROSS, Sample, TRIANGLE};
use nivora_platform::{InputEvent, Key, Point};
use smallvec::SmallVec;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(super) enum Event {
    Ui(InputEvent),
    Shortcut(Action),
    CancelPointer,
}
pub(super) struct Input {
    buttons: u32,
    held: u32,
    repeat_at: Option<Instant>,
    touch: Option<(u8, Point)>,
    touch_ready: bool,
}
impl Input {
    pub fn new(sample: Sample) -> Self {
        Self {
            buttons: buttons(sample),
            held: 0,
            repeat_at: None,
            touch: None,
            touch_ready: sample.touch.is_none(),
        }
    }
    pub fn poll(&mut self, sample: Sample, now: Instant) -> SmallVec<[Event; 8]> {
        let mut events = SmallVec::new();
        let current = buttons(sample);
        let held = current & 0xf0;
        let mut pressed = current & !self.buttons;
        if held != self.held {
            self.held = held;
            self.repeat_at = (held != 0).then_some(now + Duration::from_millis(350));
        } else if self.repeat_at.is_some_and(|at| now >= at) {
            pressed |= held;
            self.repeat_at = Some(now + Duration::from_millis(85));
        }
        for (mask, key) in [
            (0x10, Key::Up),
            (0x40, Key::Down),
            (0x80, Key::Left),
            (0x20, Key::Right),
            (CIRCLE, Key::Accept),
            (CROSS, Key::Back),
        ] {
            if pressed & mask != 0 {
                events.push(Event::Ui(InputEvent::KeyDown(key)));
            }
            if self.buttons & !current & mask != 0 {
                events.push(Event::Ui(InputEvent::KeyUp(key)));
            }
        }
        for (mask, action) in [
            (1, Action::Settings),
            (8, Action::Refresh),
            (TRIANGLE, Action::OpenSelected),
            (0x8000, Action::OpenSelected),
            (0x100, Action::PageUp),
            (0x200, Action::PageDown),
        ] {
            if pressed & mask != 0 {
                events.push(Event::Shortcut(action));
            }
        }
        self.buttons = current;
        let touch = sample.touch.map(|(id, (x, y))| {
            (
                id,
                Point {
                    x: x as f32,
                    y: y as f32,
                },
            )
        });
        match (self.touch, touch) {
            (Some((id, old)), Some((next, point))) if id == next => {
                if old != point {
                    events.push(Event::Ui(InputEvent::PointerMove(point)));
                }
                self.touch = touch;
            }
            (Some(_), Some(_)) => {
                self.touch = None;
                self.touch_ready = false;
                events.push(Event::CancelPointer);
            }
            (Some((_, point)), None) => {
                self.touch = None;
                self.touch_ready = true;
                events.push(Event::Ui(InputEvent::PointerUp(point)));
            }
            (None, Some((_, point))) if self.touch_ready => {
                self.touch = touch;
                events.push(Event::Ui(InputEvent::PointerDown(point)));
            }
            (None, None) => self.touch_ready = true,
            _ => {}
        }
        events
    }
}
fn buttons(sample: Sample) -> u32 {
    let mut bits = sample.buttons;
    if sample.analog[0] < 72 {
        bits |= 0x80;
    }
    if sample.analog[0] > 184 {
        bits |= 0x20;
    }
    if sample.analog[1] < 72 {
        bits |= 0x10;
    }
    if sample.analog[1] > 184 {
        bits |= 0x40;
    }
    bits
}

#[cfg(all(test, not(target_os = "vita")))]
#[path = "../../tests/launcher/input.rs"]
mod tests;

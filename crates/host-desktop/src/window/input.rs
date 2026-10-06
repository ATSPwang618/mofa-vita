use krkr_protocol::window::{Event, Host, Input, WindowId};
use std::time::{Duration, Instant};
pub(super) const DOUBLE_CLICK_MILLIS: u64 = 500;
use winit::{
    event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent},
    keyboard::{KeyCode, PhysicalKey},
};

#[derive(Default)]
pub(super) struct State {
    x: i32,
    y: i32,
    modifiers: u32,
    buttons: u32,
    left_down: bool,
    double: bool,
    last_click: Option<(Instant, i32, i32)>,
    wheel: f64,
}
impl State {
    pub(super) fn cancel_gesture(&mut self) {
        self.buttons = 0;
        self.modifiers = 0;
        self.left_down = false;
        self.double = false;
        self.last_click = None;
        self.wheel = 0.0;
    }
    pub(super) fn set_position(&mut self, x: i32, y: i32) {
        self.x = x;
        self.y = y;
    }
    fn shift(&self) -> u32 {
        self.modifiers | self.buttons
    }
    pub fn deliver(
        &mut self,
        event: WindowEvent,
        window: WindowId,
        host: &Host,
    ) -> Result<(), String> {
        let post = |input| host.post(Event { window, input });
        match event {
            WindowEvent::CloseRequested => post(Input::Close)?,
            WindowEvent::Focused(active) => {
                if !active {
                    self.buttons = 0;
                    self.modifiers = 0;
                    self.left_down = false;
                }
                post(Input::Focus(active))?;
            }
            WindowEvent::CursorEntered { .. } => post(Input::MouseEnter)?,
            WindowEvent::CursorLeft { .. } => post(Input::MouseLeave)?,
            WindowEvent::CursorMoved { position, .. } => {
                self.x = position.x as i32;
                self.y = position.y as i32;
                post(Input::MouseMove {
                    x: self.x,
                    y: self.y,
                    shift: self.shift(),
                })?;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let modifiers = modifiers.state();
                host.update_key_state(16, modifiers.shift_key());
                host.update_key_state(17, modifiers.control_key());
                host.update_key_state(18, modifiers.alt_key());
                self.modifiers = u32::from(modifiers.shift_key())
                    | (u32::from(modifiers.alt_key()) << 1)
                    | (u32::from(modifiers.control_key()) << 2);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (button, mask) = match button {
                    MouseButton::Left => (0, 8),
                    MouseButton::Right => (1, 16),
                    MouseButton::Middle => (2, 32),
                    MouseButton::Back => (3, 256),
                    MouseButton::Forward => (4, 512),
                    MouseButton::Other(_) => return Ok(()),
                };
                let pressed = state == ElementState::Pressed;
                if pressed {
                    self.buttons |= mask;
                } else {
                    self.buttons &= !mask;
                }
                let (x, y, shift) = (self.x, self.y, self.shift());
                if pressed {
                    if button == 0 {
                        self.left_down = true;
                        self.double = self.last_click.is_some_and(|(time, last_x, last_y)| {
                            time.elapsed() <= Duration::from_millis(DOUBLE_CLICK_MILLIS)
                                && x.abs_diff(last_x) <= 4
                                && y.abs_diff(last_y) <= 4
                        });
                        if self.double {
                            self.last_click = None;
                            post(Input::DoubleClick { x, y })?;
                        } else {
                            self.last_click = Some((Instant::now(), x, y));
                        }
                    }
                    post(Input::MouseDown {
                        x,
                        y,
                        button,
                        shift,
                    })?;
                } else {
                    // The original Window sends left click before mouse-up.
                    if button == 0 {
                        if self.left_down && !self.double {
                            post(Input::Click { x, y })?;
                        }
                        self.left_down = false;
                        self.double = false;
                    }
                    post(Input::MouseUp {
                        x,
                        y,
                        button,
                        shift,
                    })?;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.wheel += match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y) * 120.0,
                    MouseScrollDelta::PixelDelta(position) => position.y,
                };
                let delta = self.wheel as i32;
                self.wheel -= f64::from(delta);
                if delta != 0 {
                    post(Input::Wheel {
                        x: self.x,
                        y: self.y,
                        shift: self.shift(),
                        delta,
                    })?;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key
                    && let Some(key) = virtual_key(code)
                {
                    let side = match code {
                        KeyCode::ShiftLeft => Some(160),
                        KeyCode::ShiftRight => Some(161),
                        KeyCode::ControlLeft => Some(162),
                        KeyCode::ControlRight => Some(163),
                        KeyCode::AltLeft => Some(164),
                        KeyCode::AltRight => Some(165),
                        _ => None,
                    };
                    if let Some(key) = side {
                        host.update_key_state(key, event.state == ElementState::Pressed);
                    }
                    let shift = self.shift() | if event.repeat { 128 } else { 0 };
                    post(if event.state == ElementState::Pressed {
                        Input::KeyDown { key, shift }
                    } else {
                        Input::KeyUp { key, shift }
                    })?;
                }
                if event.state == ElementState::Pressed
                    && let Some(text) = event.text
                {
                    for unit in text.encode_utf16() {
                        post(Input::KeyPress(unit))?;
                    }
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                for unit in text.encode_utf16() {
                    post(Input::KeyPress(unit))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}
fn virtual_key(key: KeyCode) -> Option<u32> {
    Some(match key {
        KeyCode::KeyA => 65,
        KeyCode::KeyB => 66,
        KeyCode::KeyC => 67,
        KeyCode::KeyD => 68,
        KeyCode::KeyE => 69,
        KeyCode::KeyF => 70,
        KeyCode::KeyG => 71,
        KeyCode::KeyH => 72,
        KeyCode::KeyI => 73,
        KeyCode::KeyJ => 74,
        KeyCode::KeyK => 75,
        KeyCode::KeyL => 76,
        KeyCode::KeyM => 77,
        KeyCode::KeyN => 78,
        KeyCode::KeyO => 79,
        KeyCode::KeyP => 80,
        KeyCode::KeyQ => 81,
        KeyCode::KeyR => 82,
        KeyCode::KeyS => 83,
        KeyCode::KeyT => 84,
        KeyCode::KeyU => 85,
        KeyCode::KeyV => 86,
        KeyCode::KeyW => 87,
        KeyCode::KeyX => 88,
        KeyCode::KeyY => 89,
        KeyCode::KeyZ => 90,
        KeyCode::Digit0 => 48,
        KeyCode::Digit1 => 49,
        KeyCode::Digit2 => 50,
        KeyCode::Digit3 => 51,
        KeyCode::Digit4 => 52,
        KeyCode::Digit5 => 53,
        KeyCode::Digit6 => 54,
        KeyCode::Digit7 => 55,
        KeyCode::Digit8 => 56,
        KeyCode::Digit9 => 57,
        KeyCode::Numpad0 => 96,
        KeyCode::Numpad1 => 97,
        KeyCode::Numpad2 => 98,
        KeyCode::Numpad3 => 99,
        KeyCode::Numpad4 => 100,
        KeyCode::Numpad5 => 101,
        KeyCode::Numpad6 => 102,
        KeyCode::Numpad7 => 103,
        KeyCode::Numpad8 => 104,
        KeyCode::Numpad9 => 105,
        KeyCode::F1 => 112,
        KeyCode::F2 => 113,
        KeyCode::F3 => 114,
        KeyCode::F4 => 115,
        KeyCode::F5 => 116,
        KeyCode::F6 => 117,
        KeyCode::F7 => 118,
        KeyCode::F8 => 119,
        KeyCode::F9 => 120,
        KeyCode::F10 => 121,
        KeyCode::F11 => 122,
        KeyCode::F12 => 123,
        KeyCode::F13 => 124,
        KeyCode::F14 => 125,
        KeyCode::F15 => 126,
        KeyCode::F16 => 127,
        KeyCode::F17 => 128,
        KeyCode::F18 => 129,
        KeyCode::F19 => 130,
        KeyCode::F20 => 131,
        KeyCode::F21 => 132,
        KeyCode::F22 => 133,
        KeyCode::F23 => 134,
        KeyCode::F24 => 135,
        KeyCode::Backspace => 8,
        KeyCode::Tab => 9,
        KeyCode::Enter => 13,
        KeyCode::NumpadEnter => 13,
        KeyCode::ShiftLeft => 16,
        KeyCode::ShiftRight => 16,
        KeyCode::ControlLeft => 17,
        KeyCode::ControlRight => 17,
        KeyCode::AltLeft => 18,
        KeyCode::AltRight => 18,
        KeyCode::Pause => 19,
        KeyCode::CapsLock => 20,
        KeyCode::Escape => 27,
        KeyCode::Space => 32,
        KeyCode::PageUp => 33,
        KeyCode::PageDown => 34,
        KeyCode::End => 35,
        KeyCode::Home => 36,
        KeyCode::ArrowLeft => 37,
        KeyCode::ArrowUp => 38,
        KeyCode::ArrowRight => 39,
        KeyCode::ArrowDown => 40,
        KeyCode::PrintScreen => 44,
        KeyCode::Insert => 45,
        KeyCode::Delete => 46,
        KeyCode::SuperLeft => 91,
        KeyCode::SuperRight => 92,
        KeyCode::ContextMenu => 93,
        KeyCode::NumpadMultiply => 106,
        KeyCode::NumpadAdd => 107,
        KeyCode::NumpadComma => 108,
        KeyCode::NumpadSubtract => 109,
        KeyCode::NumpadDecimal => 110,
        KeyCode::NumpadDivide => 111,
        KeyCode::NumLock => 144,
        KeyCode::ScrollLock => 145,
        KeyCode::Semicolon => 186,
        KeyCode::Equal => 187,
        KeyCode::Comma => 188,
        KeyCode::Minus => 189,
        KeyCode::Period => 190,
        KeyCode::Slash => 191,
        KeyCode::Backquote => 192,
        KeyCode::BracketLeft => 219,
        KeyCode::Backslash => 220,
        KeyCode::BracketRight => 221,
        KeyCode::Quote => 222,
        KeyCode::IntlBackslash => 226,
        _ => return None,
    })
}

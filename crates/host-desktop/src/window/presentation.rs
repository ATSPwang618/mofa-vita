use super::*;
use winit::window::CursorIcon;

impl Native {
    pub(super) fn set_cursor_state(&mut self, state: i32) {
        self.cursor_state = state;
        self.hidden_at = (state == 1).then_some(self.cursor);
        self.window
            .set_cursor_visible(state == 0 && self.input_style.cursor != -1);
    }
    pub(super) fn set_input_style(
        &mut self,
        style: krkr_protocol::input_style::Style,
        cursors: &HashMap<
            i32,
            (
                Weak<krkr_protocol::input_style::CursorImage>,
                winit::window::CustomCursor,
            ),
        >,
    ) -> Result<(), String> {
        if style.cursor >= 2 {
            let cursor = cursors.get(&style.cursor).ok_or("unknown cursor handle")?;
            self.window.set_cursor(cursor.1.clone());
        } else {
            let cursor = match style.cursor {
                -2..=0 => CursorIcon::Default,
                -3 => CursorIcon::Crosshair,
                -4 => CursorIcon::Text,
                -5 | -22 => CursorIcon::Move,
                -6 => CursorIcon::NeswResize,
                -7 => CursorIcon::NsResize,
                -8 => CursorIcon::NwseResize,
                -9 => CursorIcon::EwResize,
                -10 => CursorIcon::NResize,
                -11 | -17 => CursorIcon::Wait,
                -12 => CursorIcon::Grab,
                -13 => CursorIcon::NoDrop,
                -14 => CursorIcon::ColResize,
                -15 => CursorIcon::RowResize,
                -16 => CursorIcon::Copy,
                -18 => CursorIcon::NotAllowed,
                -19 => CursorIcon::Progress,
                -20 => CursorIcon::Help,
                -21 => CursorIcon::Pointer,
                1 => CursorIcon::VerticalText,
                _ => return Err("unknown cursor handle".into()),
            };
            self.window.set_cursor(cursor);
        }
        // winit controls whether IME is available; conversion mode is selected
        // by the user's input method. DontCare leaves the active state alone.
        if style.ime != 3 {
            self.window.set_ime_allowed(matches!(style.ime, 2 | 4..=11));
        }
        if let Some(area) = style.attention {
            self.window.set_ime_cursor_area(
                PhysicalPosition::new(area.left, area.top),
                PhysicalSize::new(area.width, area.height),
            );
        } else {
            self.window
                .set_ime_cursor_area(PhysicalPosition::new(0, 0), PhysicalSize::new(1, 1));
        }
        self.input_style = style;
        self.window
            .set_cursor_visible(self.cursor_state == 0 && style.cursor != -1);
        Ok(())
    }
}

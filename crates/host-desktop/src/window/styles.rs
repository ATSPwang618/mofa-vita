use super::Native;
use krkr_protocol::window::BorderStyle;
use winit::window::WindowButtons;

impl Native {
    pub(super) fn set_border_style(&mut self, style: BorderStyle) {
        use BorderStyle::*;
        self.border_style = style;
        let resizable = matches!(style, Sizeable | SizeToolWin) && !self.disable_resize;
        let buttons = match style {
            Sizeable => WindowButtons::all(),
            None | Single => WindowButtons::CLOSE | WindowButtons::MINIMIZE,
            Dialog | ToolWindow | SizeToolWin => WindowButtons::CLOSE,
        };
        self.window.set_resizable(resizable);
        self.window.set_enabled_buttons(buttons);
        self.window.set_decorations(style != None);
        self.window.request_redraw();
    }
}

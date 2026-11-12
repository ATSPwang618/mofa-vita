//! VitaSDK controller and front-panel input, polled once per game frame.
use crate::{
    game::Game,
    story_font::{CHORD, Event, L, SQUARE, Shortcut, Size},
    ui::{Pad, Screen, Ui},
};
use knife_render::{CANVAS_H, CANVAS_W, Quad, Renderer};
use std::path::PathBuf;

const LEFT: u32 = 0x80;
const RIGHT: u32 = 0x20;
const CIRCLE: u32 = 0x2000;
const CROSS: u32 = 0x4000;

// Buttons follow the Japanese (Asian) PlayStation layout: Circle confirms and advances, Cross
// cancels and goes back (the reverse of the Western convention). The game itself is Switch code
// whose A is "confirm" and B is "cancel", so Pad::A gets SCE_CTRL_CIRCLE and Pad::B SCE_CTRL_CROSS.
const BUTTONS: &[(u32, Pad)] = &[
    (0x10, Pad::Up),
    (0x40, Pad::Down),
    (LEFT, Pad::Left),
    (RIGHT, Pad::Right),
    (CIRCLE, Pad::A),
    (CROSS, Pad::B),
    (SQUARE, Pad::X),
    (0x1000, Pad::Y),
    (L, Pad::L),
    (0x200, Pad::R),
    (0x1, Pad::Minus),
    (0x8, Pad::Plus),
];
const DIRECTIONS: &[Pad] = &[
    Pad::Up,
    Pad::Down,
    Pad::Left,
    Pad::Right,
    Pad::LUp,
    Pad::LDown,
    Pad::LLeft,
    Pad::LRight,
];
const ALL: &[Pad] = &[
    Pad::Up,
    Pad::Down,
    Pad::Left,
    Pad::Right,
    Pad::A,
    Pad::B,
    Pad::X,
    Pad::Y,
    Pad::L,
    Pad::R,
    Pad::Minus,
    Pad::Plus,
    Pad::LUp,
    Pad::LDown,
    Pad::LLeft,
    Pad::LRight,
];

#[repr(C)]
#[derive(Default)]
struct CtrlData {
    timestamp: u64,
    buttons: u32,
    lx: u8,
    ly: u8,
    rx: u8,
    ry: u8,
    extras: [u8; 16],
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct TouchReport {
    id: u8,
    force: u8,
    x: i16,
    y: i16,
    reserved: [u8; 8],
    info: u16,
}
#[repr(C)]
#[derive(Default)]
struct TouchData {
    timestamp: u64,
    status: u32,
    count: u32,
    reports: [TouchReport; 8],
}
#[repr(C)]
#[derive(Default)]
struct PanelInfo {
    min_aa_x: i16,
    min_aa_y: i16,
    max_aa_x: i16,
    max_aa_y: i16,
    min_disp_x: i16,
    min_disp_y: i16,
    max_disp_x: i16,
    max_disp_y: i16,
    min_force: u8,
    max_force: u8,
    reserved: [u8; 30],
}

#[cfg(target_os = "vita")]
#[link(name = "SceCtrl_stub", kind = "static")]
unsafe extern "C" {
    fn sceCtrlSetSamplingMode(mode: i32) -> i32;
    fn sceCtrlPeekBufferPositive(port: i32, data: *mut CtrlData, count: i32) -> i32;
}
#[cfg(target_os = "vita")]
#[link(name = "SceTouch_stub", kind = "static")]
unsafe extern "C" {
    fn sceTouchSetSamplingState(port: u32, state: i32) -> i32;
    fn sceTouchGetPanelInfo(port: u32, info: *mut PanelInfo) -> i32;
    fn sceTouchPeek(port: u32, data: *mut TouchData, count: u32) -> i32;
}

pub struct Input {
    held: [bool; 16],
    frames: [u16; 16],
    buttons: u32,
    suppressed: u32,
    shortcut: Shortcut,
    shortcut_enabled: bool,
    font_menu: Option<Size>,
    font_settings: PathBuf,
    touch_id: Option<u8>,
    touch_suppressed: bool,
    panel: PanelInfo,
}
impl Input {
    pub fn new(game: &mut Game, font_settings: PathBuf) -> anyhow::Result<Self> {
        anyhow::ensure!(
            std::mem::size_of::<CtrlData>() == 0x20
                && std::mem::size_of::<TouchData>() == 0x90
                && std::mem::size_of::<PanelInfo>() == 0x30,
            "VitaSDK input ABI mismatch"
        );
        // SAFETY: VitaSDK writes only the SDK-sized repr(C) structures above.
        let mode = unsafe { sceCtrlSetSamplingMode(1) };
        anyhow::ensure!(mode >= 0, "sceCtrlSetSamplingMode: {mode:#x}");
        let touch = unsafe { sceTouchSetSamplingState(0, 1) };
        anyhow::ensure!(touch >= 0, "sceTouchSetSamplingState(front): {touch:#x}");
        let mut panel = PanelInfo::default();
        let status = unsafe { sceTouchGetPanelInfo(0, &mut panel) };
        anyhow::ensure!(
            status >= 0
                && panel.max_disp_x > panel.min_disp_x
                && panel.max_disp_y > panel.min_disp_y,
            "front touch panel info unavailable: {status:#x}"
        );
        match std::fs::read_to_string(&font_settings) {
            Ok(text) => game.set_story_font_size(Size::parse(&text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!("Reading story font setting: {error}"),
        }
        Ok(Self {
            held: [false; 16],
            frames: [0; 16],
            buttons: 0,
            suppressed: 0,
            shortcut: Shortcut::default(),
            shortcut_enabled: false,
            font_menu: None,
            font_settings,
            touch_id: None,
            touch_suppressed: false,
            panel,
        })
    }
    /// Last successful native pad observation, for the optional DEV log.
    pub fn diagnostic_buttons(&self) -> u32 {
        self.buttons
    }

    pub fn font_menu_open(&self) -> bool {
        self.font_menu.is_some()
    }

    pub fn draw_font_menu(&self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        if !self.font_menu_open() {
            return;
        }
        let size = game.story_font_size().percent();
        let left = (CANVAS_W as f32 - 784.0) * 0.5;
        let top = CANVAS_H as f32 - 250.0;
        let mut rect = |x, y, w, h, color| {
            out.push(Quad::rect(r.white.clone(), [left + x, top + y, w, h], color));
        };
        let accent = [0.59, 0.79, 1.0, 1.0];
        rect(0.0, 0.0, 784.0, 222.0, [0.05, 0.07, 0.12, 0.97]);
        rect(0.0, 0.0, 784.0, 3.0, accent);
        rect(36.0, 96.0, 712.0, 5.0, [0.26, 0.32, 0.42, 1.0]);
        let position = f32::from(size - Size::MIN) / f32::from(Size::MAX - Size::MIN);
        if position > 0.0 {
            rect(36.0, 96.0, 712.0 * position, 5.0, accent);
        }
        rect(30.0 + 712.0 * position, 90.0, 12.0, 17.0, accent);

        let font = game.font_mut();
        let value = format!("{size}%");
        let value_width = font.measure_slot(&value, [44.0; 2], 1.0);
        for (text, x, y, px, color) in [
            ("剧情字号", 36.0, 62.0, 42.0, [1.0; 4]),
            (&value, 748.0 - value_width, 62.0, 44.0, accent),
            ("50%", 36.0, 146.0, 29.0, [0.76, 0.81, 0.88, 1.0]),
            ("150%", 670.0, 146.0, 29.0, [0.76, 0.81, 0.88, 1.0]),
        ] {
            font.draw_str_slot(r, out, text, [px; 2], 1.0, [left + x, top + y], color);
        }
        font.draw_str_slot(r, out, "左右键  调整 5%    ○ 保存    × 取消", [33.0; 2], 1.0,
            [left + 36.0, top + 194.0], [1.0; 4]);
    }

    fn font_menu_input(&mut self, game: &mut Game, event: &Option<Event>, pressed: u32) {
        if matches!(event, Some(Event::Toggle)) {
            if let Some(original) = self.font_menu.take() {
                game.set_story_font_size(original);
            } else {
                self.font_menu = Some(game.story_font_size());
            }
        } else if let Some(original) = self.font_menu {
            if pressed & CROSS != 0 {
                self.font_menu = None;
                game.set_story_font_size(original);
            } else if pressed & CIRCLE != 0 {
                self.font_menu = None;
                let size = game.story_font_size();
                if size != original
                    && let Err(error) = std::fs::write(&self.font_settings, format!("{}\n", size.percent()))
                {
                    tracing::warn!("Writing story font setting: {error}");
                }
            } else {
                let direction = i16::from(pressed & RIGHT != 0) - i16::from(pressed & LEFT != 0);
                game.set_story_font_size(game.story_font_size().step(direction));
            }
        }
    }

    pub fn poll(&mut self, game: &mut Game, ui: &mut Ui, render: &Renderer) -> anyhow::Result<()> {
        let mut pad = CtrlData::default();
        // SAFETY: pointers refer to initialized, properly aligned repr(C) buffers; count is one.
        let status = unsafe { sceCtrlPeekBufferPositive(0, &mut pad, 1) };
        crate::vita_watchdog::input(pad.buttons, status);
        anyhow::ensure!(status == 1, "sceCtrlPeekBufferPositive: {status:#x}");
        let previous = self.buttons;
        let pressed = pad.buttons & !previous;
        self.buttons = pad.buttons;
        let was_open = self.font_menu_open();
        // Other screens can use held Square (for example, gallery zoom).
        let shortcut_enabled = (was_open || matches!(ui.screen, Screen::Title | Screen::Game))
            && (self.shortcut_enabled || previous & CHORD == 0);
        let shortcut = if shortcut_enabled {
            self.shortcut.update(pad.buttons)
        } else {
            if self.shortcut_enabled {
                self.suppressed |= pad.buttons & CHORD;
                self.shortcut = Shortcut::default();
            }
            None
        };
        self.shortcut_enabled = shortcut_enabled;
        self.font_menu_input(game, &shortcut, pressed);
        let modal = was_open || self.font_menu_open();
        let previously_suppressed = self.suppressed;
        self.suppressed &= pad.buttons;
        if modal {
            self.suppressed |= pad.buttons;
        }
        let reserved = if shortcut_enabled { CHORD } else { 0 };
        let game_buttons = pad.buttons & !(self.suppressed | reserved);
        let axis = |v: u8| -> i32 { (v as i32 - 128).clamp(-128, 127) };
        let left = if modal { [0; 2] } else { [axis(pad.lx), -axis(pad.ly)] };
        let right = if modal { [0; 2] } else { [axis(pad.rx), -axis(pad.ry)] };
        ui.stick = left;
        ui.rstick = right[1];
        for (i, &button) in ALL.iter().enumerate() {
            let on = BUTTONS
                .iter()
                .find(|(_, b)| *b == button)
                .is_some_and(|(mask, _)| game_buttons & mask != 0)
                || match button {
                    Pad::Up => right[1] > 80,
                    Pad::Down => right[1] < -80,
                    Pad::Left => right[0] < -80,
                    Pad::Right => right[0] > 80,
                    Pad::LUp => left[1] > 80,
                    Pad::LDown => left[1] < -80,
                    Pad::LLeft => left[0] < -80,
                    Pad::LRight => left[0] > 80,
                    _ => false,
                };
            if on != self.held[i] {
                self.held[i] = on;
                self.frames[i] = 0;
                ui.pad(game, render, button, on, false);
            } else if on && DIRECTIONS.contains(&button) {
                self.frames[i] = self.frames[i].saturating_add(1);
                if self.frames[i] >= 15 && (self.frames[i] - 15) % 5 == 0 {
                    ui.pad(game, render, button, true, true);
                }
            }
        }
        if !modal && let Some(Event::Tap(mask)) = shortcut && previously_suppressed & mask == 0 {
            let button = if mask == L { Pad::L } else { Pad::X };
            ui.pad(game, render, button, true, false);
            ui.pad(game, render, button, false, false);
        }
        let mut touch = TouchData::default();
        let status = unsafe { sceTouchPeek(0, &mut touch, 1) };
        anyhow::ensure!(
            status == 1 && touch.count <= 8,
            "sceTouchPeek(front): {status:#x}, reports {}",
            touch.count
        );
        if modal && self.touch_id.take().is_some() {
            ui.cancel_vita_touch();
        }
        if modal || self.touch_suppressed {
            self.touch_suppressed = touch.count > 0;
            return Ok(());
        }
        let report = touch.reports[..touch.count as usize]
            .iter()
            .find(|p| Some(p.id) == self.touch_id)
            .or_else(|| touch.reports[..touch.count as usize].first());
        // The front sensor covers 960x544 display coordinates. Scene content is y=2..541.
        if let Some(report) = report {
            let axis = |v: i16, min: i16, max: i16, pixels: f32| {
                ((v as f32 - min as f32) / (max as f32 - min as f32)).clamp(0.0, 1.0) * pixels
            };
            let x = axis(
                report.x,
                self.panel.min_disp_x,
                self.panel.max_disp_x,
                960.0,
            ) * CANVAS_W as f32
                / 960.0;
            let y = (axis(
                report.y,
                self.panel.min_disp_y,
                self.panel.max_disp_y,
                544.0,
            ) - 2.0)
                .clamp(0.0, 540.0)
                * CANVAS_H as f32
                / 540.0;
            ui.mouse_move(game, x, y);
            if self.touch_id.is_none() {
                ui.mouse_down(game, render);
            }
            self.touch_id = Some(report.id);
        } else if self.touch_id.take().is_some() {
            ui.mouse_up(game, render);
        }
        Ok(())
    }
}

//! VitaSDK controller and front-panel input, polled once per game frame.
use crate::{
    game::Game,
    ui::{Pad, Ui},
};
use knife_render::{CANVAS_H, CANVAS_W, Renderer};

// Buttons follow the Japanese (Asian) PlayStation layout: Circle confirms and advances, Cross
// cancels and goes back (the reverse of the Western convention). The game itself is Switch code
// whose A is "confirm" and B is "cancel", so Pad::A gets SCE_CTRL_CIRCLE and Pad::B SCE_CTRL_CROSS.
const BUTTONS: &[(u32, Pad)] = &[
    (0x10, Pad::Up),
    (0x40, Pad::Down),
    (0x80, Pad::Left),
    (0x20, Pad::Right),
    (0x2000, Pad::A),
    (0x4000, Pad::B),
    (0x8000, Pad::X),
    (0x1000, Pad::Y),
    (0x100, Pad::L),
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
    touch_id: Option<u8>,
    panel: PanelInfo,
}
impl Input {
    pub fn new() -> anyhow::Result<Self> {
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
        Ok(Self {
            held: [false; 16],
            frames: [0; 16],
            buttons: 0,
            touch_id: None,
            panel,
        })
    }
    /// Last successful native pad observation, for the optional DEV log.
    pub fn diagnostic_buttons(&self) -> u32 {
        self.buttons
    }
    pub fn poll(&mut self, game: &mut Game, ui: &mut Ui, render: &Renderer) -> anyhow::Result<()> {
        let mut pad = CtrlData::default();
        // SAFETY: pointers refer to initialized, properly aligned repr(C) buffers; count is one.
        let status = unsafe { sceCtrlPeekBufferPositive(0, &mut pad, 1) };
        crate::vita_watchdog::input(pad.buttons, status);
        anyhow::ensure!(status == 1, "sceCtrlPeekBufferPositive: {status:#x}");
        self.buttons = pad.buttons;
        let axis = |v: u8| -> i32 { (v as i32 - 128).clamp(-128, 127) };
        let left = [axis(pad.lx), -axis(pad.ly)];
        let right = [axis(pad.rx), -axis(pad.ry)];
        ui.stick = left;
        ui.rstick = right[1];
        for (i, &button) in ALL.iter().enumerate() {
            let on = BUTTONS
                .iter()
                .find(|(_, b)| *b == button)
                .is_some_and(|(mask, _)| pad.buttons & mask != 0)
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
        let mut touch = TouchData::default();
        let status = unsafe { sceTouchPeek(0, &mut touch, 1) };
        anyhow::ensure!(
            status == 1 && touch.count <= 8,
            "sceTouchPeek(front): {status:#x}, reports {}",
            touch.count
        );
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

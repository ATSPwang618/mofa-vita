//! Controller/touch translation. Physical sampling is isolated from protocol
//! gestures so the same ordering and inverse coordinates can be tested on PC.
use crate::window::{DISPLAY, Windows};
use krkr_protocol::window::{Input, WindowId};
use std::time::{Duration, Instant};
pub mod bindings;
pub(crate) mod panel;
use bindings::{Bindings, Keys, Output, SELECT, START};

pub const CIRCLE: u32 = 0x2000;
pub const CROSS: u32 = 0x4000;
pub const TRIANGLE: u32 = 0x1000;
#[derive(Clone, Copy)]
pub struct Sample {
    pub buttons: u32,
    pub analog: [u8; 2],
    /// Right-stick vertical axis, translated to mouse-wheel not arrow keys.
    pub scroll: u8,
    /// Centroid of two front-panel contacts; one finger remains a mouse drag.
    pub scroll_touch: Option<(i32, i32)>,
    /// Front panel contact identity and display-space coordinates.
    pub touch: Option<(u8, (i32, i32))>,
}
impl Default for Sample {
    fn default() -> Self {
        Self {
            buttons: 0,
            analog: [128, 128],
            scroll: 128,
            scroll_touch: None,
            touch: None,
        }
    }
}
pub struct State {
    pointer_speed: f32,
    pointer_until: Option<Instant>,
    focused: Option<WindowId>,
    position: [f32; 2],
    last: Instant,
    buttons: u32,
    keys: Keys,
    bindings: Bindings,
    directory: Option<std::path::PathBuf>,
    language: crate::launcher::Language,
    panel_active: bool,
    select_since: Option<(Instant, bool)>,
    physical: u32,
    pending: u32,
    pending_until: Option<Instant>,
    used_chords: u32,
    ambiguous: u32,
    mapped: Output,
    pulse: Output,
    pulse_until: Option<Instant>,
    wheel: i32,
    wheel_repeat: Option<Instant>,
    scroll_blocked: bool,
    press_origin: Option<(i32, i32)>,
    dragged: bool,
    touch_scrolling: bool,
    scroll_origin: Option<i32>,
    caps: bool,
    blocked: u32,
    blocked_touch: Option<u8>,
    click: Option<(Instant, (i32, i32))>,
    double: bool,
    repeat: Option<(u32, Instant)>,
}
impl State {
    pub fn new(now: Instant) -> Self {
        Self {
            pointer_speed: 520.,
            pointer_until: None,
            focused: None,
            position: [480., 272.],
            last: now,
            buttons: 0,
            keys: Keys::default(),
            bindings: Bindings::default(),
            directory: None,
            language: crate::launcher::Language::default(),
            panel_active: false,
            select_since: None,
            physical: 0,
            pending: 0,
            pending_until: None,
            used_chords: 0,
            ambiguous: 0,
            mapped: Output::default(),
            pulse: Output::default(),
            pulse_until: None,
            wheel: 0,
            wheel_repeat: None,
            scroll_blocked: false,
            press_origin: None,
            dragged: false,
            touch_scrolling: false,
            scroll_origin: None,
            caps: false,
            blocked: 0,
            blocked_touch: None,
            click: None,
            double: false,
            repeat: None,
        }
    }
    pub fn set_pointer_speed(&mut self, speed: f32) {
        if speed.is_finite() {
            self.pointer_speed = speed.clamp(100., 1600.);
        }
    }
    pub fn configure(&mut self, directory: &std::path::Path, language: crate::launcher::Language) {
        self.directory = Some(directory.into());
        self.language = language;
        match Bindings::load(directory) {
            Ok(bindings) => self.set_bindings(bindings),
            Err(error) => {
                self.set_bindings(Bindings::default());
                krkr_protocol::diagnostic!(
                    "[VITA][INPUT] settings: {error}; using default controls"
                );
            }
        }
    }
    pub fn set_bindings(&mut self, bindings: Bindings) {
        self.ambiguous = bindings.ambiguous_buttons();
        self.bindings = bindings;
        self.physical = 0;
        self.mapped = Output::default();
        self.pending = 0;
        self.pending_until = None;
        self.used_chords = 0;
    }
    pub fn poll(
        &mut self,
        sample: Sample,
        now: Instant,
        windows: &mut Windows,
    ) -> Result<(), String> {
        let dt = now
            .saturating_duration_since(self.last)
            .min(Duration::from_millis(50))
            .as_secs_f32();
        self.last = now;
        let focused = windows.focused();
        if focused != self.focused {
            // Modal changes cancel an in-progress press. Held physical inputs
            // must be released before they can act in the next window.
            self.blocked = sample.buttons;
            self.blocked_touch = sample.touch.map(|touch| touch.0);
            self.buttons = 0;
            self.keys = Keys::default();
            self.click = None;
            self.double = false;
            self.repeat = None;
            self.focused = focused;
            self.reset_mapping();
            self.select_since = None;
            self.scroll_blocked = axis(sample.scroll) != 0.;
            self.press_origin = None;
            self.dragged = false;
            self.touch_scrolling = false;
            self.scroll_origin = None;
            windows.close_input_panel();
            self.panel_active = false;
        }
        self.blocked &= sample.buttons;
        self.scroll_blocked &= axis(sample.scroll) != 0.;
        if sample.touch.map(|touch| touch.0) != self.blocked_touch {
            self.blocked_touch = None;
        }
        let Some(id) = focused else {
            return Ok(());
        };
        let Some(mapping) = windows.mapping(id) else {
            return Ok(());
        };
        if self.panel_active && windows.input_panel.is_none() {
            self.cancel(sample, id, windows)?;
            self.panel_active = false;
        }
        if let Some(panel) = &mut windows.input_panel {
            let filtered = Sample {
                buttons: sample.buttons & !self.blocked,
                touch: sample.touch.filter(|t| Some(t.0) != self.blocked_touch),
                ..sample
            };
            let event = panel.poll(filtered, now);
            if event.save {
                let bindings = panel.bindings.clone();
                let saved = bindings.validate().and_then(|()| {
                    self.directory
                        .as_ref()
                        .map_or(Ok(()), |path| bindings.save(path))
                });
                match saved {
                    Ok(()) => {
                        panel.saved();
                        self.set_bindings(bindings);
                    }
                    Err(error) => panel.error(error),
                }
            }
            self.send_keys(event.output.keys, now, id, windows)?;
            if event.close {
                self.cancel(sample, id, windows)?;
                windows.close_input_panel();
                self.panel_active = false;
            }
            return Ok(());
        }
        if sample.buttons & (START | SELECT) == START | SELECT {
            self.cancel(sample, id, windows)?;
            return Ok(());
        }
        let physical_buttons = sample.buttons & !self.blocked;
        if physical_buttons & SELECT != 0 {
            let (started, used) = self.select_since.get_or_insert((now, false));
            *used |= physical_buttons != SELECT;
            if !*used && now.saturating_duration_since(*started) >= Duration::from_millis(350) {
                self.cancel(sample, id, windows)?;
                windows.open_input_panel(panel::Panel::new(self.bindings.clone(), self.language));
                self.panel_active = true;
                return Ok(());
            }
        } else if let Some((_, used)) = self.select_since.take()
            && !used
            && let Some(entry) = self.bindings.entries.iter().find(|e| e.buttons == SELECT)
        {
            self.pulse.add(entry.action);
            self.pulse_until = Some(now + Duration::from_millis(60));
        }
        let mut touch_wheel = 0;
        if let Some((x, y)) = sample.scroll_touch {
            if !self.touch_scrolling {
                // A second contact turns the mouse gesture into scrolling.
                // Release capture without clicking, including when the first
                // finger has already pressed a scrollbar or a game button.
                self.cancel(sample, id, windows)?;
                self.touch_scrolling = true;
                self.scroll_origin = Some(y);
            } else if let Some(origin) = self.scroll_origin {
                let steps = (y - origin) / 18;
                if steps != 0 {
                    touch_wheel = steps.clamp(-4, 4) * 120;
                    self.scroll_origin = Some(origin + steps * 18);
                }
            }
            self.position = [x as f32, y as f32];
            windows.set_pointer((x, y));
            self.pointer_until = None;
        } else if sample.touch.is_none() {
            self.touch_scrolling = false;
            self.scroll_origin = None;
        } else if self.touch_scrolling {
            // Require a fresh two-contact origin after one finger lifts.
            self.scroll_origin = None;
        }
        if sample.scroll_touch.is_some() && self.scroll_origin.is_none() {
            self.scroll_origin = sample.scroll_touch.map(|p| p.1);
        }
        let physical = windows.pointer();
        if physical
            != (
                self.position[0].round() as i32,
                self.position[1].round() as i32,
            )
        {
            self.position = [physical.0 as f32, physical.1 as f32];
        }
        let touch = sample
            .touch
            .filter(|touch| !self.touch_scrolling && Some(touch.0) != self.blocked_touch);
        if touch.is_some() {
            self.pointer_until = None;
        } else if sample.analog.into_iter().any(|value| axis(value) != 0.) {
            // Buttons still click at the pointer position while it is hidden.
            // Only deliberate pointer movement should reveal it again.
            self.pointer_until = Some(now + Duration::from_secs(2));
        }
        windows.show_pointer(self.pointer_until.is_some_and(|until| until > now));
        let old = (
            self.position[0].round() as i32,
            self.position[1].round() as i32,
        );
        if let Some((_, point)) = touch {
            self.position = [point.0 as f32, point.1 as f32];
        } else {
            for (index, limit) in [DISPLAY.width, DISPLAY.height].into_iter().enumerate() {
                self.position[index] = (self.position[index]
                    + axis(sample.analog[index]) * self.pointer_speed * dt)
                    .clamp(0., limit.saturating_sub(1) as f32);
            }
        }
        let point = (
            self.position[0].round() as i32,
            self.position[1].round() as i32,
        );
        windows.set_pointer(point);
        let (x, y) = mapping.input(point);
        let inside = x >= 0
            && y >= 0
            && (x as u32) < mapping.logical.width
            && (y as u32) < mapping.logical.height;
        if !inside && self.buttons == 0 {
            self.blocked |= sample.buttons & self.bindings.mouse_buttons();
            if let Some(touch) = touch {
                self.blocked_touch = Some(touch.0);
            }
        }
        let physical_buttons = sample.buttons & !self.blocked;
        let mut output = self.mapped_output(physical_buttons, now);
        if !self.scroll_blocked && axis(sample.scroll) != 0. {
            output.wheel = if sample.scroll < 128 { 120 } else { -120 };
        }
        let modifiers = output.keys.modifiers();
        let next_buttons = output.mouse
            | (u32::from(touch.is_some_and(|touch| Some(touch.0) != self.blocked_touch)) * 8);
        if old != point {
            if self.buttons & 8 != 0
                && self
                    .press_origin
                    .is_some_and(|p| p.0.abs_diff(point.0) > 4 || p.1.abs_diff(point.1) > 4)
            {
                self.dragged = true;
                self.click = None;
            }
            windows.post(
                id,
                Input::MouseMove {
                    x,
                    y,
                    shift: self.buttons | modifiers,
                },
            )?;
        }
        self.send_keys(output.keys, now, id, windows)?;
        if touch_wheel != 0 {
            windows.post(
                id,
                Input::Wheel {
                    x,
                    y,
                    shift: modifiers,
                    delta: touch_wheel,
                },
            )?;
        }
        if output.wheel != 0
            && (output.wheel != self.wheel || self.wheel_repeat.is_some_and(|due| now >= due))
        {
            windows.post(
                id,
                Input::Wheel {
                    x,
                    y,
                    shift: modifiers | self.buttons,
                    delta: output.wheel,
                },
            )?;
            self.wheel_repeat = Some(
                now + Duration::from_millis(if output.wheel != self.wheel { 400 } else { 80 }),
            );
        }
        self.wheel = output.wheel;
        if self.wheel == 0 {
            self.wheel_repeat = None;
        }
        for (button, mask) in [(0, 8), (1, 16)] {
            let pressed = next_buttons & mask != 0;
            if pressed == (self.buttons & mask != 0) {
                continue;
            }
            if pressed {
                self.buttons |= mask;
            } else {
                self.buttons &= !mask;
            }
            let shift = modifiers | self.buttons;
            if pressed {
                if button == 0 {
                    self.press_origin = Some(point);
                    self.dragged = false;
                    self.double = self.click.is_some_and(|(time, p)| {
                        now.saturating_duration_since(time) <= Duration::from_millis(500)
                            && x.abs_diff(p.0) <= 4
                            && y.abs_diff(p.1) <= 4
                    });
                    if self.double {
                        self.click = None;
                        windows.post(id, Input::DoubleClick { x, y })?;
                    } else {
                        self.click = Some((now, (x, y)));
                    }
                }
                windows.post(
                    id,
                    Input::MouseDown {
                        x,
                        y,
                        button,
                        shift,
                    },
                )?;
            } else {
                // Legacy click precedes mouse-up, matching the desktop host.
                if button == 0
                    && !self.double
                    && !self.dragged
                    && inside
                    && self.used_chords & self.bindings.mouse_buttons() == 0
                {
                    windows.post(id, Input::Click { x, y })?;
                }
                windows.post(
                    id,
                    Input::MouseUp {
                        x,
                        y,
                        button,
                        shift,
                    },
                )?;
                if button == 0 {
                    self.press_origin = None;
                }
            }
        }
        Ok(())
    }
    fn reset_mapping(&mut self) {
        self.physical = 0;
        self.pending = 0;
        self.pending_until = None;
        self.used_chords = 0;
        self.mapped = Output::default();
        self.pulse = Output::default();
        self.pulse_until = None;
        self.wheel = 0;
        self.wheel_repeat = None;
        self.press_origin = None;
        self.dragged = false;
    }
    fn cancel(
        &mut self,
        sample: Sample,
        id: WindowId,
        windows: &mut Windows,
    ) -> Result<(), String> {
        self.send_keys(Keys::default(), Instant::now(), id, windows)?;
        let (x, y) = windows.mapping(id).unwrap().input(windows.pointer());
        for (button, mask) in [(0, 8), (1, 16)] {
            if self.buttons & mask != 0 {
                windows.post(
                    id,
                    Input::MouseUp {
                        x,
                        y,
                        button,
                        shift: 0,
                    },
                )?;
            }
        }
        self.buttons = 0;
        self.blocked = sample.buttons;
        self.blocked_touch = sample.touch.map(|t| t.0);
        self.click = None;
        self.double = false;
        self.repeat = None;
        self.select_since = None;
        self.pointer_until = None;
        windows.show_pointer(false);
        self.reset_mapping();
        Ok(())
    }
    fn mapped_output(&mut self, physical: u32, now: Instant) -> Output {
        if self.pulse_until.is_some_and(|due| now >= due) {
            self.pulse = Output::default();
            self.pulse_until = None;
        }
        let old_pending = self.pending;
        let released_pending = self.pending & !physical & !self.used_chords;
        if released_pending != 0 {
            for entry in &self.bindings.entries {
                if entry.buttons.count_ones() == 1
                    && entry.buttons != SELECT
                    && entry.buttons & released_pending != 0
                {
                    self.pulse.add(entry.action);
                }
            }
            self.pulse_until = Some(now + Duration::from_millis(60));
        }
        self.pending &= physical;
        let new_pending = physical & !self.physical & self.ambiguous;
        self.pending |= new_pending;
        if new_pending != 0 && self.pending_until.is_none() {
            self.pending_until = Some(now + Duration::from_millis(60));
        }
        if self.pending_until.is_some_and(|due| now >= due) {
            self.pending = 0;
            self.pending_until = None;
        }
        if physical != self.physical || old_pending != self.pending {
            self.used_chords &= physical;
            let (mapped, chords) = self.bindings.resolve(
                physical,
                physical & !self.pending & !self.used_chords & !SELECT,
            );
            self.mapped = mapped;
            self.used_chords |= chords;
            self.physical = physical;
        }
        let mut output = self.mapped;
        output.merge(self.pulse);
        output
    }
    fn send_keys(
        &mut self,
        next: Keys,
        now: Instant,
        id: WindowId,
        windows: &Windows,
    ) -> Result<(), String> {
        let shift = next.modifiers() | self.buttons;
        if next != self.keys {
            // Release ordinary keys before modifiers; press modifiers first.
            for modifier in [false, true] {
                for key in 8u16..=254 {
                    if (16..=18).contains(&key) != modifier
                        || !self.keys.contains(key)
                        || next.contains(key)
                    {
                        continue;
                    }
                    windows.post(
                        id,
                        Input::KeyUp {
                            key: u32::from(key),
                            shift,
                        },
                    )?;
                }
            }
            for modifier in [true, false] {
                for key in 8u16..=254 {
                    if (16..=18).contains(&key) != modifier
                        || self.keys.contains(key)
                        || !next.contains(key)
                    {
                        continue;
                    }
                    windows.post(
                        id,
                        Input::KeyDown {
                            key: u32::from(key),
                            shift,
                        },
                    )?;
                    if key == 20 {
                        self.caps = !self.caps;
                    }
                    if !modifier && key != 20 {
                        if let Some(unit) = key_character(key, next.modifiers(), self.caps) {
                            windows.post(id, Input::KeyPress(unit))?;
                        }
                        self.repeat = Some((u32::from(key), now + Duration::from_millis(400)));
                    }
                }
            }
            self.keys = next;
            if self
                .repeat
                .is_some_and(|(key, _)| !next.contains(key as u16))
            {
                self.repeat = None;
            }
        }
        if let Some((key, due)) = self.repeat
            && now >= due
        {
            windows.post(
                id,
                Input::KeyDown {
                    key,
                    shift: shift | 128,
                },
            )?;
            if let Some(unit) = key_character(key as u16, next.modifiers(), self.caps) {
                windows.post(id, Input::KeyPress(unit))?;
            }
            self.repeat = Some((key, now + Duration::from_millis(80)));
        }
        Ok(())
    }
}
fn key_character(key: u16, modifiers: u32, caps: bool) -> Option<u16> {
    if modifiers & 6 != 0 {
        return None;
    }
    let shift = modifiers & 1 != 0;
    Some(match key {
        8 | 9 | 13 | 32 => key,
        65..=90 => {
            if shift ^ caps {
                key
            } else {
                key + 32
            }
        }
        48..=57 => {
            if shift {
                b")!@#$%^&*("[(key - 48) as usize] as u16
            } else {
                key
            }
        }
        _ => {
            let pair = match key {
                186 => b";:",
                187 => b"=+",
                188 => b",<",
                189 => b"-_",
                190 => b".>",
                191 => b"/?",
                192 => b"`~",
                219 => b"[{",
                220 => b"\\|",
                221 => b"]}",
                222 => b"'\"",
                _ => return None,
            };
            pair[usize::from(shift)] as u16
        }
    })
}
fn axis(value: u8) -> f32 {
    let value = f32::from(value) - 128.;
    if value.abs() <= 24. {
        0.
    } else {
        value.signum() * ((value.abs() - 24.) / 103.).min(1.)
    }
}

#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
pub mod native {
    use super::*;
    use vitasdk_sys::*;
    pub struct Controller {
        panel: SceTouchPanelInfo,
        contact: Option<u8>,
    }
    impl Controller {
        pub fn new() -> Result<Self, String> {
            unsafe {
                let result = sceCtrlSetSamplingMode(SCE_CTRL_MODE_ANALOG);
                if result < 0 {
                    return Err(format!("controller sampling: {result:#x}"));
                }
                let result =
                    sceTouchSetSamplingState(SCE_TOUCH_PORT_FRONT, SCE_TOUCH_SAMPLING_STATE_START);
                if result < 0 {
                    return Err(format!("touch sampling: {result:#x}"));
                }
                let mut panel = std::mem::zeroed();
                let result = sceTouchGetPanelInfo(SCE_TOUCH_PORT_FRONT, &mut panel);
                if result < 0 {
                    return Err(format!("touch panel: {result:#x}"));
                }
                Ok(Self {
                    panel,
                    contact: None,
                })
            }
        }
        pub fn sample(&mut self) -> Result<Sample, String> {
            unsafe {
                let mut pad: SceCtrlData = std::mem::zeroed();
                let mut touch: SceTouchData = std::mem::zeroed();
                let result = sceCtrlPeekBufferPositive(0, &mut pad, 1);
                if result < 0 {
                    return Err(format!("controller read: {result:#x}"));
                }
                let result = sceTouchPeek(SCE_TOUCH_PORT_FRONT, &mut touch, 1);
                if result < 0 {
                    return Err(format!("touch read: {result:#x}"));
                }
                let reports = &touch.report[..(touch.reportNum as usize).min(touch.report.len())];
                let report = reports
                    .iter()
                    .find(|report| Some(report.id) == self.contact)
                    .or_else(|| reports.first());
                self.contact = report.map(|report| report.id);
                let map = |value: i16, min: i16, max: i16, extent: u32| {
                    ((i64::from(value) - i64::from(min)) * i64::from(extent - 1)
                        / (i64::from(max) - i64::from(min)).max(1))
                    .clamp(0, i64::from(extent - 1)) as i32
                };
                Ok(Sample {
                    buttons: pad.buttons,
                    analog: [pad.lx, pad.ly],
                    scroll: pad.ry,
                    scroll_touch: (reports.len() >= 2).then(|| {
                        let x = (i32::from(reports[0].x) + i32::from(reports[1].x)) / 2;
                        let y = (i32::from(reports[0].y) + i32::from(reports[1].y)) / 2;
                        (
                            map(
                                x as i16,
                                self.panel.minDispX,
                                self.panel.maxDispX,
                                DISPLAY.width,
                            ),
                            map(
                                y as i16,
                                self.panel.minDispY,
                                self.panel.maxDispY,
                                DISPLAY.height,
                            ),
                        )
                    }),
                    touch: report.map(|r| {
                        (
                            r.id,
                            (
                                map(r.x, self.panel.minDispX, self.panel.maxDispX, DISPLAY.width),
                                map(
                                    r.y,
                                    self.panel.minDispY,
                                    self.panel.maxDispY,
                                    DISPLAY.height,
                                ),
                            ),
                        )
                    }),
                })
            }
        }
    }
    impl Drop for Controller {
        fn drop(&mut self) {
            unsafe {
                sceTouchSetSamplingState(SCE_TOUCH_PORT_FRONT, SCE_TOUCH_SAMPLING_STATE_STOP);
            }
        }
    }
}

//! On-demand game keyboard and mapping editor. Hidden panels own no GPU image.
use super::{Sample, bindings::*};
use crate::launcher::Language;
use krkr_protocol::{
    graphics::{Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render_gles2::{Gpu, Image};
use std::time::{Duration, Instant};
mod paint;

pub const SIZE: Size = Size {
    width: 960,
    height: 344,
};
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Keyboard,
    Mappings,
    Capture,
    Edit,
}
#[derive(Clone)]
struct Key {
    rectangle: Rect,
    code: u16,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Widget {
    pub rect: Rect,
    pub label: String,
    pub style: u8,
    pub size: u8,
}
impl Widget {
    fn new(rect: Rect, label: impl Into<String>, style: u8, size: u8) -> Self {
        Self {
            rect,
            label: label.into(),
            style,
            size,
        }
    }
}
#[derive(Default)]
pub struct Event {
    pub close: bool,
    pub save: bool,
    pub output: Output,
}
pub struct Panel {
    pub bindings: Bindings,
    language: Language,
    mode: Mode,
    keys: Vec<Key>,
    selected: usize,
    row: usize,
    modifiers: u8,
    caps: bool,
    held: Option<u16>,
    held_touch: Option<u8>,
    previous: Sample,
    navigate: Option<(u32, Instant)>,
    editing: Option<usize>,
    trigger: u32,
    action: Action,
    capture: u32,
    capture_ready: bool,
    message: String,
    dirty: bool,
    surface: Option<Surface>,
}
impl Panel {
    pub fn new(bindings: Bindings, language: Language) -> Self {
        Self {
            bindings,
            language,
            mode: Mode::Keyboard,
            keys: layout(),
            selected: 0,
            row: 0,
            modifiers: 0,
            caps: false,
            held: None,
            held_touch: None,
            previous: Sample::default(),
            navigate: None,
            editing: None,
            trigger: 0,
            action: Action::key(13),
            capture: 0,
            capture_ready: false,
            message: String::new(),
            dirty: true,
            surface: None,
        }
    }
    pub fn error(&mut self, message: String) {
        self.message = message;
        self.dirty = true;
    }
    pub fn saved(&mut self) {
        self.message = self
            .tr(
                "Saved for this game",
                "已为当前游戏保存",
                "ゲームごとに保存しました",
            )
            .into();
        self.dirty = true;
    }
    fn tr<'a>(&self, en: &'a str, zh: &'a str, ja: &'a str) -> &'a str {
        match self.language {
            Language::English => en,
            Language::Chinese => zh,
            Language::Japanese => ja,
        }
    }
    fn top(&self) -> i32 {
        if self.bindings.top {
            0
        } else {
            544 - SIZE.height as i32
        }
    }
    fn key_at(&self, point: (i32, i32)) -> Option<usize> {
        self.keys.iter().position(|k| inside(k.rectangle, point))
    }
    pub fn poll(&mut self, sample: Sample, now: Instant) -> Event {
        let mut event = Event::default();
        let pressed = sample.buttons & !self.previous.buttons;
        let new_touch = sample
            .touch
            .filter(|(id, _)| self.previous.touch.is_none_or(|(old, _)| old != *id));
        let tap = new_touch.map(|(_, (x, y))| (x, y - self.top()));
        if self.mode == Mode::Capture {
            if !self.capture_ready {
                self.capture_ready = sample.buttons == 0;
            } else {
                self.capture |= sample.buttons & VALID;
                if self.capture != 0 && sample.buttons == 0 {
                    let candidate = Binding {
                        buttons: self.capture,
                        action: self.action,
                    };
                    let mut check = self.bindings.clone();
                    if let Some(index) = self.editing {
                        check.entries.remove(index);
                    }
                    check.entries.push(candidate);
                    match check.validate() {
                        Ok(()) => {
                            self.trigger = self.capture;
                            self.mode = Mode::Edit;
                            self.message.clear();
                        }
                        Err(error) => {
                            self.message = error;
                            self.capture = 0;
                        }
                    }
                    self.dirty = true;
                }
            }
            if tap.is_some_and(|p| {
                inside(
                    Rect {
                        left: 820,
                        top: 4,
                        width: 128,
                        height: 30,
                    },
                    p,
                )
            }) {
                self.mode = Mode::Mappings;
                self.dirty = true;
            }
            self.previous = sample;
            return event;
        }
        let back = pressed & CROSS != 0
            || pressed & SELECT != 0
            || tap.is_some_and(|p| p.0 >= 820 && (0..36).contains(&p.1));
        if back {
            if self.mode == Mode::Edit {
                self.mode = Mode::Mappings;
                self.modifiers = 0;
                self.dirty = true;
            } else {
                event.close = true;
            }
        } else if pressed & SQUARE != 0
            || tap.is_some_and(|p| (0..36).contains(&p.1) && (12..278).contains(&p.0))
        {
            self.mode = if let Some((x, _)) = tap {
                if x < 142 {
                    Mode::Keyboard
                } else {
                    Mode::Mappings
                }
            } else if self.mode == Mode::Keyboard {
                Mode::Mappings
            } else {
                Mode::Keyboard
            };
            self.modifiers = 0;
            self.held = None;
            self.held_touch = None;
            self.message.clear();
            self.dirty = true;
        } else if pressed & (L | R) != 0
            || tap.is_some_and(|p| (674..810).contains(&p.0) && (0..36).contains(&p.1))
        {
            self.bindings.top = !self.bindings.top;
            self.dirty = true;
            event.save = true;
        } else {
            let direction = sample.buttons & (UP | DOWN | LEFT | RIGHT);
            let step = direction != 0
                && (direction != self.previous.buttons & (UP | DOWN | LEFT | RIGHT)
                    || self
                        .navigate
                        .is_some_and(|(key, due)| key == direction && now >= due));
            if direction == 0 {
                self.navigate = None;
            }
            if step {
                let repeated = self.navigate.is_some_and(|(key, _)| key == direction);
                self.navigate = Some((
                    direction,
                    now + Duration::from_millis(if repeated { 100 } else { 350 }),
                ));
                if self.mode == Mode::Mappings {
                    let count = self.bindings.entries.len() + 2;
                    self.row = if direction & (UP | LEFT) != 0 {
                        (self.row + count - 1) % count
                    } else {
                        (self.row + 1) % count
                    };
                } else {
                    self.navigate_key(direction);
                }
                self.dirty = true;
            }
            match self.mode {
                Mode::Mappings => self.mappings_input(pressed, tap, &mut event),
                Mode::Keyboard | Mode::Edit => {
                    if self
                        .held_touch
                        .is_some_and(|id| sample.touch.is_none_or(|(current, _)| current != id))
                        || (self.held_touch.is_none() && sample.buttons & CIRCLE == 0)
                    {
                        if self.held.take().is_some() {
                            self.dirty = true;
                        }
                        self.held_touch = None;
                    }
                    let hit = tap.and_then(|p| self.key_at(p));
                    if let Some(index) = hit {
                        self.selected = index;
                        self.dirty = true;
                    }
                    if hit.is_some() || pressed & CIRCLE != 0 {
                        let key = self.keys[self.selected].code;
                        if let Some(flag) = modifier(key) {
                            self.modifiers ^= flag;
                            if self.mode == Mode::Edit {
                                self.update_action_modifiers();
                            }
                        } else if self.mode == Mode::Edit {
                            self.action = Action::Key {
                                key,
                                modifiers: self.modifiers,
                            };
                        } else {
                            if key == 20 {
                                self.caps = !self.caps;
                            }
                            self.held = Some(key);
                            self.held_touch = new_touch.map(|(id, _)| id);
                        }
                        self.dirty = true;
                    }
                    if self.mode == Mode::Edit {
                        if pressed & TRIANGLE != 0 || tap.is_some_and(|p| p.1 >= 302 && p.0 >= 760)
                        {
                            let entry = Binding {
                                buttons: self.trigger,
                                action: self.action,
                            };
                            if let Some(index) = self.editing {
                                self.bindings.entries[index] = entry;
                            } else {
                                self.bindings.entries.push(entry);
                            }
                            self.mode = Mode::Mappings;
                            self.modifiers = 0;
                            self.dirty = true;
                            event.save = true;
                        } else if let Some((x, y)) = tap.filter(|p| p.1 >= 302 && p.1 < 340) {
                            match x {
                                12..=147 => {
                                    self.mode = Mode::Capture;
                                    self.capture = 0;
                                    self.capture_ready = false;
                                }
                                154..=281 => self.action = Action::Mouse(0),
                                288..=415 => self.action = Action::Mouse(1),
                                422..=549 => self.action = Action::Wheel(120),
                                556..=683 => self.action = Action::Wheel(-120),
                                _ => {}
                            }
                            let _ = y;
                            self.dirty = true;
                        }
                    }
                }
                Mode::Capture => {}
            }
        }
        if self.mode == Mode::Keyboard && !event.close {
            event.output.keys.add_modifiers(self.modifiers);
            if let Some(key) = self.held {
                event.output.keys.set(key);
            }
        }
        self.previous = sample;
        event
    }
    fn update_action_modifiers(&mut self) {
        if let Action::Key { modifiers, .. } = &mut self.action {
            *modifiers = self.modifiers;
        }
    }
    fn mappings_input(&mut self, pressed: u32, tap: Option<(i32, i32)>, event: &mut Event) {
        let count = self.bindings.entries.len();
        let start = self
            .row
            .saturating_sub(4)
            .min((count + 2).saturating_sub(6));
        let mut activate = pressed & CIRCLE != 0;
        if let Some((_, y)) = tap.filter(|&(x, y)| (12..948).contains(&x) && (44..290).contains(&y))
        {
            let row = start + ((y - 44) / 41) as usize;
            if row < count + 2 {
                self.row = row;
                activate = true;
                self.dirty = true;
            }
        }
        if pressed & TRIANGLE != 0 && self.row < count {
            self.bindings.entries.remove(self.row);
            self.row = self.row.min(self.bindings.entries.len() + 1);
            event.save = true;
            self.dirty = true;
        } else if activate {
            self.editing = (self.row < count).then_some(self.row);
            if self.row == count + 1 {
                self.bindings = Bindings::default();
                self.row = 0;
                event.save = true;
            } else if let Some(index) = self.editing {
                self.trigger = self.bindings.entries[index].buttons;
                self.action = self.bindings.entries[index].action;
                self.modifiers = match self.action {
                    Action::Key { modifiers, .. } => modifiers,
                    _ => 0,
                };
                self.mode = Mode::Edit;
            } else if count < MAX_BINDINGS {
                self.trigger = 0;
                self.action = Action::key(13);
                self.modifiers = 0;
                self.capture = 0;
                self.capture_ready = false;
                self.mode = Mode::Capture;
            } else {
                self.message = "Maximum 32 bindings".into();
            }
            self.dirty = true;
        }
    }
    fn navigate_key(&mut self, direction: u32) {
        let here = self.keys[self.selected].rectangle;
        let (x, y) = center(here);
        let next = self
            .keys
            .iter()
            .enumerate()
            .filter_map(|(i, k)| {
                let (kx, ky) = center(k.rectangle);
                let dx = kx - x;
                let dy = ky - y;
                let valid = match direction {
                    UP => dy < -8,
                    DOWN => dy > 8,
                    LEFT => dx < -8 && dy.abs() < 8,
                    RIGHT => dx > 8 && dy.abs() < 8,
                    _ => false,
                };
                valid.then_some((
                    if direction & (UP | DOWN) != 0 {
                        dy.abs() * 1000 + dx.abs()
                    } else {
                        dx.abs()
                    },
                    i,
                ))
            })
            .min();
        if let Some((_, index)) = next {
            self.selected = index;
        }
    }
    fn widgets(&self) -> Vec<Widget> {
        let rect = |x, y, w, h| Rect {
            left: x,
            top: y,
            width: w,
            height: h,
        };
        let mut out = vec![
            Widget::new(
                rect(12, 4, 126, 30),
                self.tr("Keyboard", "虚拟键盘", "キーボード"),
                u8::from(self.mode == Mode::Keyboard),
                18,
            ),
            Widget::new(
                rect(146, 4, 132, 30),
                self.tr("Mapping", "按键映射", "キー設定"),
                u8::from(self.mode != Mode::Keyboard),
                18,
            ),
            Widget::new(
                rect(674, 4, 136, 30),
                if self.bindings.top {
                    self.tr("Move down", "下移", "下へ")
                } else {
                    self.tr("Move up", "上移", "上へ")
                },
                0,
                18,
            ),
            Widget::new(
                rect(820, 4, 128, 30),
                self.tr("Close ×", "关闭 ×", "閉じる ×"),
                0,
                18,
            ),
        ];
        if self.mode == Mode::Capture {
            out.push(Widget::new(
                rect(24, 92, 912, 54),
                self.tr(
                    "Press a controller button or chord, then release",
                    "按住需要映射的手柄按键或组合，然后松开",
                    "割り当てるボタンを押してから離してください",
                ),
                4,
                24,
            ));
            out.push(Widget::new(
                rect(24, 160, 912, 48),
                button_label(self.capture),
                4,
                24,
            ));
            out.push(Widget::new(rect(24, 250, 912, 56), &self.message, 4, 18));
            return out;
        }
        if self.mode == Mode::Mappings {
            let count = self.bindings.entries.len() + 2;
            let start = self.row.saturating_sub(4).min(count.saturating_sub(6));
            for row in start..(start + 6).min(count) {
                let label = if let Some(entry) = self.bindings.entries.get(row) {
                    format!(
                        "{}    →    {}",
                        button_label(entry.buttons),
                        entry.action.label()
                    )
                } else if row == count - 2 {
                    self.tr("+ Add binding", "+ 添加映射", "+ 追加").into()
                } else {
                    self.tr("Restore defaults", "恢复默认方案", "初期設定に戻す")
                        .into()
                };
                out.push(Widget::new(
                    rect(12, 44 + ((row - start) * 41) as i32, 936, 37),
                    label,
                    u8::from(row == self.row),
                    21,
                ));
            }
            out.push(Widget::new(
                rect(12, 302, 936, 34),
                if self.message.is_empty() {
                    self.tr(
                        "○ Edit   △ Remove   □ Keyboard   L/R Dock",
                        "○ 编辑   △ 删除   □ 键盘   L/R 上下移动",
                        "○ 編集   △ 削除   □ キーボード   L/R 移動",
                    )
                } else {
                    &self.message
                },
                4,
                18,
            ));
            return out;
        }
        for (index, key) in self.keys.iter().enumerate() {
            let active = modifier(key.code).is_some_and(|flag| self.modifiers & flag != 0)
                || (key.code == 20 && self.caps && self.mode == Mode::Keyboard);
            let style = if self.held == Some(key.code) {
                3
            } else if index == self.selected {
                1
            } else if active {
                2
            } else {
                0
            };
            out.push(Widget::new(
                key.rectangle,
                key_label(key.code),
                style,
                if key.rectangle.width < 52 { 16 } else { 18 },
            ));
        }
        if self.mode == Mode::Edit {
            out.push(Widget::new(
                rect(288, 4, 376, 30),
                format!("{} → {}", button_label(self.trigger), self.action.label()),
                4,
                16,
            ));
            for (x, w, text) in [
                (12, 136, self.tr("Record chord", "重录组合", "ボタン登録")),
                (154, 128, "Mouse L"),
                (288, 128, "Mouse R"),
                (422, 128, "Wheel ↑"),
                (556, 128, "Wheel ↓"),
                (760, 188, self.tr("△ Save", "△ 保存", "△ 保存")),
            ] {
                out.push(Widget::new(rect(x, 302, w, 34), text, 0, 17));
            }
        } else {
            out.push(Widget::new(
                rect(12, 302, 936, 34),
                self.tr(
                    "○ Hold key   Ctrl / Alt / Shift: latch   □ Mapping   L/R Dock",
                    "○ 按键   Ctrl / Alt / Shift 点按锁定   □ 映射   L/R 上下移动",
                    "○ キー   Ctrl / Alt / Shift: 固定   □ 設定   L/R 移動",
                ),
                4,
                18,
            ));
        }
        out
    }
    pub fn refresh(&mut self, gpu: &Gpu) -> Result<bool, String> {
        if !self.dirty {
            return Ok(false);
        }
        let widgets = self.widgets();
        let surface = self.surface.get_or_insert_with(Surface::new);
        surface.update(gpu, widgets)?;
        self.dirty = false;
        Ok(true)
    }
    pub fn draw(&self, gpu: &Gpu) -> Result<(), String> {
        if let Some(image) = self.surface.as_ref().and_then(|s| s.image.as_ref()) {
            gpu.present_cursor(
                image,
                crate::window::DISPLAY,
                Rect {
                    top: self.top(),
                    ..SIZE.rect()
                },
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}
fn inside(rect: Rect, (x, y): (i32, i32)) -> bool {
    x >= rect.left
        && y >= rect.top
        && x < rect.left + rect.width as i32
        && y < rect.top + rect.height as i32
}
fn center(r: Rect) -> (i32, i32) {
    (r.left + r.width as i32 / 2, r.top + r.height as i32 / 2)
}
fn modifier(key: u16) -> Option<u8> {
    match key {
        16 => Some(1),
        17 => Some(4),
        18 => Some(2),
        _ => None,
    }
}
fn layout() -> Vec<Key> {
    let mut keys = Vec::new();
    let mut row = |row: i32, items: &[(u16, f32)]| {
        let mut x: f32 = 12.0;
        for &(code, units) in items {
            let width = units * 51.2;
            keys.push(Key {
                code,
                rectangle: Rect {
                    left: x.round() as i32,
                    top: 40 + row * 42,
                    width: width.round() as u32 - 2,
                    height: 39,
                },
            });
            x += width;
        }
    };
    let mut function = vec![(27, 1.0)];
    function.extend((112..=123).map(|key| (key, 14.0 / 12.0)));
    row(0, &function);
    let mut numbers = vec![(192, 1.0)];
    numbers.extend((48..=57).cycle().skip(1).take(10).map(|key| (key, 1.0)));
    numbers.extend([(189, 1.0), (187, 1.0), (8, 2.0)]);
    row(1, &numbers);
    let mut q = vec![(9, 1.5)];
    q.extend("QWERTYUIOP".bytes().map(|k| (k as u16, 1.0)));
    q.extend([(219, 1.0), (221, 1.0), (220, 1.5)]);
    row(2, &q);
    let mut a = vec![(20, 1.8)];
    a.extend("ASDFGHJKL".bytes().map(|k| (k as u16, 1.0)));
    a.extend([(186, 1.0), (222, 1.0), (13, 2.2)]);
    row(3, &a);
    let mut z = vec![(16, 2.3)];
    z.extend("ZXCVBNM".bytes().map(|k| (k as u16, 1.0)));
    z.extend([(188, 1.0), (190, 1.0), (191, 1.0), (16, 2.7)]);
    row(4, &z);
    row(5, &[(17, 1.6), (18, 1.6), (32, 8.6), (18, 1.6), (17, 1.6)]);
    for (row, columns) in [
        (0, vec![(0, 44), (1, 145), (2, 19)]),
        (1, vec![(0, 45), (1, 36), (2, 33)]),
        (2, vec![(0, 46), (1, 35), (2, 34)]),
        (4, vec![(1, 38)]),
        (5, vec![(0, 37), (1, 40), (2, 39)]),
    ] {
        for (col, code) in columns {
            keys.push(Key {
                code,
                rectangle: Rect {
                    left: 798 + col * 50,
                    top: 40 + row * 42,
                    width: 48,
                    height: 39,
                },
            });
        }
    }
    keys
}
struct Surface {
    image: Option<Image>,
    widgets: Vec<Widget>,
    paint: paint::Paint,
}
impl Surface {
    fn new() -> Self {
        Self {
            image: None,
            widgets: Vec::new(),
            paint: paint::Paint::new(),
        }
    }
    fn update(&mut self, gpu: &Gpu, widgets: Vec<Widget>) -> Result<(), String> {
        let full = self.image.is_none()
            || widgets.len() != self.widgets.len()
            || widgets
                .iter()
                .zip(&self.widgets)
                .any(|(a, b)| a.rect != b.rect);
        if full {
            let data = self.paint.page(SIZE, &widgets);
            let pixels = owned_pixels(gpu, SIZE, data)?;
            if self.image.is_none() {
                self.image = Some(
                    gpu.reserve_upload(SIZE, true, false)
                        .map_err(|e| e.to_string())?,
                );
            }
            gpu.upload(self.image.as_mut().unwrap(), &pixels)
                .map_err(|e| e.to_string())?;
        } else {
            for (next, old) in widgets.iter().zip(&self.widgets) {
                if next == old {
                    continue;
                }
                let size = Size {
                    width: next.rect.width,
                    height: next.rect.height,
                };
                let data = self.paint.widget(next);
                let pixels = owned_pixels(gpu, size, data)?;
                gpu.patch_region(self.image.as_mut().unwrap(), next.rect, &pixels)
                    .map_err(|e| e.to_string())?;
            }
        }
        self.widgets = widgets;
        Ok(())
    }
}
fn owned_pixels(gpu: &Gpu, size: Size, data: Vec<u8>) -> Result<Pixels, String> {
    let permit = gpu.staging.reserve(data.len()).map_err(|e| e.to_string())?;
    Ok(Pixels {
        size,
        main: Some(Bytes::with_permit(data, permit)),
        province: None,
    })
}

#[cfg(all(test, target_os = "linux"))]
#[allow(unsafe_code)]
#[path = "../../tests/input/panel.rs"]
mod tests;

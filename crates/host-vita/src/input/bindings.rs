//! Small, per-game controller maps. No allocation or filesystem work in lookup.
use std::{fs, path::Path};

pub const SELECT: u32 = 0x1;
pub const START: u32 = 0x8;
pub const UP: u32 = 0x10;
pub const RIGHT: u32 = 0x20;
pub const DOWN: u32 = 0x40;
pub const LEFT: u32 = 0x80;
pub const L: u32 = 0x100;
pub const R: u32 = 0x200;
pub const TRIANGLE: u32 = 0x1000;
pub const CIRCLE: u32 = 0x2000;
pub const CROSS: u32 = 0x4000;
pub const SQUARE: u32 = 0x8000;
pub const BUTTONS: &[(u32, &str)] = &[
    (L, "L"),
    (R, "R"),
    (SELECT, "SELECT"),
    (START, "START"),
    (UP, "↑"),
    (DOWN, "↓"),
    (LEFT, "←"),
    (RIGHT, "→"),
    (TRIANGLE, "△"),
    (CIRCLE, "○"),
    (CROSS, "×"),
    (SQUARE, "□"),
];
pub const VALID: u32 = 0xf3f9;
pub const CONFIG: &str = "krkr-input.tsv";
pub const MAX_BINDINGS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Key { key: u16, modifiers: u8 },
    Mouse(u8),
    Wheel(i16),
}
impl Action {
    pub const fn key(key: u16) -> Self {
        Self::Key { key, modifiers: 0 }
    }
    pub fn label(self) -> String {
        match self {
            Self::Key { key, modifiers } => {
                let mut label = String::new();
                for (flag, name) in [(4, "Ctrl+"), (2, "Alt+"), (1, "Shift+")] {
                    if modifiers & flag != 0 {
                        label.push_str(name);
                    }
                }
                label.push_str(&key_label(key));
                label
            }
            Self::Mouse(0) => "Mouse L".into(),
            Self::Mouse(_) => "Mouse R".into(),
            Self::Wheel(n) => if n > 0 { "Wheel ↑" } else { "Wheel ↓" }.into(),
        }
    }
    fn valid(self) -> bool {
        match self {
            Self::Key { key, modifiers } => (8..=254).contains(&key) && modifiers & !7 == 0,
            Self::Mouse(n) => n <= 1,
            Self::Wheel(n) => n == -120 || n == 120,
        }
    }
}
pub fn key_label(key: u16) -> String {
    match key {
        8 => "Backspace".into(),
        9 => "Tab".into(),
        13 => "Enter".into(),
        16 => "Shift".into(),
        17 => "Ctrl".into(),
        18 => "Alt".into(),
        19 => "Pause".into(),
        20 => "Caps".into(),
        27 => "Esc".into(),
        32 => "Space".into(),
        33 => "PgUp".into(),
        34 => "PgDn".into(),
        35 => "End".into(),
        36 => "Home".into(),
        37 => "←".into(),
        38 => "↑".into(),
        39 => "→".into(),
        40 => "↓".into(),
        44 => "PrtSc".into(),
        45 => "Ins".into(),
        46 => "Del".into(),
        48..=57 | 65..=90 => char::from_u32(u32::from(key)).unwrap().to_string(),
        112..=123 => format!("F{}", key - 111),
        145 => "ScrLk".into(),
        186 => ";".into(),
        187 => "=".into(),
        188 => ",".into(),
        189 => "-".into(),
        190 => ".".into(),
        191 => "/".into(),
        192 => "`".into(),
        219 => "[".into(),
        220 => "\\".into(),
        221 => "]".into(),
        222 => "'".into(),
        _ => format!("VK {key}"),
    }
}
pub fn button_label(mask: u32) -> String {
    BUTTONS
        .iter()
        .filter_map(|&(bit, name)| (mask & bit != 0).then_some(name))
        .collect::<Vec<_>>()
        .join("+")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Keys([u64; 4]);
impl Keys {
    pub fn set(&mut self, key: u16) {
        if key < 256 {
            self.0[key as usize / 64] |= 1 << (key % 64);
        }
    }
    pub fn contains(self, key: u16) -> bool {
        key < 256 && self.0[key as usize / 64] & (1 << (key % 64)) != 0
    }
    pub fn merge(&mut self, other: Self) {
        for (a, b) in self.0.iter_mut().zip(other.0) {
            *a |= b;
        }
    }
    pub fn modifiers(self) -> u32 {
        u32::from(self.contains(16))
            | (u32::from(self.contains(18)) << 1)
            | (u32::from(self.contains(17)) << 2)
    }
    pub fn add_modifiers(&mut self, modifiers: u8) {
        for (flag, key) in [(1, 16), (2, 18), (4, 17)] {
            if modifiers & flag != 0 {
                self.set(key);
            }
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub keys: Keys,
    pub mouse: u32,
    pub wheel: i32,
}
impl Output {
    pub fn add(&mut self, action: Action) {
        match action {
            Action::Key { key, modifiers } => {
                self.keys.add_modifiers(modifiers);
                self.keys.set(key);
            }
            Action::Mouse(button) => self.mouse |= 8 << button,
            Action::Wheel(delta) => self.wheel += i32::from(delta),
        }
    }
    pub fn merge(&mut self, other: Self) {
        self.keys.merge(other.keys);
        self.mouse |= other.mouse;
        self.wheel += other.wheel;
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub buttons: u32,
    pub action: Action,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bindings {
    pub entries: Vec<Binding>,
    pub top: bool,
}
impl Default for Bindings {
    fn default() -> Self {
        let mut entries = vec![
            Binding {
                buttons: CIRCLE,
                action: Action::Mouse(0),
            },
            Binding {
                buttons: CROSS,
                action: Action::Mouse(1),
            },
        ];
        entries.extend(
            [
                (TRIANGLE, 17),
                (SQUARE, 32),
                (START, 27),
                (SELECT, 13),
                (L, 33),
                (R, 34),
                (UP, 38),
                (RIGHT, 39),
                (DOWN, 40),
                (LEFT, 37),
            ]
            .map(|(buttons, key)| Binding {
                buttons,
                action: Action::key(key),
            }),
        );
        Self {
            entries,
            top: false,
        }
    }
}
impl Bindings {
    pub fn validate(&self) -> Result<(), String> {
        if self.entries.len() > MAX_BINDINGS {
            return Err("Too many bindings (maximum 32)".into());
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if entry.buttons == 0
                || entry.buttons & !VALID != 0
                || entry.buttons & (SELECT | START) == SELECT | START
                || !entry.action.valid()
            {
                return Err("Invalid binding; START+SELECT is reserved".into());
            }
            if self.entries[..index]
                .iter()
                .any(|e| e.buttons == entry.buttons)
            {
                return Err("This button combination is already mapped".into());
            }
        }
        Ok(())
    }
    pub fn load(directory: &Path) -> Result<Self, String> {
        let path = directory.join(CONFIG);
        let size = match fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e.to_string()),
        };
        if size > 8192 {
            return Err("Input settings exceed 8 KiB".into());
        }
        Self::parse(&fs::read_to_string(path).map_err(|e| e.to_string())?)
    }
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.lines();
        if lines.next() != Some("KRKR-INPUT\t1") {
            return Err("Invalid input settings header".into());
        }
        let mut settings = Self {
            entries: Vec::new(),
            top: false,
        };
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let fields: Vec<_> = line.split('\t').collect();
            match fields.as_slice() {
                ["dock", "top"] => settings.top = true,
                ["dock", "bottom"] => settings.top = false,
                ["bind", buttons, "key", key, modifiers] => settings.entries.push(Binding {
                    buttons: u32::from_str_radix(buttons, 16).map_err(|_| "Invalid button mask")?,
                    action: Action::Key {
                        key: key.parse().map_err(|_| "Invalid key")?,
                        modifiers: modifiers.parse().map_err(|_| "Invalid modifiers")?,
                    },
                }),
                ["bind", buttons, kind, value] => settings.entries.push(Binding {
                    buttons: u32::from_str_radix(buttons, 16).map_err(|_| "Invalid button mask")?,
                    action: match *kind {
                        "mouse" => {
                            Action::Mouse(value.parse().map_err(|_| "Invalid mouse button")?)
                        }
                        "wheel" => Action::Wheel(value.parse().map_err(|_| "Invalid wheel delta")?),
                        _ => return Err("Invalid binding action".into()),
                    },
                }),
                _ => return Err("Invalid input settings line".into()),
            }
            if settings.entries.len() > MAX_BINDINGS {
                return Err("Too many bindings".into());
            }
        }
        settings.validate()?;
        Ok(settings)
    }
    pub fn save(&self, directory: &Path) -> Result<(), String> {
        self.validate()?;
        let mut text = format!(
            "KRKR-INPUT\t1\ndock\t{}\n",
            if self.top { "top" } else { "bottom" }
        );
        for entry in &self.entries {
            use std::fmt::Write;
            write!(text, "bind\t{:x}\t", entry.buttons).unwrap();
            match entry.action {
                Action::Key { key, modifiers } => writeln!(text, "key\t{key}\t{modifiers}"),
                Action::Mouse(button) => writeln!(text, "mouse\t{button}"),
                Action::Wheel(delta) => writeln!(text, "wheel\t{delta}"),
            }
            .unwrap();
        }
        let temporary = directory.join("krkr-input.tsv.tmp");
        fs::write(&temporary, text).map_err(|e| e.to_string())?;
        fs::rename(temporary, directory.join(CONFIG)).map_err(|e| e.to_string())
    }
    pub fn ambiguous_buttons(&self) -> u32 {
        self.entries
            .iter()
            .filter(|e| e.buttons.count_ones() > 1)
            .fold(0, |mask, e| mask | e.buttons)
    }
    pub fn mouse_buttons(&self) -> u32 {
        self.entries
            .iter()
            .filter(|e| matches!(e.action, Action::Mouse(_)))
            .fold(0, |mask, e| mask | e.buttons)
    }
    pub fn resolve(&self, buttons: u32, singles: u32) -> (Output, u32) {
        let mut output = Output::default();
        let mut claimed = 0;
        // Most-specific complete chords win over their component bindings.
        for count in (1..=12).rev() {
            for entry in &self.entries {
                if entry.buttons.count_ones() != count
                    || buttons & entry.buttons != entry.buttons
                    || claimed & entry.buttons != 0
                    || (count == 1 && singles & entry.buttons == 0)
                {
                    continue;
                }
                output.add(entry.action);
                claimed |= entry.buttons;
            }
        }
        let chord_buttons = self
            .entries
            .iter()
            .filter(|e| e.buttons.count_ones() > 1 && claimed & e.buttons == e.buttons)
            .fold(0, |mask, e| mask | e.buttons);
        (output, chord_buttons)
    }
}

#[cfg(test)]
#[path = "../../tests/input/bindings.rs"]
mod tests;

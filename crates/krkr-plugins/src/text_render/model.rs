//! Layout behavior follows krkrsdl3/plugins/textrender.cpp, including buffered
//! output and the reference's alignment/flush rules.
use super::*;
#[derive(Clone)]
pub(super) struct Style {
    pub bold: bool,
    pub italic: bool,
    pub face: Vec<u16>,
    pub fontsize: i32,
    pub fontscale: f64,
    pub color: i32,
    pub ruby_size: i32,
    pub ruby_offset: i32,
    pub shadow: bool,
    pub shadow_color: i32,
    pub edge: bool,
    pub edge_color: i32,
    pub line_spacing: i32,
    pub pitch: i32,
    pub line_size: i32,
    pub align: i32,
    pub valign: i32,
    pub over: bool,
    pub delay: i32,
    pub text: Vec<u16>,
}
impl Default for Style {
    fn default() -> Self {
        Self {
            bold: false,
            italic: false,
            face: "user".encode_utf16().collect(),
            fontsize: 24,
            fontscale: 1.,
            color: 0xffffff,
            ruby_size: 10,
            ruby_offset: -2,
            shadow: true,
            shadow_color: 0,
            edge: false,
            edge_color: 0x0080ff,
            line_spacing: 6,
            pitch: 0,
            line_size: 0,
            align: -1,
            valign: -1,
            over: false,
            delay: 1000,
            text: Vec::new(),
        }
    }
}
#[derive(Clone)]
pub(super) struct Character {
    pub bold: bool,
    pub italic: bool,
    pub graph: bool,
    pub face: Vec<u16>,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub size: i32,
    pub color: i32,
    pub edge: i32,
    pub shadow: i32,
    pub text: Vec<u16>,
}
pub(super) struct Metrics {
    pub ascent: i32,
    pub widths: std::collections::BTreeMap<u16, i32>,
}
pub(super) struct Layout {
    pub style: Style,
    pub defaults: Style,
    pub vertical: bool,
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
    pub x: i32,
    pub y: i32,
    pub indent: i32,
    pub characters: Vec<Character>,
    pub pending: Vec<Character>,
    pub font: krkr_engine::protocol::text::Font,
    pub metrics: Option<Metrics>,
}
impl Default for Layout {
    fn default() -> Self {
        let mut result = Self {
            style: Style::default(),
            defaults: Style::default(),
            vertical: false,
            width: 0,
            height: 0,
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
            x: 0,
            y: 0,
            indent: 0,
            characters: Vec::new(),
            pending: Vec::new(),
            font: Default::default(),
            metrics: None,
        };
        result.update_font();
        result
    }
}
impl Layout {
    pub fn update_font(&mut self) {
        self.font = krkr_engine::protocol::text::Font {
            face: String::from_utf16_lossy(&self.style.face),
            height: self.style.fontsize,
            bold: self.style.bold,
            italic: self.style.italic,
            ..Default::default()
        };
        self.metrics = None;
    }
    pub fn ascent(&self) -> i32 {
        self.metrics.as_ref().map_or(0, |m| m.ascent)
    }
    pub fn clear(&mut self) {
        self.characters.clear();
        // The reference retains unflushed characters across clear/setRenderSize.
        self.style = self.defaults.clone();
        self.x = 0;
        match self.style.valign {
            -1 => {
                self.y = 0;
                self.top = 0;
                self.bottom = 0;
            }
            1 => {
                self.y = self.height.saturating_sub(self.style.fontsize);
                self.top = self.height;
                self.bottom = self.height;
            }
            _ => {
                self.y = self.height.saturating_sub(self.ascent()) / 2;
                self.top = 0;
                self.bottom = 0;
            }
        }
        self.left = if self.style.align == 1 { self.width } else { 0 };
        self.right = self.left;
        self.indent = 0;
        self.update_font();
    }
    pub fn newline(&mut self) {
        self.newlines(1);
    }
    pub fn newlines(&mut self, count: i32) {
        self.x = if self.style.align == 1 {
            self.width.saturating_sub(self.indent)
        } else {
            self.indent
        };
        let step = self
            .ascent()
            .saturating_add(self.style.line_spacing)
            .saturating_mul(count);
        let spacing = self.style.line_spacing.saturating_mul(count);
        match self.style.valign {
            -1 => {
                self.y = self.y.saturating_add(step);
                self.bottom = self.bottom.saturating_add(spacing);
            }
            1 => {
                self.y = self.y.saturating_sub(step);
                self.top = self.top.saturating_sub(spacing);
            }
            _ => self.y = self.y.saturating_add(step),
        }
    }
    pub fn push(&mut self, text: Vec<u16>, graph: Option<(i32, i32)>) -> NativeResult<()> {
        if self.characters.len() + self.pending.len() >= 65536 {
            return Err(NativeError::Message("text layout exceeds character limit"));
        }
        let (width, size) = graph.unwrap_or_else(|| {
            (
                self.metrics
                    .as_ref()
                    .and_then(|m| m.widths.get(&text[0]))
                    .copied()
                    .unwrap_or(0),
                self.style.fontsize,
            )
        });
        self.pending.push(Character {
            bold: self.style.bold,
            italic: self.style.italic,
            graph: graph.is_some(),
            face: self.style.face.clone(),
            x: 0,
            y: 0,
            width,
            size,
            color: self.style.color,
            edge: if self.style.edge {
                self.style.edge_color
            } else {
                0
            },
            shadow: if self.style.shadow {
                self.style.shadow_color
            } else {
                0
            },
            text,
        });
        Ok(())
    }
    pub fn flush(&mut self, force: bool) {
        if self.pending.is_empty() {
            return;
        }
        let step = self.ascent().saturating_add(self.style.line_spacing);
        match self.style.align {
            -1 => self.bottom = self.bottom.saturating_add(step),
            1 => self.top = self.top.saturating_sub(step),
            _ => {
                self.top = 0;
                self.bottom = self.height;
            }
        }
        match self.style.align {
            -1 => {
                let mut x = 0i32;
                for i in 0..self.pending.len() {
                    let mut next = x
                        .saturating_add(self.pending[i].width)
                        .saturating_add(self.style.pitch);
                    if self.width < next {
                        if !force {
                            self.flush(true);
                            return;
                        }
                        self.newline();
                        x = self.x;
                        next = x
                            .saturating_add(self.pending[i].width)
                            .saturating_add(self.style.pitch);
                    }
                    self.pending[i].x = x;
                    self.pending[i].y = self.y;
                    x = next;
                    self.right = self.right.max(x);
                }
                self.x = x;
            }
            1 => {
                let mut x = self.width;
                for i in 0..self.pending.len() {
                    let mut next = x.saturating_sub(self.pending[i].width);
                    if next < 0 {
                        if !force {
                            self.flush(true);
                            return;
                        }
                        self.newline();
                        next = self.x.saturating_sub(self.pending[i].width);
                    }
                    self.pending[i].x = next;
                    self.pending[i].y = self.y;
                    x = next.saturating_sub(self.style.pitch);
                    self.left = self.left.min(x);
                }
                self.x = x;
            }
            _ => {
                let total = self
                    .pending
                    .iter()
                    .fold(0i32, |n, c| n.saturating_add(c.width))
                    .saturating_add(
                        self.style
                            .pitch
                            .saturating_mul(self.pending.len().saturating_sub(1) as i32),
                    );
                if total > self.width && !force {
                    self.newline();
                    self.flush(true);
                    return;
                }
                let mut x = self.width.saturating_sub(total) / 2;
                let last = self.pending.len() - 1;
                for (i, c) in self.pending.iter_mut().enumerate() {
                    c.x = x;
                    c.y = self.y;
                    x = x.saturating_add(c.width);
                    if i < last {
                        x = x.saturating_add(self.style.pitch);
                    }
                }
                self.x = x;
                self.left = 0;
                self.right = self.width;
            }
        }
        for c in &self.pending {
            self.style.text.extend_from_slice(&c.text);
        }
        self.characters.append(&mut self.pending);
    }
}

//! Permissive UTF-16 fields, including the reference's text after closing quotes.
use super::{FIELD_LIMIT, TEXT_BYTES};
use std::sync::Arc;
use tjs_core::{NativeError, NativeResult};

#[derive(Clone, Copy)]
enum Mode {
    Start,
    Plain,
    Quoted,
    Quote,
    Tail,
}
pub(super) struct Row {
    text: Arc<[u16]>,
    pub position: usize,
    separator: u16,
    newline: Arc<[u16]>,
    mode: Mode,
    field: Vec<u16>,
    pub fields: Vec<Vec<u16>>,
    units: usize,
    started: bool,
}
impl Row {
    pub fn new(text: Arc<[u16]>, position: usize, separator: u16, newline: Arc<[u16]>) -> Self {
        Self {
            text,
            position,
            separator,
            newline,
            mode: Mode::Start,
            field: Vec::new(),
            fields: Vec::new(),
            units: 0,
            started: false,
        }
    }
    fn push(&mut self, c: u16) -> NativeResult<()> {
        self.admit(1)?;
        self.field.push(c);
        Ok(())
    }
    fn admit(&mut self, count: usize) -> NativeResult<()> {
        self.units = self
            .units
            .checked_add(count)
            .filter(|&n| n <= TEXT_BYTES / 2)
            .ok_or(NativeError::Message("CSV row exceeds text size limit"))?;
        Ok(())
    }
    fn newline(&mut self) -> NativeResult<()> {
        self.admit(self.newline.len())?;
        self.field.extend_from_slice(&self.newline);
        Ok(())
    }
    fn field(&mut self) -> NativeResult<()> {
        if self.fields.len() >= FIELD_LIMIT {
            return Err(NativeError::Message("CSV row exceeds field count limit"));
        }
        self.fields.push(std::mem::take(&mut self.field));
        self.mode = Mode::Start;
        Ok(())
    }
    pub fn advance(&mut self) -> NativeResult<bool> {
        for _ in 0..4096 {
            let Some(&c) = self.text.get(self.position) else {
                if matches!(self.mode, Mode::Quoted) {
                    self.newline()?;
                }
                if self.started {
                    self.field()?;
                }
                return Ok(true);
            };
            self.position += 1;
            // IFileStr adds each character through ttstr += char, which omits
            // NUL. It still consumes that physical line, even if all were NUL.
            if c == 0 {
                continue;
            }
            if matches!(c, 13 | 10) {
                if c == 13 && self.text.get(self.position) == Some(&10) {
                    self.position += 1;
                }
                if matches!(self.mode, Mode::Quoted) {
                    self.newline()?;
                    if self.position == self.text.len() {
                        self.field()?;
                        return Ok(true);
                    }
                    continue;
                }
                if self.started {
                    self.field()?;
                }
                return Ok(true); // A completely empty physical row is an empty Array.
            }
            self.started = true;
            match self.mode {
                Mode::Start if c == 34 => self.mode = Mode::Quoted,
                Mode::Quoted if c == 34 => self.mode = Mode::Quote,
                Mode::Quote if c == 34 => {
                    self.push(34)?;
                    self.mode = Mode::Quoted;
                }
                Mode::Quoted => self.push(c)?,
                _ if c == self.separator => self.field()?,
                _ => {
                    self.push(c)?;
                    match self.mode {
                        Mode::Start => self.mode = Mode::Plain,
                        Mode::Quote => self.mode = Mode::Tail,
                        _ => {}
                    }
                }
            }
        }
        Ok(false)
    }
}

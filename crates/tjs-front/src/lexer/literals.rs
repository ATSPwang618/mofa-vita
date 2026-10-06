use super::{Lexer, hex_digit, is_space};
use tjs_core::Diagnostic;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StringEnd {
    Quote,
    Ampersand,
    Brace,
}

pub(super) struct StringPart {
    pub units: Vec<u16>,
    pub end: StringEnd,
    pub boundary: usize,
}

impl Lexer<'_> {
    pub(super) fn string(&mut self, start: usize, delimiter: u16) -> Result<Vec<u16>, Diagnostic> {
        self.cursor += 1;
        Ok(self.string_part(start, delimiter, false)?.units)
    }

    pub(super) fn string_part(
        &mut self,
        start: usize,
        delimiter: u16,
        interpolate: bool,
    ) -> Result<StringPart, Diagnostic> {
        let mut units = Vec::new();
        let (end, boundary) = loop {
            let boundary = self.cursor;
            let unit = self
                .peek(0)
                .ok_or_else(|| self.error(start, "unterminated string"))?;
            self.cursor += 1;
            if unit == delimiter {
                let mut next = self.cursor;
                while self.units.get(next).copied().is_some_and(is_space) {
                    next += 1;
                }
                if self.units.get(next) == Some(&delimiter) {
                    // The reference merges adjacent literals only with the same delimiter.
                    self.cursor = next + 1;
                    continue;
                }
                break (StringEnd::Quote, boundary);
            }
            if unit == 0 {
                return Err(self.error(start, "unterminated string before NUL source unit"));
            }
            if interpolate && unit == 38 {
                break (StringEnd::Ampersand, boundary);
            }
            if interpolate && unit == 36 && self.peek(0) == Some(123) {
                self.cursor += 1;
                break (StringEnd::Brace, boundary);
            }
            let decoded = if unit == 92 {
                let mut escaped = self
                    .peek(0)
                    .ok_or_else(|| self.error(start, "unterminated string escape"))?;
                self.cursor += 1;
                if escaped == 13 {
                    if self.peek(0) == Some(10) {
                        self.cursor += 1;
                    }
                    escaped = 10;
                }
                match escaped {
                    120 | 88 => {
                        let mut value = 0;
                        for _ in 0..4 {
                            let Some(digit) = self.peek(0).and_then(hex_digit) else {
                                break;
                            };
                            value = value * 16 + digit;
                            self.cursor += 1;
                        }
                        value
                    }
                    48 => {
                        let mut value = 0_u16;
                        while let Some(digit) = self.peek(0).filter(|u| (48..=55).contains(u)) {
                            value = value.wrapping_mul(8).wrapping_add(digit - 48);
                            self.cursor += 1;
                        }
                        value
                    }
                    97 => 7,
                    98 => 8,
                    102 => 12,
                    110 => 10,
                    114 => 13,
                    116 => 9,
                    118 => 11,
                    other => other,
                }
            } else if unit == 13 {
                // The reference normalizes source CRLF/CR before tokenization.
                if self.peek(0) == Some(10) {
                    self.cursor += 1;
                }
                10
            } else {
                unit
            };
            units.push(decoded);
        };
        // FixLength applies to each literal segment, including those in interpolation.
        if let Some(end) = units.iter().position(|&unit| unit == 0) {
            units.truncate(end);
        }
        Ok(StringPart {
            units,
            end,
            boundary,
        })
    }

    pub(super) fn octet(&mut self, start: usize) -> Result<Vec<u8>, Diagnostic> {
        self.cursor += 2;
        let mut bytes = Vec::new();
        let mut pending = None;
        loop {
            if self.comment()? {
                continue;
            }
            let unit = self
                .peek(0)
                .ok_or_else(|| self.error(start, "unterminated octet literal"))?;
            if unit == 0 {
                return Err(self.error(start, "unterminated octet before NUL source unit"));
            }
            if unit == 37 && self.peek(1) == Some(62) {
                self.cursor += 2;
                if let Some(nibble) = pending {
                    bytes.push(nibble);
                }
                return Ok(bytes);
            }
            self.cursor += 1;
            if let Some(digit) = hex_digit(unit) {
                let digit = digit as u8;
                if let Some(high) = pending.take() {
                    bytes.push((high << 4) | digit);
                } else {
                    pending = Some(digit);
                }
            } else if unit == 44 {
                if let Some(nibble) = pending.take() {
                    bytes.push(nibble);
                }
            }
            // Like TJSParseOctet, separators other than comma do not end a byte.
        }
    }
}

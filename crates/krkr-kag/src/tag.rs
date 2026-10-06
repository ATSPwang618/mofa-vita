use crate::{Error, Result, Text, lower, ws};

#[derive(Clone, Debug)]
pub enum Argument {
    Omitted,
    Literal(Text),
    Expression(Text),
    Macro(Text),
}
pub(crate) enum ParsedTag {
    Complete(Tag),
    Continue(usize),
}
#[derive(Clone, Debug)]
pub enum Attribute {
    Spread,
    Named { name: Text, value: Argument },
}
#[derive(Clone, Debug)]
pub struct Tag {
    pub name: Text,
    pub attributes: Vec<Attribute>,
    pub start: usize,
    pub end: usize,
    pub line_command: bool,
    pub raw: Text,
    pub continuation_lines: usize,
}
impl Tag {
    pub(crate) fn parse(
        text: &[u16],
        start: usize,
        line: usize,
        line_command: bool,
        multiline: bool,
        advanced: bool,
    ) -> Result<ParsedTag> {
        let delimiter = if line_command { 0 } else { 93 };
        let at = |pos: usize| text.get(pos).copied().unwrap_or(0);
        let error = |pos| Error::new(line, pos, "malformed tag or quoted attribute");
        let mut pos = start + 1;
        while ws(at(pos)) {
            pos += 1;
        }
        let name_start = pos;
        let embedded = advanced && at(pos) == 38;
        let embedded_name = if embedded {
            Some(read_advanced_value(text, &mut pos, delimiter, line)?)
        } else {
            None
        };
        if !embedded {
            while at(pos) != 0 && at(pos) != delimiter && !ws(at(pos)) {
                pos += 1;
            }
        }
        if name_start == pos {
            return Err(error(pos));
        }
        let name = embedded_name.unwrap_or_else(|| lower(&text[name_start..pos]));
        let mut attributes = Vec::new();
        loop {
            while ws(at(pos)) {
                pos += 1;
            }
            if at(pos) == delimiter {
                break;
            }
            if at(pos) == 0 {
                return Err(error(pos));
            }
            // KAGParserEx joins lines only between attributes, never within a
            // quoted string or an unquoted attribute value.
            if at(pos) == 92 && (advanced || (multiline && at(pos + 1) == 0)) {
                if !multiline {
                    return Err(Error::new(line, pos, "multiline tags are disabled"));
                }
                return Ok(ParsedTag::Continue(pos));
            }
            if at(pos) == 42 {
                attributes.push(Attribute::Spread);
                pos += 1;
                continue;
            }
            let name_start = pos;
            while at(pos) != 0 && at(pos) != delimiter && at(pos) != 61 && !ws(at(pos)) {
                pos += 1;
            }
            if pos == name_start {
                return Err(error(pos));
            }
            let name = lower(&text[name_start..pos]);
            while ws(at(pos)) {
                pos += 1;
            }
            if at(pos) != 61 {
                attributes.push(Attribute::Named {
                    name,
                    value: Argument::Omitted,
                });
                continue;
            }
            pos += 1;
            if advanced {
                let mut raw = read_advanced_value(text, &mut pos, delimiter, line)?;
                let value = match raw.first().copied() {
                    Some(38) => {
                        raw.remove(0);
                        Argument::Expression(raw)
                    }
                    Some(37) => {
                        raw.remove(0);
                        Argument::Macro(raw)
                    }
                    _ => Argument::Literal(raw),
                };
                attributes.push(Attribute::Named { name, value });
                continue;
            }
            while ws(at(pos)) {
                pos += 1;
            }
            if at(pos) == 0 {
                return Err(error(pos));
            }
            let mut entity = false;
            let mut macro_arg = false;
            if at(pos) == 38 {
                entity = true;
                pos += 1;
            } else if at(pos) == 37 {
                macro_arg = true;
                pos += 1;
            }
            let quote = if matches!(at(pos), 34 | 39) {
                let q = at(pos);
                pos += 1;
                q
            } else {
                0
            };
            // A sigil immediately inside quotes is active; a backtick escapes it.
            if !entity && at(pos) == 38 {
                entity = true;
                pos += 1;
            }
            if !macro_arg && at(pos) == 37 {
                macro_arg = true;
                pos += 1;
            }
            let mut value = Vec::new();
            while at(pos) != 0
                && if quote != 0 {
                    at(pos) != quote
                } else {
                    at(pos) != delimiter && !ws(at(pos))
                }
            {
                if at(pos) == 96 {
                    pos += 1;
                    if at(pos) == 0 {
                        return Err(error(pos));
                    }
                }
                value.push(at(pos));
                pos += 1;
            }
            if quote != 0 {
                if at(pos) != quote {
                    return Err(error(pos));
                }
                pos += 1;
            }
            let value = if entity {
                Argument::Expression(value)
            } else if macro_arg {
                Argument::Macro(value)
            } else {
                Argument::Literal(value)
            };
            attributes.push(Attribute::Named { name, value });
        }
        let end = pos + usize::from(!line_command);
        let raw = if line_command {
            let mut raw = Vec::with_capacity(pos - start + 1);
            raw.push(91);
            raw.extend_from_slice(&text[start + 1..pos]);
            raw.push(93);
            raw
        } else {
            text[start..end].to_vec()
        };
        Ok(ParsedTag::Complete(Self {
            name,
            attributes,
            start,
            end,
            line_command,
            raw,
            continuation_lines: 0,
        }))
    }
}

fn read_advanced_value(text: &[u16], pos: &mut usize, delimiter: u16, line: usize) -> Result<Text> {
    let at = |p: usize| text.get(p).copied().unwrap_or(0);
    let error = |p| Error::new(line, p, "malformed ExtKAG attribute value");
    while ws(at(*pos)) {
        *pos += 1;
    }
    if at(*pos) == 0 || at(*pos) == delimiter {
        return Err(error(*pos));
    }
    let mut value = Text::new();
    if at(*pos) == 38 {
        value.push(38);
        *pos += 1;
    }
    let quote = if matches!(at(*pos), 34 | 39) {
        let ch = at(*pos);
        *pos += 1;
        ch
    } else {
        0
    };
    while at(*pos) != 0
        && if quote != 0 {
            at(*pos) != quote
        } else {
            at(*pos) != delimiter && !ws(at(*pos))
        }
    {
        if at(*pos) == 96 {
            *pos += 1;
            if at(*pos) == 0 {
                return Err(error(*pos));
            }
        }
        value.push(at(*pos));
        *pos += 1;
    }
    if at(*pos) == 0 && (delimiter != 0 || quote != 0) {
        return Err(error(*pos));
    }
    if quote != 0 {
        *pos += 1;
    }
    Ok(value)
}

use crate::{Condition, Error, Result, Scenario, Tag, Text, is, units};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct Position {
    pub line: usize,
    pub pos: usize,
    pub buffer: Option<Arc<Text>>,
}
#[derive(Clone, Debug)]
pub struct CallFrame {
    pub storage: Text,
    pub label: Text,
    pub offset: usize,
    pub original_line: Text,
    pub position: Position,
    pub conditions: Vec<Condition>,
    pub macro_base: usize,
    pub macro_depth: usize,
}
#[derive(Clone)]
pub struct Parser {
    pub scenario: Option<Arc<Scenario>>,
    pub storage: Text,
    pub position: Position,
    pub label: Text,
    pub conditions: Vec<Condition>,
    pub calls: Vec<CallFrame>,
    pub macro_base: usize,
    pub macro_depth: usize,
    pub process_special: bool,
    pub ignore_cr: bool,
    pub multiline_tags: bool,
    pub advanced: bool,
    pub enable_np: bool,
    pub interrupted: bool,
    pub max_depth: usize,
    pub max_expanded_units: usize,
    recording: Option<(Text, Text)>,
}
impl Default for Parser {
    fn default() -> Self {
        Self {
            scenario: None,
            storage: Vec::new(),
            position: Position::default(),
            label: Vec::new(),
            conditions: Vec::new(),
            calls: Vec::new(),
            macro_base: 0,
            macro_depth: 0,
            process_special: true,
            ignore_cr: false,
            multiline_tags: false,
            advanced: false,
            enable_np: false,
            interrupted: false,
            max_depth: 256,
            max_expanded_units: 8 * 1024 * 1024,
            recording: None,
        }
    }
}
pub enum Token {
    Skip,
    End,
    Interrupt,
    Character(u16),
    Newline(bool),
    Tag(Tag),
    Label { name: Text, page: Option<Text> },
    Script { text: Text, line: usize },
    Macro { name: Text, body: Text },
}
impl Parser {
    pub fn line(&self) -> &[u16] {
        self.position
            .buffer
            .as_deref()
            .map(Vec::as_slice)
            .or_else(|| {
                self.scenario
                    .as_ref()
                    .and_then(|s| s.line(self.position.line))
            })
            .unwrap_or_default()
    }
    pub fn load(&mut self, storage: Text, scenario: Arc<Scenario>) {
        self.storage = storage;
        self.scenario = Some(scenario);
        self.position = Position::default();
        self.break_control();
    }
    pub fn break_control(&mut self) {
        self.conditions.clear();
        self.recording = None;
        self.macro_depth = self.macro_base;
    }
    pub fn clear_buffer(&mut self) {
        self.scenario = None;
        self.storage.clear();
        self.position = Position::default();
        self.break_control();
    }
    pub fn clear(&mut self) {
        self.clear_buffer();
        self.clear_calls();
    }
    pub fn advance_line(&mut self) {
        self.position.line += 1;
        self.position.pos = 0;
        self.position.buffer = None;
    }
    pub fn finish_tag(&mut self, tag: &Tag) {
        if tag.continuation_lines != 0 {
            self.position.line += tag.continuation_lines;
            self.advance_line();
        } else if tag.line_command {
            self.advance_line();
        } else {
            self.position.pos = tag.end;
        }
    }
    pub fn record(&mut self, name: Text) -> Result<()> {
        if name.is_empty() {
            return Err(self.error("macro name missing"));
        }
        self.recording = Some((crate::lower(&name), Vec::new()));
        Ok(())
    }
    pub fn append_recording(&mut self, text: &[u16]) -> Result<()> {
        if let Some((_, body)) = &mut self.recording {
            if body.len().saturating_add(text.len()) > self.max_expanded_units {
                return Err(self.error("macro size limit"));
            }
            body.extend_from_slice(text);
        }
        Ok(())
    }
    pub fn expand(&mut self, tag: &Tag, text: &[u16], escape: bool) -> Result<()> {
        let extra = if escape {
            text.iter().filter(|&&u| u == 91).count()
        } else {
            0
        };
        let current = self.line();
        let length = current.len() - (tag.end - tag.start)
            + text.len()
            + extra
            + usize::from(tag.line_command && !self.ignore_cr);
        if length > self.max_expanded_units {
            return Err(self.error("expanded line size limit"));
        }
        if self.position.buffer.is_none() {
            let mut buffer = Vec::with_capacity(length);
            buffer.extend_from_slice(&current[..tag.start]);
            for &unit in text {
                buffer.push(unit);
                if escape && unit == 91 {
                    buffer.push(unit);
                }
            }
            if tag.line_command && !self.ignore_cr {
                buffer.push(92);
            }
            buffer.extend_from_slice(&current[tag.end..]);
            self.position.buffer = Some(Arc::new(buffer));
            self.position.pos = tag.start;
            return Ok(());
        }
        if extra == 0 {
            let buffer = Arc::make_mut(self.position.buffer.as_mut().unwrap());
            buffer.splice(
                tag.start..tag.end,
                text.iter()
                    .copied()
                    .chain((tag.line_command && !self.ignore_cr).then_some(92)),
            );
            self.position.pos = tag.start;
            return Ok(());
        }
        let mut inserted = Vec::with_capacity(text.len() + extra + 1);
        for &u in text {
            if escape && u == 91 {
                inserted.push(91);
            }
            inserted.push(u);
        }
        if tag.line_command && !self.ignore_cr {
            inserted.push(92);
        }
        let buffer = Arc::make_mut(self.position.buffer.as_mut().unwrap());
        buffer.splice(tag.start..tag.end, inserted);
        self.position.pos = tag.start;
        Ok(())
    }
    pub fn next_token(&mut self) -> Result<Token> {
        let Some(scenario) = self.scenario.clone() else {
            return Ok(Token::End);
        };
        if self.position.line >= scenario.line_count() {
            return Ok(Token::End);
        }
        if self.interrupted {
            self.interrupted = false;
            return Ok(Token::Interrupt);
        }
        let line = self.line();
        if self.position.buffer.is_none() && self.position.pos == 0 {
            if line.first() == Some(&59) {
                self.advance_line();
                return Ok(Token::Skip);
            }
            if line.first() == Some(&42) {
                if self.recording.is_some() {
                    return Err(self.error("label inside macro"));
                }
                let name = scenario.labels()?.aliases[self.position.line].clone();
                let page = line
                    .iter()
                    .position(|&u| u == 124)
                    .map(|at| line[at + 1..].to_vec());
                self.label = name.clone();
                return Ok(Token::Label { name, page });
            }
            if is(line, "[iscript]") || is(line, "[iscript]\\") || is(line, "@iscript") {
                if self.recording.is_some() {
                    return Err(self.error("inline script inside macro"));
                }
                let start = self.position.line + 1;
                let mut text = Vec::new();
                let mut end = start;
                while let Some(line) = scenario.line(end) {
                    if is(line, "[endscript]")
                        || is(line, "[endscript]\\")
                        || is(line, "@endscript")
                    {
                        break;
                    }
                    if !self.excluded() {
                        text.extend_from_slice(line);
                        text.extend([13, 10]);
                    }
                    end += 1;
                }
                if end == scenario.line_count() {
                    return Err(Error::new(start, 0, "iscript has no endscript"));
                }
                self.position.line = end;
                return Ok(if self.excluded() {
                    self.advance_line();
                    Token::Skip
                } else {
                    Token::Script { text, line: start }
                });
            }
        }
        let pos = self.position.pos;
        if !self.ignore_cr
            && (line.get(pos..) == Some(&[92])
                || (pos == line.len() && line.ends_with(&[91, 112, 93])))
        {
            self.advance_line();
            return Ok(Token::Skip);
        }
        if pos >= line.len() {
            let emit = !self.ignore_cr;
            self.advance_line();
            if emit
                && self.enable_np
                && scenario
                    .line(self.position.line)
                    .is_some_and(|line| line.is_empty())
            {
                self.position.buffer = Some(Arc::new(units("[np]\\")));
                return Ok(Token::Skip);
            }
            if !emit {
                return Ok(Token::Skip);
            }
            if self.recording.is_some() {
                self.append_recording(&units("[r eol=true]"))?;
                return Ok(Token::Skip);
            }
            return Ok(if self.excluded() {
                Token::Skip
            } else {
                Token::Newline(true)
            });
        }
        let command = pos == 0 && self.position.buffer.is_none() && line[0] == 64;
        if !command && (line[pos] != 91 || line.get(pos + 1) == Some(&91)) {
            let ch = line[pos];
            self.position.pos += if ch == 91 { 2 } else { 1 };
            if ch == 9 {
                return Ok(Token::Skip);
            }
            if self.recording.is_some() {
                self.append_recording(if ch == 91 {
                    &[91, 91]
                } else if ch == 10 {
                    &[91, 114, 93]
                } else {
                    std::slice::from_ref(&ch)
                })?;
                return Ok(Token::Skip);
            }
            return Ok(if self.excluded() {
                Token::Skip
            } else if ch == 10 && !self.advanced {
                Token::Newline(false)
            } else {
                Token::Character(ch)
            });
        }
        let mut continuation_lines = 0;
        let mut tag = loop {
            let join_at = match Tag::parse(
                self.line(),
                pos,
                self.position.line,
                command,
                self.multiline_tags,
                self.advanced,
            )? {
                crate::tag::ParsedTag::Complete(tag) => break tag,
                crate::tag::ParsedTag::Continue(at) => at,
            };
            continuation_lines += 1;
            let next = scenario
                .line(self.position.line + continuation_lines)
                .and_then(|line| {
                    if self.advanced {
                        Some(line)
                    } else {
                        line.strip_prefix(&[59])
                    }
                })
                .ok_or_else(|| self.error("multi-line tag requires a following semicolon line"))?;
            if self.line().len().saturating_add(next.len()) > self.max_expanded_units {
                return Err(self.error("multi-line tag size limit"));
            }
            if let Some(buffer) = &mut self.position.buffer {
                let joined = Arc::make_mut(buffer);
                joined.truncate(join_at);
                joined.extend_from_slice(next);
            } else {
                let mut joined = Vec::with_capacity(join_at + next.len());
                joined.extend_from_slice(&self.line()[..join_at]);
                joined.extend_from_slice(next);
                self.position.buffer = Some(Arc::new(joined));
            }
        };
        if self.advanced {
            self.position.line += continuation_lines;
        } else {
            tag.continuation_lines = continuation_lines;
        }
        self.position.pos = tag.end - usize::from(!command);
        if self.recording.is_some() {
            self.finish_tag(&tag);
            if self.process_special && is(&tag.name, "endmacro") {
                self.append_recording(&units("[macropop]"))?;
                let (name, body) = self.recording.take().unwrap();
                return Ok(Token::Macro { name, body });
            }
            self.append_recording(&tag.raw)?;
            return Ok(Token::Skip);
        }
        Ok(Token::Tag(tag))
    }
}

use crate::{Error, Parser, Result, Text};

#[derive(Clone, Debug)]
pub struct Condition {
    pub parent_excluded: bool,
    pub executed: bool,
    pub excluded: bool,
}
impl Parser {
    pub fn excluded(&self) -> bool {
        self.conditions.last().is_some_and(|c| c.excluded)
    }
    pub fn begin_if(&mut self, condition: bool) -> Result<()> {
        if self.conditions.len() >= self.max_depth {
            return Err(self.error("conditional depth limit"));
        }
        let parent = self.excluded();
        self.conditions.push(Condition {
            parent_excluded: parent,
            executed: !parent && condition,
            excluded: parent || !condition,
        });
        Ok(())
    }
    pub fn needs_elsif(&self) -> bool {
        self.conditions
            .last()
            .is_some_and(|c| !c.parent_excluded && !c.executed)
    }
    pub fn branch(&mut self, condition: bool) {
        if let Some(last) = self.conditions.last_mut() {
            let execute = !last.parent_excluded && !last.executed && condition;
            last.excluded = !execute;
            last.executed |= execute;
        }
    }
    pub fn end_if(&mut self) {
        self.conditions.pop();
    }
    pub fn goto(&mut self, label: &[u16]) -> Result<()> {
        if label.is_empty() {
            return Ok(());
        }
        let scenario = self
            .scenario
            .as_ref()
            .ok_or_else(|| self.error("no scenario loaded"))?;
        let line = *scenario.labels()?.by_name.get(label).ok_or_else(|| {
            self.error(format!(
                "label {} not found",
                String::from_utf16_lossy(label)
            ))
        })?;
        self.position.line = line;
        self.position.pos = 0;
        self.position.buffer = None;
        self.label.clear();
        self.label.extend_from_slice(label);
        self.break_control();
        Ok(())
    }
    pub fn push_call(&mut self) -> Result<()> {
        if self.calls.len() >= self.max_depth {
            return Err(self.error("call depth limit"));
        }
        self.calls.push(self.call_frame()?);
        self.macro_base = self.macro_depth;
        Ok(())
    }
    pub fn call_frame(&self) -> Result<crate::CallFrame> {
        let mut label_line = 0;
        let mut label = Text::new();
        if let Some(scenario) = &self.scenario
            && self.position.line != 0
            && let Some((line, alias)) = scenario
                .labels()?
                .before(self.position.line.min(scenario.line_count()))
        {
            label_line = line;
            label = alias.clone();
        }
        Ok(crate::CallFrame {
            storage: self.storage.clone(),
            label,
            offset: self.position.line - label_line,
            original_line: self
                .scenario
                .as_ref()
                .and_then(|s| s.line(self.position.line))
                .unwrap_or_default()
                .to_vec(),
            position: self.position.clone(),
            conditions: self.conditions.clone(),
            macro_base: self.macro_base,
            macro_depth: self.macro_depth,
        })
    }
    pub fn return_position(&mut self, frame: &crate::CallFrame) -> Result<()> {
        if !frame.label.is_empty() {
            self.goto(&frame.label)?;
        }
        let line = self
            .position
            .line
            .checked_add(frame.offset)
            .ok_or_else(|| self.error("invalid return offset"))?;
        let scenario = self
            .scenario
            .as_ref()
            .ok_or_else(|| self.error("no return scenario"))?;
        if line > scenario.line_count()
            || scenario.line(line).unwrap_or_default() != frame.original_line
        {
            return Err(self.error("scenario changed at return position"));
        }
        self.position = frame.position.clone();
        self.position.line = line;
        self.conditions.clone_from(&frame.conditions);
        Ok(())
    }
    pub fn pop_macro(&mut self) -> Result<()> {
        self.macro_depth = self
            .macro_depth
            .checked_sub(1)
            .ok_or_else(|| self.error("macro argument stack underflow"))?;
        Ok(())
    }
    pub fn clear_calls(&mut self) {
        self.calls.clear();
        self.macro_depth = 0;
        self.macro_base = 0;
    }
    pub(crate) fn error(&self, message: impl Into<String>) -> Error {
        Error::new(self.position.line, self.position.pos, message)
    }
}

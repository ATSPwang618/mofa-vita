use super::{Lexer, TriviaKind, is_space};
use tjs_core::{Diagnostic, Phase};

impl Lexer<'_> {
    fn directive(&mut self) -> Option<&'static str> {
        for name in ["endif", "set", "if"] {
            if self.units[self.cursor + 1..]
                .iter()
                .copied()
                .take(name.len())
                .eq(name.bytes().map(u16::from))
            {
                self.cursor += name.len() + 1;
                return Some(name);
            }
        }
        None
    }

    fn pp_expression(&mut self, start: usize) -> Result<std::ops::Range<usize>, Diagnostic> {
        while self.peek(0).is_some_and(is_space) {
            self.cursor += 1;
        }
        match self.peek(0) {
            Some(40) => self.cursor += 1,
            None => return Err(self.error(start, "expected ( after preprocessor directive")),
            _ => return Err(self.error(start, "expected ( after preprocessor directive")),
        }
        let expression = self.cursor;
        let mut depth = 0;
        while let Some(unit) = self.peek(0) {
            match unit {
                40 => depth += 1,
                41 if depth == 0 => {
                    let end = self.cursor;
                    self.cursor += 1;
                    return Ok(expression..end);
                }
                41 => depth -= 1,
                _ => {}
            }
            self.cursor += 1;
        }
        Err(self.error(start, "unterminated preprocessor expression"))
    }

    pub(super) fn preprocess(&mut self, start: usize) -> Result<bool, Diagnostic> {
        let Some(directive) = self.directive() else {
            return Ok(false);
        };
        if directive == "endif" {
            if self.pp_conditions.pop().is_none() {
                return Err(self.error(start, "@endif without @if"));
            }
        } else {
            let range = self.pp_expression(start)?;
            let value = self
                .preprocessor
                .evaluate(&self.units[range.clone()])
                .map_err(|error| {
                    let position = range.start + error.offset;
                    Diagnostic::new(
                        Phase::Lex,
                        self.span(position, (position + 1).min(self.cursor)),
                        error.message,
                    )
                })?;
            self.record_trivia(start, TriviaKind::Preprocessor)?;
            if directive == "if" {
                if value == 0 {
                    self.skip_disabled(start)?;
                } else {
                    self.pp_conditions.push(start);
                }
            }
            return Ok(true);
        }
        self.record_trivia(start, TriviaKind::Preprocessor)?;
        Ok(true)
    }

    fn skip_disabled(&mut self, start: usize) -> Result<(), Diagnostic> {
        let begin = self.cursor;
        let mut depth = 1;
        while let Some(unit) = self.peek(0) {
            // Match SkipUntil_endif: skip comments and directive parentheses,
            // but otherwise scan raw text, including quotes and invalid TJS.
            if unit == 47 {
                let trivia = self.trivia.len();
                if self.comment()? {
                    self.trivia.truncate(trivia);
                    continue;
                }
            }
            if unit == 64 {
                let directive_start = self.cursor;
                match self.directive() {
                    Some("endif") => {
                        depth -= 1;
                        if depth == 0 {
                            return self.record_trivia(begin, TriviaKind::Disabled);
                        }
                        continue;
                    }
                    Some(name) => {
                        self.pp_expression(directive_start)?;
                        if name == "if" {
                            depth += 1;
                        }
                        continue;
                    }
                    None => {}
                }
            }
            self.cursor += 1;
        }
        Err(self.error(start, "unterminated @if; expected @endif"))
    }
}

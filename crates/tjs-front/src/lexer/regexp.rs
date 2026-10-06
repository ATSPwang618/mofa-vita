use super::{Lexer, TokenKind};
use tjs_core::{Diagnostic, Span};

impl Lexer<'_> {
    /// Called only by the parser after consuming / or /= in prefix position.
    /// TJS terminates at the first unescaped slash, also inside character classes.
    pub(crate) fn regexp(&mut self, start: Span) -> Result<u32, Diagnostic> {
        let begin = start.start().get() as usize;
        self.cursor = begin + 1;
        let mut pattern = Vec::new();
        let mut escaped = false;
        loop {
            let unit = self
                .peek(0)
                .ok_or_else(|| self.error(begin, "unterminated regular expression"))?;
            if unit == 0 {
                return Err(self.error(begin, "unterminated regular expression"));
            }
            self.cursor += 1;
            if unit == 47 && !escaped {
                break;
            }
            pattern.push(unit);
            escaped = unit == 92 && !escaped;
        }
        let flag_start = self.cursor;
        while self.peek(0).is_some_and(|u| matches!(u, 97..=122)) {
            self.cursor += 1;
        }
        let mut encoded = vec![47, 47];
        encoded.extend_from_slice(&self.units[flag_start..self.cursor]);
        encoded.push(47);
        encoded.extend(pattern);
        let index = self.strings.len() as u32;
        self.strings.push(encoded.into());
        let span = self.span(begin, self.cursor);
        let token = self.tokens.last_mut().expect("consumed slash");
        debug_assert_eq!(token.span.start(), start.start());
        token.kind = TokenKind::RegExp(index);
        token.span = span;
        Ok(index)
    }
}

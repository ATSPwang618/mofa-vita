//! Lower interpolated strings to the reference's (text + string(expr) + text).
//! An explicit mode stack handles nesting without recursive lexer calls or
//! synthetic source files; expression tokens retain their original UTF-16 spans.
use super::{Lexer, TokenKind, is_space, literals::StringEnd};
use tjs_core::Diagnostic;

#[derive(Clone, Copy)]
pub(super) struct Interpolation {
    pub start: usize,
    delimiter: u16,
    need_plus: bool,
    expression: Option<Expression>,
}

#[derive(Clone, Copy)]
struct Expression {
    terminator: TokenKind,
    // Like the reference NestLevel, parentheses, brackets and braces share depth.
    depth: i32,
}

impl Lexer<'_> {
    pub(super) fn begin_interpolation(&mut self, start: usize) -> Result<(), Diagnostic> {
        self.cursor += 1;
        let whitespace = self.cursor;
        while self.peek(0).is_some_and(is_space) {
            self.cursor += 1;
        }
        if self.cursor != whitespace {
            self.record_trivia(whitespace, super::TriviaKind::Whitespace)?;
        }
        let delimiter = match self.peek(0) {
            Some(delimiter @ (34 | 39)) => delimiter,
            None => return Err(self.error(start, "expected a quote after @")),
            _ => {
                return Err(self.error(
                    start,
                    "expected an interpolated string or @set, @if, @endif",
                ));
            }
        };
        self.cursor += 1;
        self.emit(TokenKind::LeftParen, self.span(start, self.cursor))?;
        self.interpolations.push(Interpolation {
            start,
            delimiter,
            need_plus: false,
            expression: None,
        });
        Ok(())
    }

    pub(super) fn interpolation_text(&mut self) -> Result<bool, Diagnostic> {
        let Some(frame) = self.interpolations.last().copied() else {
            return Ok(false);
        };
        if frame.expression.is_some() {
            return Ok(false);
        }
        let start = self.cursor;
        let part = self.string_part(frame.start, frame.delimiter, true)?;
        let boundary = self.span(part.boundary, self.cursor);
        let mut need_plus = frame.need_plus;
        if !part.units.is_empty() || (part.end == StringEnd::Quote && !need_plus) {
            if need_plus {
                self.emit(TokenKind::Plus, self.span(start, start))?;
            }
            let index = self.strings.len() as u32;
            self.strings.push(part.units.into());
            self.emit(TokenKind::String(index), self.span(start, part.boundary))?;
            need_plus = true;
        }
        if part.end == StringEnd::Quote {
            self.emit(TokenKind::RightParen, boundary)?;
            self.interpolations.pop();
        } else {
            if need_plus {
                self.emit(TokenKind::Plus, boundary)?;
            }
            self.emit(TokenKind::StringType, boundary)?;
            self.emit(TokenKind::LeftParen, boundary)?;
            let frame = self
                .interpolations
                .last_mut()
                .expect("active interpolation");
            frame.need_plus = true;
            frame.expression = Some(Expression {
                terminator: if part.end == StringEnd::Ampersand {
                    TokenKind::Semicolon
                } else {
                    TokenKind::RightBrace
                },
                depth: 0,
            });
        }
        Ok(true)
    }

    pub(super) fn interpolation_token(
        &mut self,
        kind: TokenKind,
        start: usize,
    ) -> Result<bool, Diagnostic> {
        let Some(frame) = self.interpolations.last_mut() else {
            return Ok(false);
        };
        let expression = frame
            .expression
            .as_mut()
            .expect("text scanned before tokens");
        if kind == expression.terminator && expression.depth == 0 {
            frame.expression = None;
            self.emit(TokenKind::RightParen, self.span(start, self.cursor))?;
            return Ok(true);
        }
        match kind {
            TokenKind::LeftParen | TokenKind::LeftBracket | TokenKind::LeftBrace => {
                expression.depth += 1
            }
            TokenKind::RightParen | TokenKind::RightBracket | TokenKind::RightBrace => {
                expression.depth -= 1
            }
            _ => {}
        }
        Ok(false)
    }
}

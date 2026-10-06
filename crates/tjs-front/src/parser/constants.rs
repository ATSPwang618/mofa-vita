use super::{MAX_EXPRESSION_DEPTH, Parser};
use crate::{
    ast::{ExprId, ExprKind, UnaryOp},
    lexer::TokenKind,
};
use tjs_core::{Diagnostic, Phase, Span};

impl Parser<'_> {
    // Deliberately use the constant grammar, not the ordinary expression parser:
    // calls, identifiers, holes, colon keys and trailing separators are illegal.
    pub(super) fn constant_container(&mut self, start: Span) -> Result<ExprId, Diagnostic> {
        if self.nesting >= MAX_EXPRESSION_DEPTH {
            return Err(Diagnostic::new(
                Phase::Parse,
                start,
                "constant nesting limit exceeded",
            ));
        }
        self.nesting += 1;
        self.expect(TokenKind::Const)?;
        self.expect(TokenKind::RightParen)?;
        let dictionary = self.current().kind == TokenKind::Percent;
        if dictionary {
            self.bump();
        }
        self.expect(TokenKind::LeftBracket)?;
        let mut elements = Vec::new();
        let mut entries = Vec::new();
        let mut depth = 1;
        if self.current().kind != TokenKind::RightBracket {
            // The reference dictionary list production permits one leading comma.
            if dictionary && self.current().kind == TokenKind::Comma {
                self.bump();
            }
            loop {
                let key = if dictionary {
                    let key = self.constant_scalar()?;
                    self.expect(TokenKind::Comma)?;
                    Some(key)
                } else {
                    None
                };
                let value = self.constant_element()?;
                depth = depth.max(self.program.expression(value).depth + 1);
                if let Some(key) = key {
                    entries.push((key, value));
                } else {
                    elements.push(value);
                }
                if self.current().kind != TokenKind::Comma {
                    break;
                }
                self.bump();
            }
        }
        let end = self.expect(TokenKind::RightBracket)?.span;
        let kind = if dictionary {
            let start = self.program.dictionary_entries.len();
            self.program.dictionary_entries.extend(entries);
            ExprKind::ConstantDictionary {
                start,
                end: self.program.dictionary_entries.len(),
            }
        } else {
            let start = self.program.array_elements.len();
            self.program.array_elements.extend(elements);
            ExprKind::ConstantArray {
                start,
                end: self.program.array_elements.len(),
            }
        };
        self.nesting -= 1;
        self.push(kind, start.join(end).expect("one source"), depth)
    }

    fn constant_element(&mut self) -> Result<ExprId, Diagnostic> {
        let token = self.current();
        match token.kind {
            TokenKind::LeftParen => {
                self.bump();
                self.constant_container(token.span)
            }
            TokenKind::Void => {
                self.bump();
                self.push(ExprKind::Void, token.span, 1)
            }
            TokenKind::Plus | TokenKind::Minus => {
                self.bump();
                let inner = self.constant_scalar()?;
                let span = token
                    .span
                    .join(self.program.expression(inner).span)
                    .expect("one source");
                let kind = if token.kind == TokenKind::Minus {
                    ExprKind::Negate(inner)
                } else {
                    ExprKind::Unary {
                        op: UnaryOp::Number,
                        inner,
                    }
                };
                self.push(kind, span, 2)
            }
            _ => self.constant_scalar(),
        }
    }

    fn constant_scalar(&mut self) -> Result<ExprId, Diagnostic> {
        let token = self.current();
        let kind = match token.kind {
            TokenKind::Integer(value) => ExprKind::Integer(value),
            TokenKind::Real(value) => ExprKind::Real(value),
            TokenKind::String(index) => ExprKind::String(index),
            TokenKind::Octet(index) => ExprKind::Octet(index),
            TokenKind::Null => ExprKind::Null,
            _ => {
                return Err(Diagnostic::new(
                    Phase::Parse,
                    token.span,
                    "expected a constant literal",
                ));
            }
        };
        self.bump();
        self.push(kind, token.span, 1)
    }
}

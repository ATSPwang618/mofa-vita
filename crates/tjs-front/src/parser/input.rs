use crate::lexer::{Lexed, Lexer, Token, TokenKind};
use tjs_core::{Diagnostic, Span};

pub(super) enum Input<'a> {
    Buffered(&'a Lexed),
    Source(Lexer<'a>),
}

impl Input<'_> {
    pub(super) fn token(&mut self, index: usize) -> Result<Token, Diagnostic> {
        let tokens = match self {
            Self::Buffered(lexed) => lexed.tokens(),
            Self::Source(lexer) => {
                while index >= lexer.buffered().len() {
                    if lexer
                        .buffered()
                        .last()
                        .is_some_and(|t| t.kind == TokenKind::Eof)
                    {
                        break;
                    }
                    lexer.next_token()?;
                }
                lexer.buffered()
            }
        };
        Ok(tokens[index.min(tokens.len() - 1)])
    }

    pub(super) fn regexp(&mut self, span: Span) -> Result<u32, Diagnostic> {
        match self {
            Self::Source(lexer) => lexer.regexp(span),
            Self::Buffered(_) => Err(Diagnostic::new(
                tjs_core::Phase::Parse,
                span,
                "regular expressions require parser-directed lexing; use parse_source or compile",
            )),
        }
    }

    pub(super) fn literals(&self) -> crate::lexer::LiteralSlices<'_> {
        match self {
            Self::Buffered(lexed) => (lexed.strings(), lexed.octets()),
            Self::Source(lexer) => lexer.literals(),
        }
    }
}

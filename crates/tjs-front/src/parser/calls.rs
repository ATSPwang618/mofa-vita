use tjs_core::{Diagnostic, Span};

use super::Parser;
use crate::{
    ast::{Argument, Arguments, ExprId, ExprKind},
    lexer::TokenKind,
};

impl Parser<'_> {
    pub(super) fn call_expression(
        &mut self,
        lhs: ExprId,
        construct: bool,
        new_span: Option<Span>,
        recursion: usize,
    ) -> Result<ExprId, Diagnostic> {
        self.bump();
        let left = *self.program.expression(lhs);
        let mut arguments = Vec::new();
        let mut depth = left.depth;
        let forward = self.current().kind == TokenKind::Ellipsis;
        if forward {
            self.bump();
        } else if self.current().kind != TokenKind::RightParen {
            loop {
                // Only a standalone star forwards rest arguments. With an
                // operand (e.g. *(&Layer.width)), it reads a property instead.
                let argument = if self.current().kind == TokenKind::Star
                    && matches!(self.peek(1).kind, TokenKind::Comma | TokenKind::RightParen)
                {
                    Argument::ForwardRest(self.bump().span)
                } else {
                    let value = if matches!(
                        self.current().kind,
                        TokenKind::Comma | TokenKind::RightParen
                    ) {
                        let span = self.current().span;
                        self.push(ExprKind::Void, span, 1)?
                    } else {
                        self.expression(2, recursion + 1)?
                    };
                    depth = depth.max(self.program.expression(value).depth);
                    if self.current().kind == TokenKind::Star {
                        self.bump();
                        Argument::Spread(value)
                    } else {
                        Argument::Value(value)
                    }
                };
                arguments.push(argument);
                if self.current().kind != TokenKind::Comma {
                    break;
                }
                self.bump();
            }
        }
        let end = self.expect(TokenKind::RightParen)?.span;
        let start = self.program.arguments.len();
        self.program.arguments.extend(arguments);
        let arguments = if forward {
            Arguments::ForwardOriginal
        } else {
            Arguments::List {
                start,
                end: self.program.arguments.len(),
            }
        };
        self.push(
            if construct {
                ExprKind::Construct {
                    callee: lhs,
                    arguments,
                }
            } else {
                ExprKind::Call {
                    callee: lhs,
                    arguments,
                }
            },
            new_span
                .filter(|_| construct)
                .unwrap_or(left.span)
                .join(end)
                .expect("one source"),
            depth + 1,
        )
    }
}

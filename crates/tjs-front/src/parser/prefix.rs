use super::Parser;
use crate::{
    ast::{ExprId, ExprKind, MemberName},
    lexer::{Token, TokenKind},
};
use tjs_core::{Diagnostic, Phase};

impl Parser<'_> {
    pub(super) fn prefix(&mut self, token: Token, recursion: usize) -> Result<ExprId, Diagnostic> {
        match token.kind {
            TokenKind::Void => self.push(ExprKind::Void, token.span, 1),
            TokenKind::Integer(value) => self.push(ExprKind::Integer(value), token.span, 1),
            TokenKind::Real(bits) => self.push(ExprKind::Real(bits), token.span, 1),
            TokenKind::String(index) => self.push(ExprKind::String(index), token.span, 1),
            TokenKind::Octet(index) => self.push(ExprKind::Octet(index), token.span, 1),
            TokenKind::RegExp(index) => self.push(ExprKind::RegExp(index), token.span, 1),
            TokenKind::Slash | TokenKind::SlashEqual => {
                let index = self.input.regexp(token.span)?;
                let span = self.input.token(self.cursor - 1)?.span;
                self.push(ExprKind::RegExp(index), span, 1)
            }
            TokenKind::Null => self.push(ExprKind::Null, token.span, 1),
            TokenKind::This => self.push(ExprKind::This, token.span, 1),
            TokenKind::Super => self.push(ExprKind::Super, token.span, 1),
            TokenKind::Global => self.push(ExprKind::Global, token.span, 1),
            TokenKind::Function => {
                let (function, span) = self.function(None, token.span)?;
                self.push(ExprKind::Function(function), span, 1)
            }
            TokenKind::Dot => {
                let name = self.expect(TokenKind::Identifier)?.span;
                let object = self.push(ExprKind::WithObject, token.span, 1)?;
                self.push(
                    ExprKind::Member {
                        object,
                        name: MemberName::Named(name),
                    },
                    token.span.join(name).expect("one source"),
                    2,
                )
            }
            TokenKind::Identifier => self.push(ExprKind::Name(token.span), token.span, 1),
            TokenKind::Percent => self.dictionary(token.span, recursion),
            TokenKind::LeftBracket => self.array(token.span, recursion),
            TokenKind::Plus
            | TokenKind::Sharp
            | TokenKind::Dollar
            | TokenKind::Int
            | TokenKind::RealType
            | TokenKind::StringType
            | TokenKind::BitNot
            | TokenKind::BitAnd
            | TokenKind::Star
            | TokenKind::TypeOf
            | TokenKind::IsValid
            | TokenKind::Invalidate
            | TokenKind::Minus
            | TokenKind::Bang
            | TokenKind::Delete
            | TokenKind::Increment
            | TokenKind::Decrement => self.prefix_unary(token, recursion),
            TokenKind::LeftParen if self.current().kind == TokenKind::Const => {
                self.constant_container(token.span)
            }
            TokenKind::LeftParen
                if matches!(
                    self.current().kind,
                    TokenKind::Int | TokenKind::RealType | TokenKind::StringType
                ) && self.peek(1).kind == TokenKind::RightParen =>
            {
                let op = self.bump().kind.unary().expect("cast keyword");
                self.bump();
                let inner = self.expression(30, recursion + 1)?;
                let node = *self.program.expression(inner);
                self.push(
                    ExprKind::Unary { op, inner },
                    token.span.join(node.span).expect("one source"),
                    node.depth + 1,
                )
            }
            TokenKind::LeftParen => {
                let inner = self.expression(0, recursion + 1)?;
                let end = self.expect(TokenKind::RightParen)?.span;
                self.program.expressions[inner.0].span = token.span.join(end).expect("one source");
                Ok(inner)
            }
            _ => Err(Diagnostic::new(
                Phase::Parse,
                token.span,
                format!("expected an expression, found {}", token.kind.description()),
            )),
        }
    }

    fn prefix_unary(&mut self, token: Token, recursion: usize) -> Result<ExprId, Diagnostic> {
        let inner = self.expression(30, recursion + 1)?;
        let node = *self.program.expression(inner);
        let update = matches!(token.kind, TokenKind::Increment | TokenKind::Decrement);
        if update {
            self.check_target(inner)?;
        }
        self.push(
            if update {
                ExprKind::Update {
                    target: inner,
                    increment: token.kind == TokenKind::Increment,
                    postfix: false,
                }
            } else if let Some(op) = token.kind.unary() {
                ExprKind::Unary { op, inner }
            } else if token.kind == TokenKind::BitAnd {
                ExprKind::RawProperty(inner)
            } else if token.kind == TokenKind::Star {
                ExprKind::Dereference(inner)
            } else if token.kind == TokenKind::TypeOf {
                ExprKind::TypeOf(inner)
            } else if token.kind == TokenKind::Minus {
                ExprKind::Negate(inner)
            } else if token.kind == TokenKind::Delete {
                ExprKind::Delete(inner)
            } else {
                ExprKind::Not(inner)
            },
            token.span.join(node.span).expect("one source"),
            node.depth + 1,
        )
    }
}

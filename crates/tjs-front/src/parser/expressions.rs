use tjs_core::{Diagnostic, Phase, Span};

use super::{MAX_EXPRESSION_DEPTH, MAX_NODES, Parser};
use crate::{
    ast::{Arguments, Expr, ExprId, ExprKind, MemberName},
    lexer::TokenKind,
};

impl Parser<'_> {
    pub(super) fn push(
        &mut self,
        kind: ExprKind,
        span: Span,
        depth: usize,
    ) -> Result<ExprId, Diagnostic> {
        if depth > MAX_EXPRESSION_DEPTH || self.program.expressions.len() >= MAX_NODES {
            return Err(Diagnostic::new(
                Phase::Parse,
                span,
                "expression complexity limit exceeded",
            ));
        }
        let id = ExprId(self.program.expressions.len());
        self.program.expressions.push(Expr { kind, span, depth });
        Ok(id)
    }

    pub(super) fn expression(
        &mut self,
        min_bp: u8,
        recursion: usize,
    ) -> Result<ExprId, Diagnostic> {
        if self.nesting >= MAX_EXPRESSION_DEPTH {
            return Err(Diagnostic::new(
                Phase::Parse,
                self.current().span,
                "expression nesting limit exceeded",
            ));
        }
        self.nesting += 1;
        let mut token = self.bump();
        let new_span = (token.kind == TokenKind::New).then_some(token.span);
        let mut construct = new_span.is_some();
        if construct {
            token = self.bump();
        }
        let mut lhs = self.prefix(token, recursion)?;
        // Parenthesized expressions start a fresh precedence level. TJS permits
        // a mul_div_expr before the argument-spread star, not an assignment or sum.
        let mut lhs_bp = u8::MAX;
        loop {
            let operation = self.current();
            if matches!(operation.kind, TokenKind::Dot | TokenKind::LeftBracket) && min_bp <= 40 {
                self.bump();
                let left = *self.program.expression(lhs);
                let (name, end, depth) = if operation.kind == TokenKind::Dot {
                    let name = self.expect(TokenKind::Identifier)?.span;
                    (MemberName::Named(name), name, left.depth + 1)
                } else {
                    let key = self.expression(0, recursion + 1)?;
                    let end = self.expect(TokenKind::RightBracket)?.span;
                    (
                        MemberName::Computed(key),
                        end,
                        left.depth.max(self.program.expression(key).depth) + 1,
                    )
                };
                lhs = self.push(
                    ExprKind::Member { object: lhs, name },
                    left.span.join(end).expect("one source"),
                    depth,
                )?;
                continue;
            }
            if operation.kind == TokenKind::LeftParen && min_bp <= 40 {
                lhs = self.call_expression(lhs, construct, new_span, recursion)?;
                construct = false;
                continue;
            }
            if construct {
                let node = *self.program.expression(lhs);
                let start = self.program.arguments.len();
                lhs = self.push(
                    ExprKind::Construct {
                        callee: lhs,
                        arguments: Arguments::List { start, end: start },
                    },
                    new_span
                        .expect("new prefix")
                        .join(node.span)
                        .expect("one source"),
                    node.depth + 1,
                )?;
                construct = false;
                continue;
            }
            if matches!(operation.kind, TokenKind::Increment | TokenKind::Decrement) && min_bp <= 40
            {
                self.check_target(lhs)?;
                self.bump();
                let node = *self.program.expression(lhs);
                lhs = self.push(
                    ExprKind::Update {
                        target: lhs,
                        increment: operation.kind == TokenKind::Increment,
                        postfix: true,
                    },
                    node.span.join(operation.span).expect("one source"),
                    node.depth + 1,
                )?;
                continue;
            }
            if operation.kind == TokenKind::Bang && min_bp <= 40 {
                self.bump();
                let node = *self.program.expression(lhs);
                lhs = self.push(
                    ExprKind::Eval(lhs),
                    node.span.join(operation.span).expect("one source"),
                    node.depth + 1,
                )?;
                continue;
            }
            if operation.kind == TokenKind::IsValid && min_bp <= 30 {
                self.bump();
                let node = *self.program.expression(lhs);
                lhs = self.push(
                    ExprKind::Unary {
                        op: crate::ast::UnaryOp::IsValid,
                        inner: lhs,
                    },
                    node.span.join(operation.span).expect("one source"),
                    node.depth + 1,
                )?;
                lhs_bp = 30;
                continue;
            }
            let (left_bp, right_bp) = match operation.kind {
                TokenKind::If => (0, 0),
                TokenKind::Comma => (1, 2),
                TokenKind::Equal | TokenKind::Swap => (2, 2),
                TokenKind::Question if min_bp <= 3 => {
                    lhs = self.conditional(lhs, recursion)?;
                    lhs_bp = 3;
                    continue;
                }
                TokenKind::LogicalOr => (4, 5),
                TokenKind::LogicalAnd => (6, 7),
                TokenKind::Star
                    if matches!(self.peek(1).kind, TokenKind::Comma | TokenKind::RightParen) =>
                {
                    if lhs_bp < 24 {
                        return Err(Diagnostic::new(
                            Phase::Parse,
                            operation.span,
                            "parenthesize this expression before the argument-spread star",
                        ));
                    }
                    break;
                }
                kind if kind.binary().is_some() => {
                    let (_, bp, assign) = kind.binary().unwrap();
                    (
                        bp,
                        if assign || matches!(kind, TokenKind::InstanceOf | TokenKind::In) {
                            bp
                        } else {
                            bp + 1
                        },
                    )
                }
                TokenKind::InContextOf => (35, 35),
                _ => break,
            };
            if left_bp < min_bp {
                break;
            }
            self.bump();
            let left = *self.program.expression(lhs);
            if matches!(operation.kind, TokenKind::Equal | TokenKind::Swap)
                || operation.kind.binary().is_some_and(|(_, _, assign)| assign)
            {
                self.check_target(lhs)?;
            }
            let rhs = self.expression(right_bp, recursion + 1)?;
            let right = *self.program.expression(rhs);
            let kind = match operation.kind {
                TokenKind::If => ExprKind::PostfixIf {
                    body: lhs,
                    condition: rhs,
                },
                TokenKind::Swap => {
                    self.check_target(rhs)?;
                    ExprKind::Swap { lhs, rhs }
                }
                TokenKind::Comma => ExprKind::Sequence { lhs, rhs },
                TokenKind::LogicalAnd | TokenKind::LogicalOr => ExprKind::Logical {
                    and: operation.kind == TokenKind::LogicalAnd,
                    lhs,
                    rhs,
                },
                TokenKind::InContextOf => ExprKind::InContextOf {
                    object: lhs,
                    context: rhs,
                },
                TokenKind::Equal => ExprKind::Assign {
                    target: lhs,
                    value: rhs,
                },
                kind => {
                    let (op, _, assign) = kind.binary().expect("infix operator");
                    if assign {
                        ExprKind::CompoundAssign {
                            target: lhs,
                            op,
                            value: rhs,
                        }
                    } else {
                        ExprKind::Binary { op, lhs, rhs }
                    }
                }
            };
            lhs = self.push(
                kind,
                left.span.join(right.span).expect("one source"),
                left.depth.max(right.depth) + 1,
            )?;
            lhs_bp = left_bp;
        }
        self.nesting -= 1;
        Ok(lhs)
    }

    fn conditional(&mut self, condition: ExprId, recursion: usize) -> Result<ExprId, Diagnostic> {
        self.bump();
        // Both arms are cond_expr in tjs.y; bare assignment/comma needs parentheses.
        let then_value = self.expression(3, recursion + 1)?;
        self.expect(TokenKind::Colon)?;
        let else_value = self.expression(3, recursion + 1)?;
        let first = self.program.expression(condition);
        let last = self.program.expression(else_value);
        let depth = first
            .depth
            .max(self.program.expression(then_value).depth)
            .max(last.depth)
            + 1;
        self.push(
            ExprKind::Conditional {
                condition,
                then_value,
                else_value,
            },
            first.span.join(last.span).expect("one source"),
            depth,
        )
    }

    pub(super) fn check_target(&self, mut id: ExprId) -> Result<(), Diagnostic> {
        while let ExprKind::Sequence { rhs, .. } = self.program.expression(id).kind {
            id = rhs;
        }
        let node = self.program.expression(id);
        if let ExprKind::Conditional {
            then_value,
            else_value,
            ..
        } = node.kind
        {
            self.check_target(then_value)?;
            return self.check_target(else_value);
        }
        if !matches!(
            node.kind,
            ExprKind::Name(_)
                | ExprKind::Member { .. }
                | ExprKind::RawProperty(_)
                | ExprKind::Dereference(_)
        ) {
            return Err(Diagnostic::new(
                Phase::Parse,
                node.span,
                "assignment target must be a variable, object member or property access",
            ));
        }
        Ok(())
    }

    pub(super) fn array(&mut self, start: Span, recursion: usize) -> Result<ExprId, Diagnostic> {
        let mut elements = Vec::new();
        let mut depth = 1;
        if self.current().kind != TokenKind::RightBracket {
            loop {
                let element = if matches!(
                    self.current().kind,
                    TokenKind::Comma | TokenKind::RightBracket
                ) {
                    let span = self.current().span;
                    self.push(ExprKind::Void, span, 1)?
                } else {
                    self.expression(2, recursion + 1)?
                };
                depth = depth.max(self.program.expression(element).depth + 1);
                elements.push(element);
                if self.current().kind != TokenKind::Comma {
                    break;
                }
                self.bump();
            }
        }
        let end = self.expect(TokenKind::RightBracket)?.span;
        let first = self.program.array_elements.len();
        self.program.array_elements.extend(elements);
        self.push(
            ExprKind::Array {
                start: first,
                end: self.program.array_elements.len(),
            },
            start.join(end).expect("one source"),
            depth,
        )
    }

    pub(super) fn dictionary(
        &mut self,
        start: Span,
        recursion: usize,
    ) -> Result<ExprId, Diagnostic> {
        self.expect(TokenKind::LeftBracket)?;
        let mut entries = Vec::new();
        let mut depth = 1;
        // The reference grammar also accepts an initial separator and a trailing one.
        if self.current().kind == TokenKind::Comma {
            self.bump();
        }
        while self.current().kind != TokenKind::RightBracket {
            let key = if self.current().kind == TokenKind::Identifier
                && self.peek(1).kind == TokenKind::Colon
            {
                let name = self.bump().span;
                self.bump();
                self.push(ExprKind::NameString(name), name, 1)?
            } else {
                let key = self.expression(2, recursion + 1)?;
                self.expect(TokenKind::Comma)?;
                key
            };
            let value = self.expression(2, recursion + 1)?;
            depth = depth
                .max(self.program.expression(key).depth + 1)
                .max(self.program.expression(value).depth + 1);
            entries.push((key, value));
            if self.current().kind != TokenKind::Comma {
                break;
            }
            self.bump();
        }
        let end = self.expect(TokenKind::RightBracket)?.span;
        let first = self.program.dictionary_entries.len();
        self.program.dictionary_entries.extend(entries);
        self.push(
            ExprKind::Dictionary {
                start: first,
                end: self.program.dictionary_entries.len(),
            },
            start.join(end).expect("one source"),
            depth,
        )
    }
}

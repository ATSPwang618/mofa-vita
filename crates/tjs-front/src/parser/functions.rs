use tjs_core::{Diagnostic, Phase, Span};

use super::Parser;
use crate::{
    ast::{Function, FunctionId, FunctionKind, Parameter, RestParameter},
    lexer::TokenKind,
};

impl Parser<'_> {
    pub(super) fn function(
        &mut self,
        name: Option<Span>,
        start: Span,
    ) -> Result<(FunctionId, Span), Diagnostic> {
        // Reserve an identity before parsing defaults/body, which may contain functions.
        let id = FunctionId(self.functions.len());
        self.functions.push(None);
        let parent = self.current_function.replace(id);
        let mut parameters = Vec::new();
        let mut rest = None;
        if self.current().kind == TokenKind::LeftParen {
            self.bump();
            if self.current().kind != TokenKind::RightParen {
                loop {
                    if self.current().kind == TokenKind::Star {
                        self.bump();
                        rest = Some(RestParameter::Unnamed);
                        break;
                    }
                    let name = self.expect(TokenKind::Identifier)?.span;
                    self.type_annotation()?;
                    if self.current().kind == TokenKind::Star {
                        self.bump();
                        rest = Some(RestParameter::Named(name));
                        break;
                    }
                    let default = if self.current().kind == TokenKind::Equal {
                        self.bump();
                        Some(self.expression(2, 0)?)
                    } else {
                        None
                    };
                    parameters.push(Parameter { name, default });
                    if self.current().kind != TokenKind::Comma {
                        break;
                    }
                    self.bump();
                }
            }
            self.expect(TokenKind::RightParen)?;
        }
        self.type_annotation()?;
        if self.current().kind != TokenKind::LeftBrace {
            return Err(Diagnostic::new(
                Phase::Parse,
                self.current().span,
                "function body requires braces",
            ));
        }
        let body = self.statement(1)?;
        let span = start
            .join(self.program.statement_span(body))
            .expect("one source");
        self.functions[id.0] = Some(Function {
            kind: if name.is_some() {
                FunctionKind::Function
            } else {
                FunctionKind::Expression
            },
            name,
            parent,
            parameters,
            rest,
            body,
            span,
        });
        self.current_function = parent;
        Ok((id, span))
    }

    pub(super) fn type_annotation(&mut self) -> Result<(), Diagnostic> {
        if self.current().kind != TokenKind::Colon {
            return Ok(());
        }
        self.bump();
        let token = self.bump();
        if !matches!(
            token.kind,
            TokenKind::Identifier
                | TokenKind::Void
                | TokenKind::Int
                | TokenKind::RealType
                | TokenKind::StringType
                | TokenKind::OctetType
        ) {
            return Err(Diagnostic::new(
                Phase::Parse,
                token.span,
                "expected a type name after ':'",
            ));
        }
        // The reference grammar accepts annotations but does not enforce types.
        Ok(())
    }
}

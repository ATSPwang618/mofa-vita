use tjs_core::{Diagnostic, Phase};

use super::Parser;
use crate::{
    ast::{Function, FunctionId, FunctionKind, Parameter, Statement, StmtId},
    lexer::TokenKind,
};

impl Parser<'_> {
    pub(super) fn class(&mut self) -> Result<Statement, Diagnostic> {
        let start = self.bump().span;
        let name = self.expect(TokenKind::Identifier)?.span;
        let id = FunctionId(self.functions.len());
        self.functions.push(None);
        let parent = self.current_function.replace(id);
        let mut bases = Vec::new();
        if self.current().kind == TokenKind::Extends {
            self.bump();
            loop {
                bases.push(self.expression(2, 0)?);
                if self.current().kind != TokenKind::Comma {
                    break;
                }
                self.bump();
            }
        }
        let body = self.definition_body()?;
        let span = start
            .join(self.program.statement_span(body))
            .expect("one source");
        self.functions[id.0] = Some(Function {
            kind: FunctionKind::Class { bases },
            name: Some(name),
            parent,
            parameters: Vec::new(),
            rest: None,
            body,
            span,
        });
        self.current_function = parent;
        Ok(Statement::Function { function: id, span })
    }

    pub(super) fn property(&mut self) -> Result<Statement, Diagnostic> {
        let start = self.bump().span;
        let name = self.expect(TokenKind::Identifier)?.span;
        let id = FunctionId(self.functions.len());
        self.functions.push(None);
        let parent = self.current_function.replace(id);
        self.expect(TokenKind::LeftBrace)?;
        let mut getter = None;
        let mut setter = None;
        while matches!(self.current().kind, TokenKind::Getter | TokenKind::Setter) {
            let token = self.bump();
            let is_setter = token.kind == TokenKind::Setter;
            let slot = if is_setter { &mut setter } else { &mut getter };
            if slot.is_some() {
                return Err(Diagnostic::new(
                    Phase::Parse,
                    token.span,
                    "duplicate property accessor",
                ));
            }
            let accessor = FunctionId(self.functions.len());
            self.functions.push(None);
            self.current_function = Some(accessor);
            let mut parameters = Vec::new();
            if is_setter {
                self.expect(TokenKind::LeftParen)?;
                let name = self.expect(TokenKind::Identifier)?.span;
                self.type_annotation()?;
                parameters.push(Parameter {
                    name,
                    default: None,
                });
                self.expect(TokenKind::RightParen)?;
            } else {
                if self.current().kind == TokenKind::LeftParen {
                    self.bump();
                    self.expect(TokenKind::RightParen)?;
                }
                self.type_annotation()?;
            }
            let body = self.definition_body()?;
            let span = token
                .span
                .join(self.program.statement_span(body))
                .expect("one source");
            self.functions[accessor.0] = Some(Function {
                kind: FunctionKind::Accessor,
                name: None,
                parent: Some(id),
                parameters,
                rest: None,
                body,
                span,
            });
            *slot = Some(accessor);
            self.current_function = Some(id);
        }
        if getter.is_none() && setter.is_none() {
            return Err(Diagnostic::new(
                Phase::Parse,
                self.current().span,
                "property requires a getter or setter",
            ));
        }
        let end = self.expect(TokenKind::RightBrace)?.span;
        let span = start.join(end).expect("one source");
        let body = StmtId(self.program.statements.len());
        self.program.statements.push(Statement::Empty(span));
        self.functions[id.0] = Some(Function {
            kind: FunctionKind::Property { getter, setter },
            name: Some(name),
            parent,
            parameters: Vec::new(),
            rest: None,
            body,
            span,
        });
        self.current_function = parent;
        Ok(Statement::Function { function: id, span })
    }

    fn definition_body(&mut self) -> Result<StmtId, Diagnostic> {
        if self.current().kind != TokenKind::LeftBrace {
            return Err(Diagnostic::new(
                Phase::Parse,
                self.current().span,
                "definition body requires braces",
            ));
        }
        self.statement(1)
    }
}

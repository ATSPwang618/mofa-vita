mod calls;
mod classes;
mod constants;
mod expressions;
mod functions;
mod input;
mod prefix;
use input::Input;

use tjs_core::{Diagnostic, Phase};

use crate::{
    ast::{ExprId, Function, FunctionId, Program, Statement, StmtId, Variable},
    lexer::{Lexed, Token, TokenKind},
};

pub const MAX_EXPRESSION_DEPTH: usize = 128;
const MAX_NODES: usize = 100_000;

/// Parse an already disambiguated token buffer.
pub fn parse(lexed: &Lexed) -> Result<Program, Diagnostic> {
    let span = lexed.tokens()[0]
        .span
        .join(lexed.tokens().last().expect("EOF").span)
        .expect("one source");
    parse_input(Input::Buffered(lexed), span).map(|(program, _)| program)
}

/// Parser-directed tokenization resolves regex/division without lexical heuristics.
pub fn parse_source(
    sources: &tjs_core::SourceMap,
    source: tjs_core::SourceId,
) -> Result<(Program, Lexed), Diagnostic> {
    parse_source_with_preprocessor(sources, source, &mut crate::Preprocessor::default())
}

pub fn parse_source_with_preprocessor(
    sources: &tjs_core::SourceMap,
    source: tjs_core::SourceId,
    preprocessor: &mut crate::Preprocessor,
) -> Result<(Program, Lexed), Diagnostic> {
    parse_mode(sources, source, preprocessor, None)
}

pub(crate) fn parse_eval(
    sources: &tjs_core::SourceMap,
    source: tjs_core::SourceId,
    preprocessor: &mut crate::Preprocessor,
    result_needed: bool,
) -> Result<(Program, Lexed), Diagnostic> {
    parse_mode(sources, source, preprocessor, Some(result_needed))
}

fn parse_mode(
    sources: &tjs_core::SourceMap,
    source: tjs_core::SourceId,
    preprocessor: &mut crate::Preprocessor,
    evaluation: Option<bool>,
) -> Result<(Program, Lexed), Diagnostic> {
    let mut lexer = crate::lexer::Lexer::new(sources, source, preprocessor)?;
    if let Some(result_needed) = evaluation {
        lexer.expression_mode(result_needed);
    }
    let file = sources.get(source).expect("lexer validated source");
    let span = sources
        .span(source, 0..file.units().len())
        .expect("whole source");
    let (program, input) = parse_input(Input::Source(lexer), span)?;
    let Input::Source(lexer) = input else {
        unreachable!()
    };
    Ok((program, lexer.finish()))
}

fn parse_input(input: Input<'_>, span: tjs_core::Span) -> Result<(Program, Input<'_>), Diagnostic> {
    let mut parser = Parser {
        input,
        lexical_error: None,
        cursor: 0,
        nesting: 0,
        current_function: None,
        functions: Vec::new(),
        program: Program {
            functions: Vec::new(),
            strings: Vec::new(),
            octets: Vec::new(),
            expressions: Vec::new(),
            statements: Vec::new(),
            roots: Vec::new(),
            arguments: Vec::new(),
            dictionary_entries: Vec::new(),
            array_elements: Vec::new(),
            span,
        },
    };
    while parser.current().kind != TokenKind::Eof {
        let statement = match parser.statement(0) {
            Ok(statement) => statement,
            Err(error) => return Err(parser.lexical_error.take().unwrap_or(error)),
        };
        parser.program.roots.push(statement);
    }
    if let Some(error) = parser.lexical_error.take() {
        return Err(error);
    }
    let (strings, octets) = parser.input.literals();
    parser.program.strings = strings.to_vec();
    parser.program.octets = octets.to_vec();
    parser.program.functions = parser
        .functions
        .into_iter()
        .map(|function| function.expect("finished parsing function"))
        .collect();
    Ok((parser.program, parser.input))
}

struct Parser<'tokens> {
    input: Input<'tokens>,
    lexical_error: Option<Diagnostic>,
    cursor: usize,
    nesting: usize,
    current_function: Option<FunctionId>,
    functions: Vec<Option<Function>>,
    program: Program,
}

impl Parser<'_> {
    fn current(&mut self) -> Token {
        self.peek(0)
    }

    fn peek(&mut self, offset: usize) -> Token {
        if self.lexical_error.is_none() {
            match self.input.token(self.cursor + offset) {
                Ok(token) => return token,
                Err(error) => self.lexical_error = Some(error),
            }
        }
        Token {
            kind: TokenKind::Eof,
            span: self
                .lexical_error
                .as_ref()
                .and_then(|e| e.span)
                .unwrap_or(self.program.span),
        }
    }

    fn bump(&mut self) -> Token {
        let token = self.current();
        if token.kind != TokenKind::Eof {
            self.cursor += 1;
        }
        token
    }

    fn expect(&mut self, kind: TokenKind) -> Result<Token, Diagnostic> {
        let token = self.current();
        if token.kind != kind {
            return Err(Diagnostic::new(
                Phase::Parse,
                token.span,
                format!(
                    "expected {}, found {}",
                    kind.description(),
                    token.kind.description()
                ),
            ));
        }
        Ok(self.bump())
    }

    fn statement(&mut self, depth: usize) -> Result<StmtId, Diagnostic> {
        if self.nesting >= MAX_EXPRESSION_DEPTH {
            return Err(Diagnostic::new(
                Phase::Parse,
                self.current().span,
                "statement nesting limit exceeded",
            ));
        }
        self.nesting += 1;
        let statement = match self.current().kind {
            TokenKind::Class => self.class()?,
            TokenKind::Property => self.property()?,
            TokenKind::Try => {
                let start = self.bump().span;
                let body = self.statement(depth + 1)?;
                self.expect(TokenKind::Catch)?;
                let mut catch_name = None;
                if self.current().kind == TokenKind::LeftParen {
                    self.bump();
                    if self.current().kind != TokenKind::RightParen {
                        catch_name = Some(self.expect(TokenKind::Identifier)?.span);
                    }
                    self.expect(TokenKind::RightParen)?;
                }
                let catch_body = self.statement(depth + 1)?;
                let span = start
                    .join(self.program.statement_span(catch_body))
                    .expect("one source");
                Statement::Try {
                    body,
                    catch_name,
                    catch_body,
                    span,
                }
            }
            TokenKind::Throw => {
                let start = self.bump().span;
                let value = self.expression(0, 0)?;
                let end = self.expect(TokenKind::Semicolon)?.span;
                Statement::Throw {
                    value,
                    span: start.join(end).expect("one source"),
                }
            }
            TokenKind::Function if self.peek(1).kind == TokenKind::Identifier => {
                let start = self.bump().span;
                let name = self.bump().span;
                let (function, span) = self.function(Some(name), start)?;
                Statement::Function { function, span }
            }
            TokenKind::Return => {
                let start = self.bump().span;
                let value = if self.current().kind == TokenKind::Semicolon {
                    None
                } else {
                    Some(self.expression(0, 0)?)
                };
                let end = self.expect(TokenKind::Semicolon)?.span;
                Statement::Return {
                    value,
                    span: start.join(end).expect("one source"),
                }
            }
            TokenKind::Var | TokenKind::Const => self.variable_statement()?,
            TokenKind::LeftBrace => {
                let start = self.bump().span;
                let mut statements = Vec::new();
                while !matches!(self.current().kind, TokenKind::RightBrace | TokenKind::Eof) {
                    statements.push(self.statement(depth + 1)?);
                }
                let end = self.expect(TokenKind::RightBrace)?.span;
                Statement::Block {
                    statements,
                    span: start.join(end).expect("one source"),
                }
            }
            TokenKind::If => {
                let start = self.bump().span;
                let condition = self.condition()?;
                let then_branch = self.statement(depth + 1)?;
                let else_branch = if self.current().kind == TokenKind::Else {
                    self.bump();
                    Some(self.statement(depth + 1)?)
                } else {
                    None
                };
                let end = self
                    .program
                    .statement_span(else_branch.unwrap_or(then_branch));
                Statement::If {
                    condition,
                    then_branch,
                    else_branch,
                    span: start.join(end).expect("one source"),
                }
            }
            TokenKind::While => {
                let start = self.bump().span;
                let condition = self.condition()?;
                let body = self.statement(depth + 1)?;
                let end = self.program.statement_span(body);
                Statement::While {
                    condition,
                    body,
                    span: start.join(end).expect("one source"),
                }
            }
            TokenKind::For => self.for_statement(depth)?,
            TokenKind::Do => self.do_statement(depth)?,
            TokenKind::Switch | TokenKind::With => {
                let token = self.bump();
                let value = self.condition()?;
                if token.kind == TokenKind::Switch && self.current().kind != TokenKind::LeftBrace {
                    return Err(Diagnostic::new(
                        Phase::Parse,
                        self.current().span,
                        "switch requires a block",
                    ));
                }
                let body = self.statement(depth + 1)?;
                let span = token
                    .span
                    .join(self.program.statement_span(body))
                    .expect("one source");
                if token.kind == TokenKind::Switch {
                    Statement::Switch { value, body, span }
                } else {
                    Statement::With {
                        object: value,
                        body,
                        span,
                    }
                }
            }
            TokenKind::Case | TokenKind::Default => {
                let token = self.bump();
                let value = if token.kind == TokenKind::Case {
                    Some(self.expression(0, 0)?)
                } else {
                    None
                };
                let end = self.expect(TokenKind::Colon)?.span;
                Statement::Case {
                    value,
                    span: token.span.join(end).expect("one source"),
                }
            }
            TokenKind::Break | TokenKind::Continue => {
                let token = self.bump();
                let end = self.expect(TokenKind::Semicolon)?.span;
                let span = token.span.join(end).expect("one source");
                if token.kind == TokenKind::Break {
                    Statement::Break(span)
                } else {
                    Statement::Continue(span)
                }
            }
            TokenKind::Semicolon => Statement::Empty(self.bump().span),
            _ => {
                let expression = self.expression(0, 0)?;
                self.expect(TokenKind::Semicolon)?;
                Statement::Expression(expression)
            }
        };
        let id = StmtId(self.program.statements.len());
        self.program.statements.push(statement);
        self.nesting -= 1;
        Ok(id)
    }

    fn condition(&mut self) -> Result<ExprId, Diagnostic> {
        self.expect(TokenKind::LeftParen)?;
        let expression = self.expression(0, 0)?;
        self.expect(TokenKind::RightParen)?;
        Ok(expression)
    }

    fn for_statement(&mut self, depth: usize) -> Result<Statement, Diagnostic> {
        let start = self.bump().span;
        self.expect(TokenKind::LeftParen)?;
        let initializer = match self.current().kind {
            TokenKind::Semicolon => {
                self.bump();
                None
            }
            TokenKind::Var | TokenKind::Const => Some(self.statement(depth + 1)?),
            _ => {
                let expression = self.expression(0, 0)?;
                self.expect(TokenKind::Semicolon)?;
                let id = StmtId(self.program.statements.len());
                self.program
                    .statements
                    .push(Statement::Expression(expression));
                Some(id)
            }
        };
        let condition = if self.current().kind == TokenKind::Semicolon {
            None
        } else {
            Some(self.expression(0, 0)?)
        };
        self.expect(TokenKind::Semicolon)?;
        let step = if self.current().kind == TokenKind::RightParen {
            None
        } else {
            Some(self.expression(0, 0)?)
        };
        self.expect(TokenKind::RightParen)?;
        let body = self.statement(depth + 1)?;
        Ok(Statement::For {
            initializer,
            condition,
            step,
            body,
            span: start
                .join(self.program.statement_span(body))
                .expect("one source"),
        })
    }

    fn do_statement(&mut self, depth: usize) -> Result<Statement, Diagnostic> {
        let start = self.bump().span;
        let body = self.statement(depth + 1)?;
        self.expect(TokenKind::While)?;
        let condition = self.condition()?;
        let end = self.expect(TokenKind::Semicolon)?.span;
        Ok(Statement::DoWhile {
            body,
            condition,
            span: start.join(end).expect("one source"),
        })
    }

    fn variable_statement(&mut self) -> Result<Statement, Diagnostic> {
        let start = self.bump().span;
        let mut variables = Vec::new();
        loop {
            let name = self.expect(TokenKind::Identifier)?.span;
            self.type_annotation()?;
            let initializer = if self.current().kind == TokenKind::Equal {
                self.bump();
                Some(self.expression(2, 0)?)
            } else {
                None
            };
            variables.push(Variable { name, initializer });
            if self.current().kind != TokenKind::Comma {
                break;
            }
            self.bump();
        }
        let end = self.expect(TokenKind::Semicolon)?.span;
        Ok(Statement::Var {
            variables,
            span: start.join(end).expect("one source"),
        })
    }
}

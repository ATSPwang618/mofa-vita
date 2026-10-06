mod interpolation;
mod literals;
mod operators;
mod preprocessing;
mod regexp;

use std::{borrow::Cow, sync::Arc};
use tjs_core::{Diagnostic, Phase, SourceId, SourceMap, Span};

pub const MAX_TOKENS: usize = 100_000;
const MAX_COMMENT_DEPTH: usize = 1_024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    RegExp(u32),
    Real(u64),
    Slash,
    SlashEqual,
    Backslash,
    BackslashEqual,
    PercentEqual,
    BitAnd,
    BitAndEqual,
    BitOr,
    BitOrEqual,
    BitXor,
    BitXorEqual,
    ShiftLeft,
    ShiftLeftEqual,
    ShiftRight,
    ShiftRightEqual,
    ShiftRightUnsigned,
    ShiftRightUnsignedEqual,
    StrictEqual,
    StrictNotEqual,
    BitNot,
    Int,
    RealType,
    StringType,
    OctetType,
    Integer(i64),
    String(u32),
    Octet(u32),
    Sharp,
    Dollar,
    Null,
    This,
    Global,
    InContextOf,
    TypeOf,
    IsValid,
    Invalidate,
    In,
    InstanceOf,
    Identifier,
    Var,
    Const,
    If,
    Else,
    While,
    For,
    Do,
    Switch,
    Case,
    Default,
    With,
    Break,
    Continue,
    Class,
    Extends,
    Super,
    Property,
    Getter,
    Setter,
    Function,
    Return,
    Void,
    Try,
    Catch,
    Throw,
    Delete,
    New,
    Ellipsis,
    Dot,
    Percent,
    Colon,
    Question,
    LogicalAnd,
    LogicalOr,
    LogicalAndEqual,
    LogicalOrEqual,
    Swap,
    Plus,
    Minus,
    Star,
    Increment,
    Decrement,
    PlusEqual,
    MinusEqual,
    StarEqual,
    Equal,
    EqualEqual,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Bang,
    LeftParen,
    RightParen,
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Semicolon,
    Comma,
    Eof,
}

impl TokenKind {
    pub fn description(self) -> &'static str {
        match self {
            Self::RegExp(_) => "regular expression",
            Self::Real(_) => "real number",
            Self::Slash => "/",
            Self::SlashEqual => "/=",
            Self::Backslash => "\\",
            Self::BackslashEqual => "\\=",
            Self::PercentEqual => "%=",
            Self::BitAnd => "&",
            Self::BitAndEqual => "&=",
            Self::BitOr => "|",
            Self::BitOrEqual => "|=",
            Self::BitXor => "^",
            Self::BitXorEqual => "^=",
            Self::ShiftLeft => "<<",
            Self::ShiftLeftEqual => "<<=",
            Self::ShiftRight => ">>",
            Self::ShiftRightEqual => ">>=",
            Self::ShiftRightUnsigned => ">>>",
            Self::ShiftRightUnsignedEqual => ">>>=",
            Self::StrictEqual => "===",
            Self::StrictNotEqual => "!==",
            Self::BitNot => "~",
            Self::Int => "int",
            Self::RealType => "real",
            Self::StringType => "string",
            Self::OctetType => "octet",
            Self::Integer(_) => "integer",
            Self::String(_) => "string",
            Self::Octet(_) => "octet literal",
            Self::Sharp => "#",
            Self::Dollar => "$",
            Self::Null => "null",
            Self::This => "this",
            Self::Global => "global",
            Self::IsValid => "isvalid",
            Self::Invalidate => "invalidate",
            Self::TypeOf => "typeof",
            Self::In => "in",
            Self::InstanceOf => "instanceof",
            Self::InContextOf => "incontextof",
            Self::Identifier => "identifier",
            Self::Var => "var",
            Self::Const => "const",
            Self::If => "if",
            Self::Else => "else",
            Self::While => "while",
            Self::For => "for",
            Self::Do => "do",
            Self::Switch => "switch",
            Self::Case => "case",
            Self::Default => "default",
            Self::With => "with",
            Self::Break => "break",
            Self::Continue => "continue",
            Self::Class => "class",
            Self::Extends => "extends",
            Self::Super => "super",
            Self::Property => "property",
            Self::Getter => "getter",
            Self::Setter => "setter",
            Self::Function => "function",
            Self::Return => "return",
            Self::Void => "void",
            Self::Try => "try",
            Self::Catch => "catch",
            Self::Throw => "throw",
            Self::Delete => "delete",
            Self::New => "new",
            Self::Ellipsis => "...",
            Self::Dot => ".",
            Self::Percent => "%",
            Self::Colon => ":",
            Self::Question => "?",
            Self::LogicalAnd => "&&",
            Self::LogicalOr => "||",
            Self::LogicalAndEqual => "&&=",
            Self::LogicalOrEqual => "||=",
            Self::Swap => "<->",
            Self::Plus => "+",
            Self::Minus => "-",
            Self::Star => "*",
            Self::Increment => "++",
            Self::Decrement => "--",
            Self::PlusEqual => "+=",
            Self::MinusEqual => "-=",
            Self::StarEqual => "*=",
            Self::Equal => "=",
            Self::EqualEqual => "==",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::Bang => "!",
            Self::LeftParen => "(",
            Self::RightParen => ")",
            Self::LeftBrace => "{",
            Self::RightBrace => "}",
            Self::LeftBracket => "[",
            Self::RightBracket => "]",
            Self::Semicolon => ";",
            Self::Comma => ",",
            Self::Eof => "end of input",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriviaKind {
    Whitespace,
    LineComment,
    BlockComment,
    DocLine,
    DocBlock,
    Preprocessor,
    Disabled,
}

#[derive(Clone, Copy, Debug)]
pub struct Trivia {
    pub kind: TriviaKind,
    pub span: Span,
}

#[derive(Debug)]
pub struct Lexed {
    tokens: Vec<Token>,
    trivia: Vec<Trivia>,
    strings: Vec<Arc<[u16]>>,
    octets: Vec<Box<[u8]>>,
}

impl Lexed {
    pub fn octets(&self) -> &[Box<[u8]>] {
        &self.octets
    }

    pub fn strings(&self) -> &[Arc<[u16]>] {
        &self.strings
    }
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    pub fn trivia(&self) -> &[Trivia] {
        &self.trivia
    }
}

pub(crate) type LiteralSlices<'a> = (&'a [Arc<[u16]>], &'a [Box<[u8]>]);

pub(crate) struct Lexer<'source> {
    sources: &'source SourceMap,
    source: SourceId,
    units: Cow<'source, [u16]>,
    source_len: usize,
    cursor: usize,
    tokens: Vec<Token>,
    trivia: Vec<Trivia>,
    strings: Vec<Arc<[u16]>>,
    octets: Vec<Box<[u8]>>,
    interpolations: Vec<interpolation::Interpolation>,
    preprocessor: &'source mut crate::Preprocessor,
    pp_conditions: Vec<usize>,
}

/// Context-free token inspection. Regex bodies require the parser to decide
/// whether a slash begins a literal; use parser::parse_source for complete input.
pub fn lex(sources: &SourceMap, source: SourceId) -> Result<Lexed, Diagnostic> {
    lex_with_preprocessor(sources, source, &mut crate::Preprocessor::default())
}

pub fn lex_with_preprocessor(
    sources: &SourceMap,
    source: SourceId,
    preprocessor: &mut crate::Preprocessor,
) -> Result<Lexed, Diagnostic> {
    let mut lexer = Lexer::new(sources, source, preprocessor)?;
    lexer.run()?;
    Ok(lexer.finish())
}

impl<'source> Lexer<'source> {
    pub(crate) fn new(
        sources: &'source SourceMap,
        source: SourceId,
        preprocessor: &'source mut crate::Preprocessor,
    ) -> Result<Self, Diagnostic> {
        let file = sources
            .get(source)
            .ok_or_else(|| Diagnostic::new(Phase::Lex, None, "source handle is no longer valid"))?;
        Ok(Lexer {
            sources,
            source,
            units: Cow::Borrowed(file.units()),
            source_len: file.units().len(),
            cursor: 0,
            tokens: Vec::new(),
            trivia: Vec::new(),
            strings: Vec::new(),
            octets: Vec::new(),
            interpolations: Vec::new(),
            preprocessor,
            pp_conditions: Vec::new(),
        })
    }

    pub(crate) fn expression_mode(&mut self, result_needed: bool) {
        // The reference appends a semicolon before lexing, so a trailing line
        // comment can consume it. A synthetic return preserves original spans.
        let mut units = Vec::with_capacity(self.units.len() + 1);
        units.extend_from_slice(&self.units);
        units.push(59);
        self.units = Cow::Owned(units);
        if result_needed {
            self.tokens.push(Token {
                kind: TokenKind::Return,
                span: self.span(0, 0),
            });
        }
    }

    pub(crate) fn finish(self) -> Lexed {
        Lexed {
            tokens: self.tokens,
            trivia: self.trivia,
            strings: self.strings,
            octets: self.octets,
        }
    }

    pub(crate) fn buffered(&self) -> &[Token] {
        &self.tokens
    }

    pub(crate) fn literals(&self) -> LiteralSlices<'_> {
        (&self.strings, &self.octets)
    }
}

pub(crate) fn is_digit(unit: u16) -> bool {
    (48..=57).contains(&unit)
}

pub(crate) fn is_name_start(unit: u16) -> bool {
    matches!(unit, 65..=90 | 97..=122 | 95 | 0x100..=0xffff)
}

pub(crate) fn is_space(unit: u16) -> bool {
    matches!(unit, 9..=13 | 32 | 0xfeff)
}

impl Lexer<'_> {
    fn span(&self, start: usize, end: usize) -> Span {
        // Synthetic eval tokens map to the original EOF, without shifting source coordinates.
        self.sources
            .span(
                self.source,
                start.min(self.source_len)..end.min(self.source_len),
            )
            .expect("lexer cursor is within source")
    }

    fn error(&self, start: usize, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(
            Phase::Lex,
            self.span(start, self.cursor.max(start + 1).min(self.units.len())),
            message,
        )
    }

    fn peek(&self, delta: usize) -> Option<u16> {
        self.units.get(self.cursor + delta).copied()
    }

    fn record_trivia(&mut self, start: usize, kind: TriviaKind) -> Result<(), Diagnostic> {
        if self.trivia.len() >= MAX_TOKENS {
            return Err(self.error(start, "trivia limit exceeded"));
        }
        self.trivia.push(Trivia {
            kind,
            span: self.span(start, self.cursor),
        });
        Ok(())
    }

    fn comment(&mut self) -> Result<bool, Diagnostic> {
        let start = self.cursor;
        let Some(unit) = self.peek(0) else {
            return Ok(false);
        };
        if unit == 47 && self.peek(1) == Some(47) {
            let kind = if self.peek(2) == Some(47) {
                TriviaKind::DocLine
            } else {
                TriviaKind::LineComment
            };
            self.cursor += 2;
            while self.peek(0).is_some_and(|unit| !matches!(unit, 10 | 13)) {
                self.cursor += 1;
            }
            self.record_trivia(start, kind)?;
            return Ok(true);
        }
        if unit == 47 && self.peek(1) == Some(42) {
            let kind = if self.peek(2) == Some(42) {
                TriviaKind::DocBlock
            } else {
                TriviaKind::BlockComment
            };
            self.cursor += 2;
            let mut depth = 1;
            while depth > 0 {
                match (self.peek(0), self.peek(1)) {
                    (None, _) => {
                        return Err(self.error(start, "unterminated block comment"));
                    }
                    (Some(47), Some(42)) => {
                        depth += 1;
                        if depth > MAX_COMMENT_DEPTH {
                            return Err(self.error(start, "block comment nesting limit exceeded"));
                        }
                        self.cursor += 2;
                    }
                    (Some(42), Some(47)) => {
                        depth -= 1;
                        self.cursor += 2;
                    }
                    _ => self.cursor += 1,
                }
            }
            self.record_trivia(start, kind)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn emit(&mut self, kind: TokenKind, span: Span) -> Result<(), Diagnostic> {
        if self.tokens.len() >= MAX_TOKENS {
            return Err(Diagnostic::new(Phase::Lex, span, "token limit exceeded"));
        }
        self.tokens.push(Token { kind, span });
        Ok(())
    }

    fn run(&mut self) -> Result<(), Diagnostic> {
        while !self
            .tokens
            .last()
            .is_some_and(|token| token.kind == TokenKind::Eof)
        {
            self.next_token()?;
        }
        Ok(())
    }

    pub(crate) fn next_token(&mut self) -> Result<(), Diagnostic> {
        let count = self.tokens.len();
        while let Some(unit) = self.peek(0) {
            if self.tokens.len() != count {
                return Ok(());
            }
            if self.interpolation_text()? {
                continue;
            }
            let start = self.cursor;
            if is_space(unit) {
                self.cursor += 1;
                while self.peek(0).is_some_and(is_space) {
                    self.cursor += 1;
                }
                self.record_trivia(start, TriviaKind::Whitespace)?;
                continue;
            }
            if self.comment()? {
                continue;
            }
            if unit == 64 {
                if self.preprocess(start)? {
                    continue;
                }
                self.begin_interpolation(start)?;
                continue;
            }
            let kind = if matches!(unit, 34 | 39) {
                let index = self.strings.len() as u32;
                let text = self.string(start, unit)?;
                self.strings.push(text.into());
                TokenKind::String(index)
            } else if unit == 60 && self.peek(1) == Some(37) {
                let index = self.octets.len() as u32;
                let bytes = self.octet(start)?;
                self.octets.push(bytes.into());
                TokenKind::Octet(index)
            } else if is_digit(unit) || (unit == 46 && self.peek(1).is_some_and(is_digit)) {
                let (number, count) = tjs_core::number::parse(&self.units[start..])
                    .ok_or_else(|| self.error(start, "invalid numeric literal"))?;
                self.cursor += count;
                match number {
                    tjs_core::number::Number::Int(value) => TokenKind::Integer(value),
                    tjs_core::number::Number::Real(value) => TokenKind::Real(value.to_bits()),
                }
            } else if is_name_start(unit) {
                self.cursor += 1;
                while self
                    .peek(0)
                    .is_some_and(|unit| is_name_start(unit) || is_digit(unit))
                {
                    self.cursor += 1;
                }
                let name = &self.units[start..self.cursor];
                // TJS accepts bare words, including keywords, after a dot.
                if self
                    .tokens
                    .last()
                    .is_some_and(|token| token.kind == TokenKind::Dot)
                {
                    TokenKind::Identifier
                } else if let Some(keyword) = keyword(name) {
                    keyword
                } else if reserved_word(name) {
                    return Err(
                        self.error(start, "keyword is not implemented in this language subset")
                    );
                } else {
                    TokenKind::Identifier
                }
            } else {
                self.operator(start)?
            };
            if !self.interpolation_token(kind, start)? {
                self.emit(kind, self.span(start, self.cursor))?;
            }
        }
        if self.tokens.len() != count {
            return Ok(());
        }
        if let Some(&start) = self.pp_conditions.last() {
            return Err(self.error(start, "unterminated @if; expected @endif"));
        }
        if let Some(frame) = self.interpolations.last() {
            return Err(self.error(frame.start, "unterminated interpolated string"));
        }
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: self.span(self.cursor, self.cursor),
        });
        Ok(())
    }
}

fn hex_digit(unit: u16) -> Option<u16> {
    match unit {
        48..=57 => Some(unit - 48),
        65..=70 => Some(unit - 65 + 10),
        97..=102 => Some(unit - 97 + 10),
        _ => None,
    }
}

fn keyword(units: &[u16]) -> Option<TokenKind> {
    // Match UTF-16 directly, without allocating a UTF-8 String per identifier.
    match units {
        [105, 110, 116] => Some(TokenKind::Int),
        [114, 101, 97, 108] => Some(TokenKind::RealType),
        [115, 116, 114, 105, 110, 103] => Some(TokenKind::StringType),
        [111, 99, 116, 101, 116] => Some(TokenKind::OctetType),
        [78, 97, 78] => Some(TokenKind::Real(f64::NAN.to_bits())),
        [73, 110, 102, 105, 110, 105, 116, 121] => Some(TokenKind::Real(f64::INFINITY.to_bits())),
        [116, 104, 105, 115] => Some(TokenKind::This),
        [103, 108, 111, 98, 97, 108] => Some(TokenKind::Global),
        [105, 115, 118, 97, 108, 105, 100] => Some(TokenKind::IsValid),
        [105, 110, 118, 97, 108, 105, 100, 97, 116, 101] => Some(TokenKind::Invalidate),
        [116, 121, 112, 101, 111, 102] => Some(TokenKind::TypeOf),
        [105, 110] => Some(TokenKind::In),
        [105, 110, 115, 116, 97, 110, 99, 101, 111, 102] => Some(TokenKind::InstanceOf),
        [105, 110, 99, 111, 110, 116, 101, 120, 116, 111, 102] => Some(TokenKind::InContextOf),
        [100, 101, 108, 101, 116, 101] => Some(TokenKind::Delete),
        [110, 101, 119] => Some(TokenKind::New),
        [118, 97, 114] => Some(TokenKind::Var),
        [99, 111, 110, 115, 116] => Some(TokenKind::Const),
        [116, 114, 117, 101] => Some(TokenKind::Integer(1)),
        [102, 97, 108, 115, 101] => Some(TokenKind::Integer(0)),
        [105, 102] => Some(TokenKind::If),
        [101, 108, 115, 101] => Some(TokenKind::Else),
        [119, 104, 105, 108, 101] => Some(TokenKind::While),
        [102, 111, 114] => Some(TokenKind::For),
        [100, 111] => Some(TokenKind::Do),
        [115, 119, 105, 116, 99, 104] => Some(TokenKind::Switch),
        [99, 97, 115, 101] => Some(TokenKind::Case),
        [100, 101, 102, 97, 117, 108, 116] => Some(TokenKind::Default),
        [119, 105, 116, 104] => Some(TokenKind::With),
        [98, 114, 101, 97, 107] => Some(TokenKind::Break),
        [99, 111, 110, 116, 105, 110, 117, 101] => Some(TokenKind::Continue),
        [99, 108, 97, 115, 115] => Some(TokenKind::Class),
        [101, 120, 116, 101, 110, 100, 115] => Some(TokenKind::Extends),
        [115, 117, 112, 101, 114] => Some(TokenKind::Super),
        [112, 114, 111, 112, 101, 114, 116, 121] => Some(TokenKind::Property),
        [103, 101, 116, 116, 101, 114] => Some(TokenKind::Getter),
        [115, 101, 116, 116, 101, 114] => Some(TokenKind::Setter),
        [102, 117, 110, 99, 116, 105, 111, 110] => Some(TokenKind::Function),
        [114, 101, 116, 117, 114, 110] => Some(TokenKind::Return),
        [118, 111, 105, 100] => Some(TokenKind::Void),
        [110, 117, 108, 108] => Some(TokenKind::Null),
        [116, 114, 121] => Some(TokenKind::Try),
        [99, 97, 116, 99, 104] => Some(TokenKind::Catch),
        [116, 104, 114, 111, 119] => Some(TokenKind::Throw),
        _ => None,
    }
}

fn reserved_word(units: &[u16]) -> bool {
    const WORDS: &[&str] = &[
        "break",
        "continue",
        "class",
        "const",
        "catch",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "global",
        "goto",
        "if",
        "import",
        "incontextof",
        "int",
        "new",
        "null",
        "private",
        "property",
        "protected",
        "public",
        "real",
        "return",
        "static",
        "string",
        "super",
        "switch",
        "synchronized",
        "this",
        "throw",
        "true",
        "try",
        "void",
        "while",
        "with",
    ];
    WORDS.iter().any(|word| {
        word.len() == units.len() && word.bytes().map(u16::from).eq(units.iter().copied())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_documentation_comments_are_preserved() {
        let mut sources = SourceMap::new();
        let id = sources
            .add_utf8(
                "test",
                "/// docs\r\n/** outer /* inner */ rest */ 7; // end",
            )
            .unwrap();
        let result = lex(&sources, id).unwrap();
        assert_eq!(
            result
                .tokens()
                .iter()
                .map(|token| token.kind)
                .collect::<Vec<_>>(),
            [TokenKind::Integer(7), TokenKind::Semicolon, TokenKind::Eof]
        );
        let comments: Vec<_> = result
            .trivia()
            .iter()
            .filter(|item| item.kind != TriviaKind::Whitespace)
            .map(|item| item.kind)
            .collect();
        assert_eq!(
            comments,
            [
                TriviaKind::DocLine,
                TriviaKind::DocBlock,
                TriviaKind::LineComment
            ]
        );
        assert_eq!(
            result.tokens().last().unwrap().span.start(),
            result.tokens().last().unwrap().span.end()
        );
    }

    #[test]
    fn invalid_input_returns_a_span_instead_of_being_skipped() {
        for input in ["/* unfinished", "0p2;", "0x;", "0b;", "var enum = 1;"] {
            let mut sources = SourceMap::new();
            let id = sources.add_utf8("test", input).unwrap();
            let error = lex(&sources, id).unwrap_err();
            assert_eq!(error.phase, Phase::Lex, "{input}");
            assert!(sources.slice(error.span.unwrap()).is_some());
        }
    }
}

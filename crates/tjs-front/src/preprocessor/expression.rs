use super::Preprocessor;
use crate::lexer::{TokenKind as Kind, is_digit, is_name_start, is_space};
use std::ops::Range;
use tjs_core::number::{self, Number};

pub(crate) struct Error {
    pub offset: usize,
    pub message: &'static str,
}

#[derive(Clone)]
struct Token {
    kind: Kind,
    range: Range<usize>,
}

struct Parser<'a> {
    units: &'a [u16],
    definitions: &'a mut Preprocessor,
    cursor: usize,
    token: Token,
}

pub(super) fn evaluate(definitions: &mut Preprocessor, units: &[u16]) -> Result<i32, Error> {
    let mut parser = Parser {
        units,
        definitions,
        cursor: 0,
        token: Token {
            kind: Kind::Eof,
            range: 0..0,
        },
    };
    parser.advance()?;
    let value = parser.expression(0, 0)?;
    if parser.token.kind != Kind::Eof {
        return Err(parser.error("unexpected preprocessor token"));
    }
    Ok(value)
}

impl Parser<'_> {
    fn error(&self, message: &'static str) -> Error {
        Error {
            offset: self.token.range.start,
            message,
        }
    }

    fn advance(&mut self) -> Result<(), Error> {
        while self.units.get(self.cursor).copied().is_some_and(is_space) {
            self.cursor += 1;
        }
        let start = self.cursor;
        let Some(&unit) = self.units.get(start) else {
            self.token = Token {
                kind: Kind::Eof,
                range: start..start,
            };
            return Ok(());
        };
        let kind = if is_digit(unit) {
            let (number, count) = number::parse(&self.units[start..]).ok_or(Error {
                offset: start,
                message: "invalid preprocessor number",
            })?;
            self.cursor += count;
            let value = match number {
                Number::Int(value) => value,
                Number::Real(value) => tjs_core::value::real_to_integer(value),
            };
            Kind::Integer(i64::from(value as i32))
        } else if is_name_start(unit) {
            self.cursor += 1;
            while self
                .units
                .get(self.cursor)
                .is_some_and(|&u| is_name_start(u) || is_digit(u))
            {
                self.cursor += 1;
            }
            Kind::Identifier
        } else {
            self.cursor += 1;
            let next = self.units.get(self.cursor).copied();
            let pair = match (unit, next) {
                (61, Some(61)) => Some(Kind::EqualEqual),
                (33, Some(61)) => Some(Kind::NotEqual),
                (60, Some(61)) => Some(Kind::LessEqual),
                (62, Some(61)) => Some(Kind::GreaterEqual),
                (38, Some(38)) => Some(Kind::LogicalAnd),
                (124, Some(124)) => Some(Kind::LogicalOr),
                _ => None,
            };
            if let Some(kind) = pair {
                self.cursor += 1;
                kind
            } else {
                match unit {
                    40 => Kind::LeftParen,
                    41 => Kind::RightParen,
                    44 => Kind::Comma,
                    61 => Kind::Equal,
                    33 => Kind::Bang,
                    60 => Kind::Less,
                    62 => Kind::Greater,
                    38 => Kind::BitAnd,
                    124 => Kind::BitOr,
                    94 => Kind::BitXor,
                    43 => Kind::Plus,
                    45 => Kind::Minus,
                    42 => Kind::Star,
                    47 => Kind::Slash,
                    37 => Kind::Percent,
                    _ => {
                        return Err(Error {
                            offset: start,
                            message: "invalid preprocessor token",
                        });
                    }
                }
            }
        };
        self.token = Token {
            kind,
            range: start..self.cursor,
        };
        Ok(())
    }

    fn expression(&mut self, minimum: u8, depth: usize) -> Result<i32, Error> {
        if depth >= crate::parser::MAX_EXPRESSION_DEPTH {
            return Err(self.error("preprocessor expression nesting limit exceeded"));
        }
        let token = self.token.clone();
        self.advance()?;
        let mut left = match token.kind {
            Kind::Integer(value) => value as i32,
            Kind::Identifier => {
                if self.token.kind == Kind::Equal {
                    self.advance()?;
                    // tjspp.y uses SYMBOL '=' expr rather than expr '=' expr.
                    // A symbol can start another assignment even in a tighter RHS.
                    let value = self.expression(8, depth + 1)?;
                    self.definitions
                        .assign(self.units[token.range].to_vec(), value);
                    value
                } else {
                    self.definitions.value(&self.units[token.range])
                }
            }
            Kind::LeftParen => {
                let value = self.expression(0, depth + 1)?;
                if self.token.kind != Kind::RightParen {
                    return Err(self.error("expected ) in preprocessor expression"));
                }
                self.advance()?;
                value
            }
            Kind::Plus | Kind::Minus | Kind::Bang => {
                let value = self.expression(12, depth + 1)?;
                match token.kind {
                    Kind::Minus => value.wrapping_neg(),
                    Kind::Bang => i32::from(value == 0),
                    _ => value,
                }
            }
            _ => {
                return Err(Error {
                    offset: token.range.start,
                    message: "expected a preprocessor expression",
                });
            }
        };
        while let Some(priority) = precedence(self.token.kind) {
            if priority < minimum {
                break;
            }
            let operation = self.token.clone();
            self.advance()?;
            let right = self.expression(priority + 1, depth + 1)?;
            left = match operation.kind {
                Kind::Comma => right,
                Kind::LogicalOr => i32::from(left != 0 || right != 0),
                Kind::LogicalAnd => i32::from(left != 0 && right != 0),
                Kind::BitOr => left | right,
                Kind::BitXor => left ^ right,
                Kind::BitAnd => left & right,
                Kind::EqualEqual => i32::from(left == right),
                Kind::NotEqual => i32::from(left != right),
                Kind::Less => i32::from(left < right),
                Kind::LessEqual => i32::from(left <= right),
                Kind::Greater => i32::from(left > right),
                Kind::GreaterEqual => i32::from(left >= right),
                Kind::Plus => left.wrapping_add(right),
                Kind::Minus => left.wrapping_sub(right),
                Kind::Star => left.wrapping_mul(right),
                Kind::Slash | Kind::Percent => {
                    if right == 0 {
                        return Err(Error {
                            offset: operation.range.start,
                            message: "division by zero in preprocessor expression",
                        });
                    }
                    if operation.kind == Kind::Slash {
                        left.wrapping_div(right)
                    } else {
                        left.wrapping_rem(right)
                    }
                }
                _ => unreachable!("preprocessor operator"),
            };
        }
        Ok(left)
    }
}

// Assignment binds more tightly than &, ^, |, && and || in tjspp.y.
fn precedence(kind: Kind) -> Option<u8> {
    Some(match kind {
        Kind::Comma => 1,
        Kind::LogicalOr => 2,
        Kind::LogicalAnd => 3,
        Kind::BitOr => 4,
        Kind::BitXor => 5,
        Kind::BitAnd => 6,
        Kind::EqualEqual | Kind::NotEqual => 8,
        Kind::Less | Kind::Greater | Kind::LessEqual | Kind::GreaterEqual => 9,
        Kind::Plus | Kind::Minus => 10,
        Kind::Star | Kind::Slash | Kind::Percent => 11,
        _ => return None,
    })
}

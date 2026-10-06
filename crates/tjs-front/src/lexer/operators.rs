use super::{Lexer, TokenKind};
use crate::ast::{BinaryOp, UnaryOp};
use tjs_core::Diagnostic;

impl TokenKind {
    pub(crate) fn binary(self) -> Option<(BinaryOp, u8, bool)> {
        Some(match self {
            Self::LogicalAndEqual => (BinaryOp::LogicalAnd, 2, true),
            Self::LogicalOrEqual => (BinaryOp::LogicalOr, 2, true),
            Self::In => (BinaryOp::In, 30, false),
            Self::InstanceOf => (BinaryOp::InstanceOf, 30, false),
            Self::Plus => (BinaryOp::Add, 20, false),
            Self::PlusEqual => (BinaryOp::Add, 2, true),
            Self::Minus => (BinaryOp::Subtract, 20, false),
            Self::MinusEqual => (BinaryOp::Subtract, 2, true),
            Self::Star => (BinaryOp::Multiply, 24, false),
            Self::StarEqual => (BinaryOp::Multiply, 2, true),
            Self::Slash => (BinaryOp::Divide, 24, false),
            Self::SlashEqual => (BinaryOp::Divide, 2, true),
            Self::Backslash => (BinaryOp::IntDivide, 24, false),
            Self::BackslashEqual => (BinaryOp::IntDivide, 2, true),
            Self::Percent => (BinaryOp::Remainder, 24, false),
            Self::PercentEqual => (BinaryOp::Remainder, 2, true),
            Self::BitAnd => (BinaryOp::BitAnd, 10, false),
            Self::BitAndEqual => (BinaryOp::BitAnd, 2, true),
            Self::BitOr => (BinaryOp::BitOr, 8, false),
            Self::BitOrEqual => (BinaryOp::BitOr, 2, true),
            Self::BitXor => (BinaryOp::BitXor, 9, false),
            Self::BitXorEqual => (BinaryOp::BitXor, 2, true),
            Self::ShiftLeft => (BinaryOp::ShiftLeft, 18, false),
            Self::ShiftLeftEqual => (BinaryOp::ShiftLeft, 2, true),
            Self::ShiftRight => (BinaryOp::ShiftRight, 18, false),
            Self::ShiftRightEqual => (BinaryOp::ShiftRight, 2, true),
            Self::ShiftRightUnsigned => (BinaryOp::ShiftRightUnsigned, 18, false),
            Self::ShiftRightUnsignedEqual => (BinaryOp::ShiftRightUnsigned, 2, true),
            Self::EqualEqual => (BinaryOp::Equal, 12, false),
            Self::NotEqual => (BinaryOp::NotEqual, 12, false),
            Self::StrictEqual => (BinaryOp::StrictEqual, 12, false),
            Self::StrictNotEqual => (BinaryOp::StrictNotEqual, 12, false),
            Self::Less => (BinaryOp::Less, 14, false),
            Self::LessEqual => (BinaryOp::LessEqual, 14, false),
            Self::Greater => (BinaryOp::Greater, 14, false),
            Self::GreaterEqual => (BinaryOp::GreaterEqual, 14, false),
            _ => return None,
        })
    }

    pub(crate) fn unary(self) -> Option<UnaryOp> {
        Some(match self {
            Self::Sharp => UnaryOp::CharacterCode,
            Self::Dollar => UnaryOp::CharacterFrom,
            Self::IsValid => UnaryOp::IsValid,
            Self::Invalidate => UnaryOp::Invalidate,
            Self::Plus => UnaryOp::Number,
            Self::Int => UnaryOp::Integer,
            Self::RealType => UnaryOp::Real,
            Self::StringType => UnaryOp::String,
            Self::BitNot => UnaryOp::BitNot,
            _ => return None,
        })
    }
}

impl Lexer<'_> {
    pub(super) fn operator(&mut self, start: usize) -> Result<TokenKind, Diagnostic> {
        // Longest token first. Slice patterns compile to character dispatch.
        let (kind, width) = match &self.units[self.cursor..] {
            [38, 38, 61, ..] => (TokenKind::LogicalAndEqual, 3),
            [124, 124, 61, ..] => (TokenKind::LogicalOrEqual, 3),
            [60, 45, 62, ..] => (TokenKind::Swap, 3),
            [62, 62, 62, 61, ..] => (TokenKind::ShiftRightUnsignedEqual, 4),
            [46, 46, 46, ..] => (TokenKind::Ellipsis, 3),
            [61, 61, 61, ..] => (TokenKind::StrictEqual, 3),
            [33, 61, 61, ..] => (TokenKind::StrictNotEqual, 3),
            [60, 60, 61, ..] => (TokenKind::ShiftLeftEqual, 3),
            [62, 62, 61, ..] => (TokenKind::ShiftRightEqual, 3),
            [62, 62, 62, ..] => (TokenKind::ShiftRightUnsigned, 3),
            [61, 62, ..] => (TokenKind::Comma, 2),
            [43, 43, ..] => (TokenKind::Increment, 2),
            [45, 45, ..] => (TokenKind::Decrement, 2),
            [38, 38, ..] => (TokenKind::LogicalAnd, 2),
            [124, 124, ..] => (TokenKind::LogicalOr, 2),
            [61, 61, ..] => (TokenKind::EqualEqual, 2),
            [33, 61, ..] => (TokenKind::NotEqual, 2),
            [60, 61, ..] => (TokenKind::LessEqual, 2),
            [62, 61, ..] => (TokenKind::GreaterEqual, 2),
            [43, 61, ..] => (TokenKind::PlusEqual, 2),
            [45, 61, ..] => (TokenKind::MinusEqual, 2),
            [42, 61, ..] => (TokenKind::StarEqual, 2),
            [47, 61, ..] => (TokenKind::SlashEqual, 2),
            [92, 61, ..] => (TokenKind::BackslashEqual, 2),
            [37, 61, ..] => (TokenKind::PercentEqual, 2),
            [38, 61, ..] => (TokenKind::BitAndEqual, 2),
            [124, 61, ..] => (TokenKind::BitOrEqual, 2),
            [94, 61, ..] => (TokenKind::BitXorEqual, 2),
            [60, 60, ..] => (TokenKind::ShiftLeft, 2),
            [62, 62, ..] => (TokenKind::ShiftRight, 2),
            [61, ..] => (TokenKind::Equal, 1),
            [60, ..] => (TokenKind::Less, 1),
            [62, ..] => (TokenKind::Greater, 1),
            [126, ..] => (TokenKind::BitNot, 1),
            [35, ..] => (TokenKind::Sharp, 1),
            [36, ..] => (TokenKind::Dollar, 1),
            [33, ..] => (TokenKind::Bang, 1),
            [63, ..] => (TokenKind::Question, 1),
            [58, ..] => (TokenKind::Colon, 1),
            [46, ..] => (TokenKind::Dot, 1),
            [40, ..] => (TokenKind::LeftParen, 1),
            [41, ..] => (TokenKind::RightParen, 1),
            [91, ..] => (TokenKind::LeftBracket, 1),
            [93, ..] => (TokenKind::RightBracket, 1),
            [123, ..] => (TokenKind::LeftBrace, 1),
            [125, ..] => (TokenKind::RightBrace, 1),
            [59, ..] => (TokenKind::Semicolon, 1),
            [44, ..] => (TokenKind::Comma, 1),
            [43, ..] => (TokenKind::Plus, 1),
            [45, ..] => (TokenKind::Minus, 1),
            [42, ..] => (TokenKind::Star, 1),
            [47, ..] => (TokenKind::Slash, 1),
            [92, ..] => (TokenKind::Backslash, 1),
            [37, ..] => (TokenKind::Percent, 1),
            [38, ..] => (TokenKind::BitAnd, 1),
            [124, ..] => (TokenKind::BitOr, 1),
            [94, ..] => (TokenKind::BitXor, 1),
            [unit, ..] => {
                let unit = *unit;
                self.cursor += 1;
                return Err(self.error(start, format!("unsupported source unit U+{unit:04X}")));
            }
            [] => unreachable!("lexer calls operator at a source unit"),
        };
        self.cursor += width;
        Ok(kind)
    }
}

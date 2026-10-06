//! KAG scenario data, lexical cursor, macro expansion and control stacks.
//! No VM, managed values, filesystem or renderer belongs to this crate.
mod control;
mod cursor;
mod scenario;
mod tag;
pub use control::Condition;
pub use cursor::{CallFrame, Parser, Position, Token};
pub use scenario::{Labels, Scenario};
pub use tag::{Argument, Attribute, Tag};
pub type Text = Vec<u16>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Debug, thiserror::Error)]
#[error("KAG line {line}, column {column}: {message}")]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}
impl Error {
    pub fn new(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self {
            line: line + 1,
            column: column + 1,
            message: message.into(),
        }
    }
}
pub fn units(text: &str) -> Text {
    text.encode_utf16().collect()
}
pub fn is(text: &[u16], ascii: &str) -> bool {
    text.iter().copied().eq(ascii.encode_utf16())
}
pub fn lower(text: &[u16]) -> Text {
    text.iter()
        .map(|&u| if (65..=90).contains(&u) { u + 32 } else { u })
        .collect()
}
pub(crate) fn ws(unit: u16) -> bool {
    matches!(unit, 9 | 32)
}

//! Backend-independent image geometry, resource accounting and composition rules.
pub mod blend;
pub mod blit;
pub mod budget;
pub mod overlay;
pub mod perspective;
pub mod resample;
pub mod scene;
pub mod transform;
pub use krkr_protocol::graphics::{DrawFace, Rect, Size};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Budget(#[from] budget::BudgetError),
    #[error("{0}")]
    Message(&'static str),
    #[error("{0}")]
    Backend(String),
}
pub type Result<T> = std::result::Result<T, Error>;
pub mod font;

pub mod transition;

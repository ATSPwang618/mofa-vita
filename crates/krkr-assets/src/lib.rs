//! Storage names, immutable read plans and Kirikiri file formats. No script
//! values or VM dependencies cross this boundary.
pub mod archive;
mod binary;
pub mod converted;
pub mod file;
pub mod local;
pub mod name;
pub mod text;
mod vfs;
mod writing;
pub mod xp3;
pub use vfs::{Lookup, ReadPlan, ReadSource, Search, StorageMedium, Stream, Vfs};
pub use writing::{WriteFile, WritePlan};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid storage name: {0}")]
    Name(&'static str),
    #[error("invalid asset data: {0}")]
    Format(&'static str),
    #[error("asset exceeds host {0} budget")]
    Limit(&'static str),
    #[error("storage not found: {0}")]
    Missing(String),
    #[error("resource changed after its read plan was created")]
    Changed,
}

/// Per-operation and retained-index admission, configurable by the host.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_read_bytes: usize,
    pub max_index_bytes: usize,
    pub max_entries: usize,
    pub max_cached_archives: usize,
    pub max_cached_index_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_read_bytes: 256 * 1024 * 1024,
            max_index_bytes: 32 * 1024 * 1024,
            max_entries: 100_000,
            max_cached_archives: 16,
            max_cached_index_bytes: 64 * 1024 * 1024,
        }
    }
}

//! Portable archive extensions. Indexes and stream factories contain no VM values.
use crate::{Limits, ReadSource, Result, xp3::Version};
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub struct ArchiveEntry {
    pub bytes: u64,
    pub source: Arc<dyn ReadSource>,
}
pub struct ArchiveIndex {
    pub version: Version,
    pub entries: BTreeMap<Vec<u16>, ArchiveEntry>,
    pub index_bytes: usize,
}
pub trait ArchiveFormat: Send + Sync {
    /// None means the signature belongs to another format, not a missing entry.
    fn open(&self, path: &Path, limits: Limits) -> Result<Option<ArchiveIndex>>;
}

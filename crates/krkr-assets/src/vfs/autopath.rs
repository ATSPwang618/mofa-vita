//! Bounded name index for ordinary auto paths, following StorageIntf's table.
//! The explicit path is always checked live; dynamic media keep ordered lookup.
use super::*;
use std::hash::{DefaultHasher, Hash, Hasher};
#[cfg(test)]
#[path = "../../tests/autopath/internal.rs"]
mod tests;

// This table only filters candidates: direct_plan still verifies the complete
// name. A hash collision adds a probe, never a false match or a missed file.
// Keeping UTF-16 names here duplicated the archive index and pushed large
// voice/portrait directories out of the Vita's bounded search table.
struct Entry {
    hash: u64,
    path: u32,
}
fn name_hash(name: &[u16]) -> u64 {
    let mut hash = DefaultHasher::new();
    name.hash(&mut hash);
    hash.finish()
}
pub(super) struct Index {
    entries: Vec<Entry>,
    fallback: Box<[bool]>,
    has_fallback: bool,
    pub bytes: usize,
}
impl Index {
    fn footprint(entries: usize, paths: usize) -> usize {
        (size_of::<Self>() + 2 * 16)
            .saturating_add(entries.saturating_mul(size_of::<Entry>()))
            .saturating_add(paths)
    }
    fn new(paths: usize) -> Self {
        Self {
            entries: Vec::new(),
            fallback: vec![true; paths].into_boxed_slice(),
            has_fallback: paths != 0,
            bytes: Self::footprint(0, paths),
        }
    }
    fn insert(&mut self, name: &[u16], path: usize, limit: usize) -> bool {
        for suffix in [
            crate::converted::VIDEO_MARKER_SUFFIX,
            crate::converted::LINK_SUFFIX,
        ] {
            if name.len() >= suffix.len() {
                let (original, tail) = name.split_at(name.len() - suffix.len());
                if tail.iter().copied().eq(suffix.bytes().map(u16::from))
                    && !self.insert_name(original, path, limit)
                {
                    return false;
                }
            }
        }
        self.insert_name(name, path, limit)
    }
    fn insert_name(&mut self, name: &[u16], path: usize, limit: usize) -> bool {
        let entries = self.entries.len().saturating_add(1);
        if path > u32::MAX as usize || Self::footprint(entries, self.fallback.len()) > limit {
            return false;
        }
        let mut entry_cap = self.entries.capacity();
        if entries > entry_cap {
            let maximum =
                limit.saturating_sub(Self::footprint(0, self.fallback.len())) / size_of::<Entry>();
            entry_cap = entries.max(entry_cap.saturating_mul(2)).min(maximum);
        }
        // Reserve geometrically while there is room, then only what fits. The
        // capacity, rather than just the used length, counts against the budget.
        if Self::footprint(entry_cap, self.fallback.len()) > limit {
            return false;
        }
        if self
            .entries
            .try_reserve_exact(entry_cap - self.entries.len())
            .is_err()
        {
            return false;
        }
        self.entries.push(Entry {
            hash: name_hash(name),
            path: path as u32,
        });
        self.bytes = Self::footprint(self.entries.capacity(), self.fallback.len());
        true
    }
    // Entries are appended one directory at a time. Roll back an oversized path
    // so later, smaller UI directories can still be indexed within the cap.
    fn rollback(&mut self, path: usize) {
        while self
            .entries
            .last()
            .is_some_and(|entry| entry.path as usize == path)
        {
            self.entries.pop();
        }
        self.entries.shrink_to_fit();
        self.bytes = Self::footprint(self.entries.capacity(), self.fallback.len());
    }
    fn finish(&mut self) {
        self.entries
            .sort_unstable_by_key(|entry| (entry.hash, entry.path));
        self.entries.dedup_by_key(|entry| (entry.hash, entry.path));
        self.has_fallback = self.fallback.iter().any(|&fallback| fallback);
    }
    pub fn candidates(&self, name: &[u16]) -> impl Iterator<Item = usize> + '_ {
        let hash = name_hash(name);
        let start = self.entries.partition_point(|entry| entry.hash < hash);
        let len = self.entries[start..].partition_point(|entry| entry.hash == hash);
        let mut matches = self.entries[start..start + len].iter().rev().peekable();
        let mut paths = (0..self.fallback.len()).rev();
        std::iter::from_fn(move || {
            if !self.has_fallback {
                return matches.next().map(|entry| entry.path as usize);
            }
            // Walk both ordered streams once when some directories could not
            // be indexed. Complete indexes visit only actual matching paths.
            for index in paths.by_ref() {
                let matched = matches
                    .peek()
                    .is_some_and(|entry| entry.path as usize == index);
                if matched {
                    matches.next();
                }
                if matched || self.fallback[index] {
                    return Some(index);
                }
            }
            None
        })
    }
    fn archive<'a>(
        &mut self,
        names: impl Iterator<Item = &'a [u16]>,
        prefix: &[u16],
        at: usize,
        limit: usize,
    ) -> bool {
        for name in names {
            let Some(leaf) = name.strip_prefix(prefix) else {
                break;
            };
            if !leaf.is_empty() && !leaf.contains(&47) && !self.insert(leaf, at, limit) {
                return false;
            }
        }
        true
    }
}

impl Vfs {
    pub(super) fn clear_auto_paths(&mut self) {
        self.auto_paths = None;
        self.auto_path_bytes = 0;
    }
    pub(super) fn ensure_auto_paths(&mut self) {
        if self.auto_paths.is_some() || self.paths.is_empty() {
            return;
        }
        // This is part of the existing retained-index budget, not an extra
        // cache allowance. Archives keep at least seven eighths available.
        let limit = self.limits.max_cached_index_bytes / 8;
        if Index::footprint(0, self.paths.len()) > limit {
            return;
        }
        self.auto_path_bytes = limit;
        while self.cached_bytes > self.limits.max_cached_index_bytes - limit {
            self.evict_archive();
        }
        let mut index = Index::new(self.paths.len());
        let mut archive: Option<(Vec<u16>, CachedArchive)> = None;
        for at in 0..self.paths.len() {
            let path = self.paths[at].clone();
            let complete = self.index_path(&path, at, &mut index, limit, &mut archive);
            if matches!(complete, Ok(true)) {
                index.fallback[at] = false;
            } else {
                // Preserve errors at the actual candidate, rather than raising
                // errors from lower-priority paths during speculative indexing.
                index.rollback(at);
            }
        }
        index.finish();
        self.auto_path_bytes = index.bytes;
        self.auto_paths = Some(index);
    }
    fn index_path(
        &mut self,
        path: &[u16],
        at: usize,
        index: &mut Index,
        limit: usize,
        cached: &mut Option<(Vec<u16>, CachedArchive)>,
    ) -> Result<bool> {
        if self.medium(path).is_some() {
            return Ok(false);
        }
        let (outer, inner) = name::split_archive(path);
        if let Some(inner) = inner {
            if cached.as_ref().is_none_or(|(key, _)| key != outer) {
                let archive = if let Some(archive) = self.foreign_archive(outer)? {
                    CachedArchive::Foreign(archive)
                } else {
                    CachedArchive::Xp3(self.archive(outer)?)
                };
                *cached = Some((outer.to_vec(), archive));
            }
            let archive = &cached.as_ref().unwrap().1;
            return Ok(match archive {
                CachedArchive::Xp3(archive) => {
                    index.archive(archive.entries.names_from(inner), inner, at, limit)
                }
                CachedArchive::Foreign(archive) => index.archive(
                    archive
                        .entries
                        .range::<[u16], _>((
                            std::ops::Bound::Included(inner),
                            std::ops::Bound::Unbounded,
                        ))
                        .map(|(name, _)| name.as_slice()),
                    inner,
                    at,
                    limit,
                ),
            });
        } else {
            let directory = local::resolve(&local::from_storage(path)?)?;
            let entries = match local::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
                Err(error) => return Err(error.into()),
            };
            for entry in entries {
                let entry = entry?;
                let kind = entry.file_type()?;
                if kind.is_file()
                    || (kind.is_symlink() && std::fs::metadata(entry.path())?.is_file())
                {
                    let leaf = name::fold(&local::units(Path::new(&entry.file_name()))?);
                    if !index.insert(&leaf, at, limit) {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }
}

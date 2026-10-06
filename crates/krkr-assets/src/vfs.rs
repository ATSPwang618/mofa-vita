use crate::{
    Error, Limits, Result, local, name,
    xp3::{self, Archive, Entry, FilterFactory, Version},
};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::OpenOptions,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Weak},
};
mod autopath;

pub trait Stream: Read + Seek + Send {}
impl<T: Read + Seek + Send> Stream for T {}
/// A portable read source. Every open owns its seek cursor and decoder state.
pub trait ReadSource: Send + Sync {
    fn open(&self) -> Result<Box<dyn Stream>>;
    fn open_interruptible(&self, cancelled: &dyn Fn() -> bool) -> Result<Box<dyn Stream>> {
        if cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "storage open cancelled",
            )
            .into());
        }
        self.open()
    }
}
/// Named, read-only storage media supplied by the host or a plugin. Media must
/// release their own locks before resolving backing files through this VFS.
pub trait StorageMedium: Send + Sync {
    /// Called after syntax normalization. Individual media own domain/path case rules.
    fn normalize(&self, path: &[u16]) -> Vec<u16> {
        name::fold(path)
    }
    fn plan(&self, vfs: &mut Vfs, normalized: &[u16]) -> Result<Option<ReadPlan>>;
    fn list(&self, vfs: &mut Vfs, normalized: &[u16]) -> Result<Vec<Vec<u16>>>;
    fn clear_cache(&self) {}
}
enum Target {
    Custom(Arc<dyn ReadSource>),
    Linked(Box<ReadPlan>),
    File {
        path: std::path::PathBuf,
        version: Version,
    },
    Archive {
        archive: Arc<Archive>,
        entry: Arc<Entry>,
        filter: Option<Arc<dyn FilterFactory>>,
    },
}
/// Owned metadata, never a shared seek cursor. Opening twice creates independent
/// streams, including independent extraction-filter state.
pub struct ReadPlan {
    pub name: Vec<u16>,
    pub bytes: u64,
    target: Target,
    limit: usize,
}
/// A name search may already have resolved the explicit local/archive candidate.
/// Reuse that plan instead of probing it again. Dynamic media always return
/// ordered candidates so their callbacks still run only during resolution.
pub enum Search {
    Found {
        /// Requested name, which can differ from a converted video's read plan.
        candidate: Vec<u16>,
        plan: ReadPlan,
    },
    Candidates(Vec<Vec<u16>>),
}
impl ReadPlan {
    /// Whether two resolved plans identify the same ordinary file version.
    /// Resolve a fresh plan before reusing decoded data. Custom media and
    /// extraction filters can change their output without changing a file.
    pub fn same_file_version(&self, other: &Self) -> bool {
        match (&self.target, &other.target) {
            (Target::Linked(a), _) => a.same_file_version(other),
            (_, Target::Linked(b)) => self.same_file_version(b),
            (
                Target::File {
                    path: a,
                    version: av,
                },
                Target::File {
                    path: b,
                    version: bv,
                },
            ) => a == b && av.modified.is_some() && av == bv,
            (
                Target::Archive {
                    archive: a,
                    entry: ae,
                    filter: None,
                },
                Target::Archive {
                    archive: b,
                    entry: be,
                    filter: None,
                },
            ) => Arc::ptr_eq(a, b) && a.version.modified.is_some() && ae.name == be.name,
            _ => false,
        }
    }
    fn alias(self, name: Vec<u16>) -> Self {
        Self {
            name,
            bytes: self.bytes,
            limit: self.limit,
            target: Target::Linked(Box::new(self)),
        }
    }
    /// The resolved storage identity, for host metadata and decoder hints.
    /// `name` retains the script identity used by masks, loops and scale metadata.
    pub fn physical_name(&self) -> &[u16] {
        match &self.target {
            Target::Linked(plan) => plan.physical_name(),
            _ => &self.name,
        }
    }
    pub fn open_interruptible(&self, cancelled: &dyn Fn() -> bool) -> Result<Box<dyn Stream>> {
        match &self.target {
            Target::Custom(source) => source.open_interruptible(cancelled),
            Target::Linked(plan) => plan.open_interruptible(cancelled),
            _ => self.open(),
        }
    }
    pub fn custom(name: Vec<u16>, bytes: u64, limit: usize, source: Arc<dyn ReadSource>) -> Self {
        Self {
            name,
            bytes,
            target: Target::Custom(source),
            limit,
        }
    }
    pub fn open(&self) -> Result<Box<dyn Stream>> {
        match &self.target {
            Target::Custom(source) => source.open(),
            Target::Linked(plan) => plan.open(),
            Target::File { path, version } => {
                let file = crate::file::open(path)?;
                version.check(&file)?;
                Ok(Box::new(file))
            }
            Target::Archive {
                archive,
                entry,
                filter,
            } => {
                let filter = filter
                    .as_ref()
                    .map(|f| f.create(&self.name, entry))
                    .transpose()?;
                let full = filter
                    .as_ref()
                    .is_some_and(|filter| filter.fetch_full_data());
                let mut reader = xp3::Reader::new(archive.clone(), entry.clone(), filter);
                if full {
                    let len = crate::binary::size(self.bytes, self.limit, "filtered stream bytes")?;
                    let mut bytes = vec![0; len];
                    reader.read_exact(&mut bytes)?;
                    Ok(Box::new(std::io::Cursor::new(bytes)))
                } else {
                    Ok(Box::new(reader))
                }
            }
        }
    }
    pub fn read(&self, offset: u64) -> Result<Vec<u8>> {
        self.read_interruptible(offset, || false)
    }
    /// Cancellation is checked between chunks. An OS read or compressed seek
    /// already in progress cannot be interrupted by this portable interface.
    pub fn read_interruptible(
        &self,
        offset: u64,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<Vec<u8>> {
        let len = crate::binary::size(self.bytes.saturating_sub(offset), self.limit, "read bytes")?;
        let check = |cancelled: &mut dyn FnMut() -> bool| -> Result<()> {
            if cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "storage read cancelled",
                )
                .into());
            }
            Ok(())
        };
        check(&mut cancelled)?;
        let cancel = std::cell::RefCell::new(&mut cancelled);
        let mut stream = self.open_interruptible(&|| (*cancel.borrow_mut())())?;
        stream.seek(SeekFrom::Start(offset))?;
        let mut data = vec![0; len];
        for chunk in data.chunks_mut(64 * 1024) {
            check(&mut cancelled)?;
            stream.read_exact(chunk)?;
        }
        Ok(data)
    }
}
/// One synchronous name-search batch. Reuse the most recently verified archive
/// for sibling candidates, then discard before yielding or invoking script.
/// Only one index and bounded local directory snapshots are retained. Local
/// misses are shared only within this batch; discard it before any mutation.
#[derive(Default)]
pub struct Lookup {
    archive: Option<(Vec<u16>, CachedArchive)>,
    verified: Vec<(Vec<u16>, VerifiedArchive)>,
    local: local::Lookup,
}
enum VerifiedArchive {
    Xp3(Weak<Archive>),
    Foreign(Weak<crate::archive::ArchiveIndex>),
}
impl VerifiedArchive {
    fn upgrade(&self) -> Option<CachedArchive> {
        match self {
            Self::Xp3(archive) => archive.upgrade().map(CachedArchive::Xp3),
            Self::Foreign(archive) => archive.upgrade().map(CachedArchive::Foreign),
        }
    }
}
#[derive(Clone)]
enum CachedArchive {
    Xp3(Arc<Archive>),
    Foreign(Arc<crate::archive::ArchiveIndex>),
}
pub struct Vfs {
    current: Vec<u16>,
    file_current: Vec<u16>,
    paths: Vec<Vec<u16>>,
    auto_paths: Option<autopath::Index>,
    auto_path_bytes: usize,
    archives: VecDeque<(Vec<u16>, Arc<Archive>)>,
    formats: Vec<Arc<dyn crate::archive::ArchiveFormat>>,
    foreign_archives: VecDeque<(Vec<u16>, Arc<crate::archive::ArchiveIndex>)>,
    cached_bytes: usize,
    filter: Option<Arc<dyn FilterFactory>>,
    limits: Limits,
    media: BTreeMap<Vec<u16>, Arc<dyn StorageMedium>>,
}
impl Vfs {
    /// Select an unpacked project or the conventional data directory/archive.
    /// An explicit archive entry selects that archive as the project directory.
    /// Resource auto paths remain under the startup script's control (KAG, etc.).
    pub fn for_project(directory: &Path, entry: &[u16], limits: Limits) -> Result<Self> {
        let mut vfs = Self::new(directory, limits)?;
        let normalized = vfs.full_path(entry)?;
        if name::split_archive(&normalized).1.is_some() {
            let outer = name::split_archive(&normalized).0;
            vfs.set_directory(&[outer, &[62]].concat())?;
        } else if vfs.direct(&normalized)?.is_none() {
            if local::resolve(&directory.join("data"))?.is_dir() {
                vfs.set_directory(&local::directory(&directory.join("data"))?)?;
            } else if local::resolve(&directory.join("data.xp3"))?.is_file() {
                vfs.set_directory(&name::units("data.xp3>"))?;
            }
        }
        Ok(vfs)
    }
    pub fn new(directory: &Path, limits: Limits) -> Result<Self> {
        Ok(Self {
            current: local::directory(directory)?,
            file_current: local::directory(directory)?,
            paths: Vec::new(),
            auto_paths: None,
            auto_path_bytes: 0,
            archives: VecDeque::new(),
            formats: Vec::new(),
            foreign_archives: VecDeque::new(),
            cached_bytes: 0,
            filter: None,
            limits,
            media: BTreeMap::new(),
        })
    }
    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn current_directory(&self) -> &[u16] {
        &self.current
    }
    /// Last file-backed cwd, retained while a virtual medium is current.
    pub fn file_directory(&self) -> &[u16] {
        &self.file_current
    }
    pub fn full_path(&self, path: &[u16]) -> Result<Vec<u16>> {
        if let Some(path) = name::medium_path(path, &self.current)? {
            let medium = self
                .medium(&path)
                .ok_or(Error::Name("storage medium is not registered"))?;
            return Ok(medium.normalize(&path));
        }
        name::normalize(path, &self.file_current)
    }
    pub fn register_medium(&mut self, scheme: &str, medium: Arc<dyn StorageMedium>) -> Result<()> {
        if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_lowercase()) || scheme == "file"
        {
            return Err(Error::Name("invalid storage medium name"));
        }
        let key = name::units(scheme);
        if self.media.contains_key(&key) {
            return Err(Error::Name("storage medium already registered"));
        }
        self.media.insert(key, medium);
        self.clear_auto_paths();
        Ok(())
    }
    pub fn unregister_medium(&mut self, scheme: &str, owner: &Arc<dyn StorageMedium>) {
        let key = name::units(scheme);
        if self.media.get(&key).is_some_and(|m| Arc::ptr_eq(m, owner)) {
            self.media.remove(&key);
            self.clear_auto_paths();
        }
    }
    fn medium(&self, path: &[u16]) -> Option<Arc<dyn StorageMedium>> {
        let at = path.iter().position(|&u| u == 58)?;
        self.media.get(&path[..at]).cloned()
    }
    /// None selects the existing local/archive directory implementation.
    pub fn list_medium(&mut self, path: &[u16]) -> Result<Option<Vec<Vec<u16>>>> {
        name::directory(path)?;
        let path = self.full_path(path)?;
        self.medium(&path).map(|m| m.list(self, &path)).transpose()
    }
    /// Present logical resource names when a directory contains generated links.
    /// Physical targets remain directly addressable, but are not duplicate list entries.
    pub fn visible_names(
        &mut self,
        directory: &[u16],
        names: Vec<Vec<u16>>,
    ) -> Result<Vec<Vec<u16>>> {
        let mut hidden = std::collections::BTreeSet::new();
        let mut aliases = Vec::new();
        let mut lookup = Lookup::default();
        let suffix = name::units(crate::converted::LINK_SUFFIX);
        for leaf in &names {
            if let Some(original) = leaf.strip_suffix(suffix.as_slice()) {
                let marker_name = [directory, leaf.as_slice()].concat();
                let marker = self
                    .direct_unmapped(&marker_name, &mut lookup)?
                    .ok_or_else(|| Error::Missing(String::from_utf16_lossy(&marker_name)))?;
                if marker.bytes > crate::converted::MAX_LINK_BYTES as u64 {
                    return Err(Error::Limit("resource link"));
                }
                let bytes = marker.read(0)?;
                let target = crate::converted::decode_link(&bytes)?;
                hidden.insert(name::fold(leaf));
                hidden.insert(name::fold(&name::units(target)));
                aliases.push(original.to_vec());
            }
        }
        let mut result: Vec<_> = names
            .into_iter()
            .filter(|n| !hidden.contains(&name::fold(n)))
            .collect();
        let mut seen: std::collections::BTreeSet<_> =
            result.iter().map(|n| name::fold(n)).collect();
        for alias in aliases {
            if seen.insert(name::fold(&alias)) {
                result.push(alias);
            }
        }
        Ok(result)
    }
    pub fn set_directory(&mut self, path: &[u16]) -> Result<()> {
        name::directory(path)?;
        self.current = self.full_path(path)?;
        self.clear_auto_paths();
        if self.current.starts_with(&name::units("file://")) {
            self.file_current.clone_from(&self.current);
        }
        Ok(())
    }
    pub fn add_path(&mut self, path: &[u16]) -> Result<()> {
        name::directory(path)?;
        let path = self.full_path(path)?;
        if !self.paths.contains(&path) {
            self.paths.push(path);
        }
        self.clear_auto_paths();
        Ok(())
    }
    pub fn remove_path(&mut self, path: &[u16]) -> Result<()> {
        name::directory(path)?;
        let path = self.full_path(path)?;
        self.paths.retain(|p| p != &path);
        self.clear_auto_paths();
        Ok(())
    }
    pub fn set_filter(&mut self, filter: Option<Arc<dyn FilterFactory>>) {
        self.filter = filter;
    }
    pub fn filter(&self) -> Option<Arc<dyn FilterFactory>> {
        self.filter.clone()
    }
    pub fn clear_archive_cache(&mut self) {
        self.clear_auto_paths();
        self.archives.clear();
        self.foreign_archives.clear();
        self.cached_bytes = 0;
        for medium in self.media.values() {
            medium.clear_cache();
        }
    }
    /// Publish a local write without discarding unrelated archive indexes.
    /// The path must be the resolved destination captured before starting IO.
    pub fn invalidate_file(&mut self, path: &Path) {
        let normalized =
            local::units(path).and_then(|raw| name::normalize(&raw, &self.file_current));
        let Ok(normalized) = normalized else {
            self.clear_archive_cache();
            return;
        };
        // A new loose file may shadow an indexed auto-path entry. Archive
        // metadata, however, belongs to the archive file, not to a save file.
        let parent = normalized
            .iter()
            .rposition(|&c| c == 47)
            .map(|at| &normalized[..=at]);
        if self.paths.iter().any(|path| {
            let (outer, inner) = name::split_archive(path);
            if inner.is_some() {
                outer == normalized
            } else {
                Some(path.as_slice()) == parent
            }
        }) {
            self.clear_auto_paths();
        }
        self.archives.retain(|(key, archive)| {
            if key == &normalized {
                self.cached_bytes -= archive.index_bytes;
                false
            } else {
                true
            }
        });
        self.foreign_archives.retain(|(key, archive)| {
            if key == &normalized {
                self.cached_bytes -= archive.index_bytes;
                false
            } else {
                true
            }
        });
        // Extension media may refer to local files through their own names.
        for medium in self.media.values() {
            medium.clear_cache();
        }
    }
    fn archive(&mut self, name: &[u16]) -> Result<Arc<Archive>> {
        #[cfg(target_os = "vita")]
        let timer = krkr_protocol::diagnostics::Timer::start();
        let (path, metadata) = local::metadata(&local::from_storage(name)?)?;
        if let Some(at) = self.archives.iter().position(|(key, _)| key == name) {
            let (key, archive) = self.archives.remove(at).unwrap();
            if archive.version == crate::file::version(&path, &metadata)? {
                self.archives.push_back((key, archive.clone()));
                #[cfg(target_os = "vita")]
                timer.report(|| {
                    format!(
                        "stage=archive-validation name={}",
                        String::from_utf16_lossy(name)
                    )
                });
                return Ok(archive);
            }
            if self.auto_paths.is_some() {
                self.clear_auto_paths();
            }
            self.cached_bytes -= archive.index_bytes;
        }
        let archive = Arc::new(Archive::load(&path, self.limits)?);
        if self.admit_archive(archive.index_bytes) {
            self.cached_bytes += archive.index_bytes;
            self.archives.push_back((name.to_vec(), archive.clone()));
        }
        Ok(archive)
    }
    pub fn register_archive_format(&mut self, format: Arc<dyn crate::archive::ArchiveFormat>) {
        self.formats.push(format);
        self.clear_archive_cache();
    }
    pub fn unregister_archive_format(&mut self, owner: &Arc<dyn crate::archive::ArchiveFormat>) {
        self.formats.retain(|format| !Arc::ptr_eq(format, owner));
        self.clear_archive_cache();
    }
    fn evict_archive(&mut self) {
        if let Some((_, old)) = self.foreign_archives.pop_front() {
            self.cached_bytes -= old.index_bytes;
        } else if let Some((_, old)) = self.archives.pop_front() {
            self.cached_bytes -= old.index_bytes;
        }
    }
    fn admit_archive(&mut self, bytes: usize) -> bool {
        let limit = self.limits.max_cached_index_bytes - self.auto_path_bytes;
        if self.limits.max_cached_archives == 0 || bytes > limit {
            return false;
        }
        while self.archives.len() + self.foreign_archives.len() >= self.limits.max_cached_archives
            || self.cached_bytes + bytes > limit
        {
            self.evict_archive();
        }
        true
    }
    fn foreign_archive(
        &mut self,
        name: &[u16],
    ) -> Result<Option<Arc<crate::archive::ArchiveIndex>>> {
        if self.formats.is_empty() {
            return Ok(None);
        }
        let (path, metadata) = local::metadata(&local::from_storage(name)?)?;
        if let Some(at) = self
            .foreign_archives
            .iter()
            .position(|(key, _)| key == name)
        {
            let (key, archive) = self.foreign_archives.remove(at).unwrap();
            if archive.version == crate::file::version(&path, &metadata)? {
                self.foreign_archives.push_back((key, archive.clone()));
                return Ok(Some(archive));
            }
            if self.auto_paths.is_some() {
                self.clear_auto_paths();
            }
            self.cached_bytes -= archive.index_bytes;
        }
        for format in self.formats.iter().rev() {
            if let Some(index) = format.open(&path, self.limits)? {
                if index.entries.len() > self.limits.max_entries
                    || index.index_bytes > self.limits.max_index_bytes
                {
                    return Err(Error::Limit("archive index"));
                }
                let index = Arc::new(index);
                if self.admit_archive(index.index_bytes) {
                    self.cached_bytes += index.index_bytes;
                    self.foreign_archives
                        .push_back((name.to_vec(), index.clone()));
                }
                return Ok(Some(index));
            }
        }
        Ok(None)
    }
    fn direct(&mut self, normalized: &[u16]) -> Result<Option<ReadPlan>> {
        self.direct_plan_in(normalized, &mut Lookup::default())
    }
    /// Resolve one candidate within a synchronous batch. The batch does not
    /// retain file contents, and is never retained across script calls.
    pub fn direct_plan_in(
        &mut self,
        normalized: &[u16],
        lookup: &mut Lookup,
    ) -> Result<Option<ReadPlan>> {
        // Ordinary files and archive entries need no link traversal state.
        if let Some(plan) = self.direct_unmapped(normalized, lookup)? {
            return Ok(Some(plan));
        }
        let mut current = normalized.to_vec();
        let mut visited = std::collections::BTreeSet::new();
        let mut linked = false;
        for depth in 0..=crate::converted::MAX_LINK_DEPTH {
            if !visited.insert(current.clone()) {
                return Err(Error::Format("cyclic resource link"));
            }
            if depth != 0
                && let Some(plan) = self.direct_unmapped(&current, lookup)?
            {
                return Ok(Some(if linked {
                    plan.alias(normalized.to_vec())
                } else {
                    plan
                }));
            }
            if self.medium(&current).is_some() {
                return Ok(None);
            }
            let marker_name = [&current[..], &name::units(crate::converted::LINK_SUFFIX)].concat();
            if let Some(marker) = self.direct_unmapped(&marker_name, lookup)? {
                if marker.bytes > crate::converted::MAX_LINK_BYTES as u64 {
                    return Err(Error::Limit("resource link"));
                }
                let bytes = marker.read(0)?;
                let target = crate::converted::decode_link(&bytes)?;
                current = crate::converted::sibling(&current, target);
                linked = true;
                continue;
            }
            if let Some(target) = self.converted_video_target(&current, lookup)? {
                current = target;
                continue;
            }
            return if visited.len() == 1 {
                Ok(None)
            } else {
                Err(Error::Missing(String::from_utf16_lossy(&current)))
            };
        }
        Err(Error::Limit("resource link depth"))
    }
    fn converted_video_target(
        &mut self,
        normalized: &[u16],
        lookup: &mut Lookup,
    ) -> Result<Option<Vec<u16>>> {
        let marker_name = [
            normalized,
            &name::units(crate::converted::VIDEO_MARKER_SUFFIX),
        ]
        .concat();
        if let Some(marker) = self.direct_unmapped(&marker_name, lookup)? {
            if marker.bytes != crate::converted::VIDEO_MARKER.len() as u64
                || marker.read(0)? != crate::converted::VIDEO_MARKER
            {
                return Err(Error::Format("invalid converted video marker"));
            }
            return Ok(Some(
                [normalized, &name::units(crate::converted::VIDEO_SUFFIX)].concat(),
            ));
        }
        Ok(None)
    }
    fn direct_unmapped(
        &mut self,
        normalized: &[u16],
        lookup: &mut Lookup,
    ) -> Result<Option<ReadPlan>> {
        if normalized.is_empty() {
            return Ok(None);
        }
        if let Some(medium) = self.medium(normalized) {
            lookup.archive = None;
            lookup.verified.clear();
            lookup.local = local::Lookup::default();
            return medium.plan(self, normalized);
        }
        let (outer, inner) = name::split_archive(normalized);
        let target;
        let bytes;
        if let Some(inner) = inner {
            let cached = lookup
                .archive
                .as_ref()
                .filter(|(name, _)| name == outer)
                .map(|(_, archive)| archive.clone())
                .or_else(|| {
                    lookup
                        .verified
                        .iter()
                        .find(|(name, _)| name == outer)
                        .and_then(|(_, archive)| archive.upgrade())
                });
            let archive = if let Some(archive) = cached {
                archive
            } else {
                let foreign = match self.foreign_archive(outer) {
                    Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(None);
                    }
                    result => result?,
                };
                let archive = if let Some(archive) = foreign {
                    CachedArchive::Foreign(archive)
                } else {
                    match self.archive(outer) {
                        Ok(archive) => CachedArchive::Xp3(archive),
                        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                            return Ok(None);
                        }
                        Err(e) => return Err(e),
                    }
                };
                lookup.archive = Some((outer.to_vec(), archive.clone()));
                // Alternative extensions revisit the same few archives. Weak
                // entries reuse validation without pinning evicted indices or
                // extending the retained-index budget.
                lookup
                    .verified
                    .retain(|(_, archive)| archive.upgrade().is_some());
                if lookup.verified.len() < 8 {
                    let weak = match &archive {
                        CachedArchive::Xp3(archive) => {
                            VerifiedArchive::Xp3(Arc::downgrade(archive))
                        }
                        CachedArchive::Foreign(archive) => {
                            VerifiedArchive::Foreign(Arc::downgrade(archive))
                        }
                    };
                    lookup.verified.push((outer.to_vec(), weak));
                }
                archive
            };
            if let CachedArchive::Foreign(archive) = &archive {
                return Ok(archive.entries.get(inner).map(|entry| {
                    ReadPlan::custom(
                        normalized.to_vec(),
                        entry.bytes,
                        self.limits.max_read_bytes,
                        entry.source.clone(),
                    )
                }));
            }
            let CachedArchive::Xp3(archive) = archive else {
                unreachable!()
            };
            let Some(entry) = archive.entries.get(inner) else {
                return Ok(None);
            };
            bytes = entry.size;
            target = Target::Archive {
                archive,
                entry,
                filter: self.filter.clone(),
            };
        } else {
            let (path, metadata) = match lookup.local.metadata(&local::from_storage(outer)?) {
                Ok(result) => result,
                Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e),
            };
            if !metadata.is_file() {
                return Ok(None);
            }
            let version = crate::file::version(&path, &metadata)?;
            bytes = version.bytes;
            target = Target::File { path, version };
        }
        Ok(Some(ReadPlan {
            name: normalized.to_vec(),
            bytes,
            target,
            limit: self.limits.max_read_bytes,
        }))
    }
    fn find(&mut self, path: &[u16]) -> Result<Option<ReadPlan>> {
        let candidates = match self.search(path)? {
            Search::Found { plan, .. } => return Ok(Some(plan)),
            Search::Candidates(candidates) => candidates,
        };
        let mut lookup = Lookup::default();
        for candidate in candidates {
            if let Some(plan) = self.direct_plan_in(&candidate, &mut lookup)? {
                return Ok(Some(plan));
            }
        }
        Ok(None)
    }
    /// Ordered candidates for hosts that resolve script-backed media on their
    /// own VM. Searching these must stop at the first existing file.
    pub fn search_candidates(&mut self, path: &[u16]) -> Result<Vec<Vec<u16>>> {
        Ok(match self.search(path)? {
            Search::Found { candidate, .. } => vec![candidate],
            Search::Candidates(candidates) => candidates,
        })
    }
    /// Preserve an already resolved ordinary candidate for callers that can use
    /// its plan immediately. Retained plans still validate files when opened.
    pub fn search(&mut self, path: &[u16]) -> Result<Search> {
        self.search_in(path, &mut Lookup::default())
    }
    /// Search siblings in one synchronous batch, without repeating missing
    /// local-file probes. Discard the lookup before invoking script or writing.
    pub fn search_in(&mut self, path: &[u16], lookup: &mut Lookup) -> Result<Search> {
        #[cfg(target_os = "vita")]
        let normalize_timer = krkr_protocol::diagnostics::Timer::start();
        let normalized = self.full_path(path)?;
        #[cfg(target_os = "vita")]
        normalize_timer.report(|| {
            format!(
                "stage=search-normalize name={}",
                String::from_utf16_lossy(path)
            )
        });
        let basename = name::split_name(&normalized).1;
        // Script-backed media can mutate files during resolution. Preserve
        // their live candidate order instead of probing overlays in advance.
        let dynamic = self.medium(&normalized).is_some()
            || self.paths.iter().any(|path| self.medium(path).is_some());

        // Relative loads inside an archive retain their full entry key when
        // consulting newer resource paths. This lets an overlay replace
        // `sub/file.tjs` without also replacing unrelated same-name files.
        // Explicit archive/local addresses continue to select their own file.
        let relative = !path.iter().any(|&u| matches!(u, 58 | 62))
            && !path.first().is_some_and(|&u| matches!(u, 47 | 92));
        if !dynamic
            && relative
            && let (_, Some(entry)) = name::split_archive(&normalized)
            && entry.contains(&47)
        {
            let paths = self.paths.clone();
            for base in paths.iter().rev() {
                if self.medium(base).is_some() {
                    continue;
                }
                let candidate = [base.as_slice(), entry].concat();
                if candidate != normalized
                    && let Some(plan) = self.direct_plan_in(&candidate, lookup)?
                {
                    return Ok(Search::Found { candidate, plan });
                }
            }
        }
        if basename.is_empty() || self.paths.is_empty() {
            return Ok(Search::Candidates(vec![normalized]));
        }
        // A script-backed medium can mutate files during resolution; such a
        // query retains the original live, ordered candidate sequence.
        if !dynamic {
            #[cfg(target_os = "vita")]
            let explicit_timer = krkr_protocol::diagnostics::Timer::start();
            let plan = self.direct_plan_in(&normalized, lookup)?;
            #[cfg(target_os = "vita")]
            explicit_timer.report(|| {
                format!(
                    "stage=search-explicit name={}",
                    String::from_utf16_lossy(&normalized)
                )
            });
            if let Some(plan) = plan {
                return Ok(Search::Found {
                    candidate: normalized,
                    plan,
                });
            }
        }
        // KAG also installs archive subdirectories as auto paths while keeping
        // a local cwd. Preserve that archive-entry namespace for overlays too.
        if !dynamic && relative {
            let paths = self.paths.clone();
            for source in paths.iter().rev() {
                let (_, Some(directory)) = name::split_archive(source) else {
                    continue;
                };
                if directory.is_empty() {
                    continue;
                }
                let entry = [directory, basename].concat();
                let original = [source.as_slice(), basename].concat();
                // Only the first existing archive candidate establishes the
                // namespace. An absent later auto path must not redirect the
                // lookup to a different archive's same-name script.
                if self.direct_plan_in(&original, lookup)?.is_none() {
                    continue;
                }
                for base in paths.iter().rev() {
                    if self.medium(base).is_some() {
                        continue;
                    }
                    let (outer, inner) = name::split_archive(base);
                    if inner.is_some_and(|directory| !directory.is_empty())
                        || (inner.is_some() && outer == name::split_archive(source).0)
                    {
                        continue;
                    }
                    let candidate = [base.as_slice(), entry.as_slice()].concat();
                    if candidate != original
                        && let Some(plan) = self.direct_plan_in(&candidate, lookup)?
                    {
                        return Ok(Search::Found { candidate, plan });
                    }
                }
                break;
            }
        }
        // The ordinary explicit candidate has already missed above. Probing it
        // again repeats archive validation and resource-link lookups for every
        // absent image in games that enumerate their sprite variants at startup.
        let mut candidates = if dynamic {
            vec![normalized.clone()]
        } else {
            Vec::new()
        };
        if !dynamic {
            #[cfg(target_os = "vita")]
            let index_timer = krkr_protocol::diagnostics::Timer::start();
            self.ensure_auto_paths();
            #[cfg(target_os = "vita")]
            index_timer.report(|| {
                format!(
                    "stage=search-index paths={} bytes={}",
                    self.paths.len(),
                    self.auto_path_bytes
                )
            });
            if let Some(index) = &self.auto_paths {
                #[cfg(target_os = "vita")]
                let candidates_timer = krkr_protocol::diagnostics::Timer::start();
                for at in index.candidates(basename) {
                    let candidate = [self.paths[at].as_slice(), basename].concat();
                    if candidate != normalized {
                        candidates.push(candidate);
                    }
                }
                #[cfg(target_os = "vita")]
                candidates_timer.report(|| {
                    format!(
                        "stage=search-candidates name={} count={}",
                        String::from_utf16_lossy(basename),
                        candidates.len()
                    )
                });
                return Ok(Search::Candidates(candidates));
            }
        }
        if !basename.is_empty() {
            for base in self.paths.iter().rev() {
                let candidate = [base.as_slice(), basename].concat();
                // add_path already deduplicates normalized directories. Only
                // the explicit candidate can also occur in that unique list.
                if candidate != normalized {
                    candidates.push(candidate);
                }
            }
        }
        Ok(Search::Candidates(candidates))
    }
    /// Open exactly one normalized candidate, without starting another search.
    pub fn direct_plan(&mut self, normalized: &[u16]) -> Result<Option<ReadPlan>> {
        self.direct(normalized)
    }
    pub fn placed_path(&mut self, path: &[u16]) -> Result<Vec<u16>> {
        Ok(self.find(path)?.map_or_else(Vec::new, |p| p.name))
    }
    /// fstat's exact existence query bypasses cwd, auto paths and local case
    /// resolution. The original still folds and deduplicates archive separators,
    /// but does not interpret dot components within that archive entry name.
    pub fn exists_no_search_no_normalize(&mut self, path: &[u16]) -> Result<bool> {
        let path = name::c_string(path);
        if path.is_empty() {
            return Ok(false);
        }
        if let Some(medium) = self.medium(path) {
            return Ok(medium.plan(self, path)?.is_some());
        }
        let (outer, inner) = name::split_archive(path);
        if let Some(inner) = inner {
            let folded = name::fold(inner);
            let mut entry = Vec::new();
            for part in folded.split(|&u| u == 47).filter(|part| !part.is_empty()) {
                if !entry.is_empty() {
                    entry.push(47);
                }
                entry.extend_from_slice(part);
            }
            if folded.last() == Some(&47) && !entry.is_empty() {
                entry.push(47);
            }
            let exact = [outer, &[62], &entry].concat();
            return Ok(self.direct(&exact)?.is_some());
        }
        if std::fs::metadata(local::from_storage(outer)?).is_ok_and(|m| m.is_file()) {
            return Ok(true);
        }
        // Keep the exact local-name semantics unless an explicit link exists.
        let marker = [outer, &name::units(crate::converted::LINK_SUFFIX)].concat();
        if local::metadata(&local::from_storage(&marker)?).is_ok_and(|(_, m)| m.is_file()) {
            return Ok(self.direct(path)?.is_some());
        }
        Ok(false)
    }
    pub fn plan(&mut self, path: &[u16]) -> Result<ReadPlan> {
        self.find(path)?
            .ok_or_else(|| Error::Missing(String::from_utf16_lossy(path)))
    }
    pub fn local_name(&self, path: &[u16]) -> Result<Vec<u16>> {
        let path = self.full_path(path)?;
        if self.medium(&path).is_some() {
            return Ok(Vec::new());
        }
        local::units(&local::from_storage(&path)?)
    }
    pub fn write(&mut self, path: &[u16], offset: Option<u64>, bytes: &[u8]) -> Result<()> {
        let normalized = if offset.is_some() {
            self.plan(path)?.name
        } else {
            self.full_path(path)?
        };
        let path = local::resolve(&local::from_storage(&normalized)?)?;
        crate::binary::size(
            bytes.len() as u64,
            self.limits.max_read_bytes,
            "write bytes",
        )?;
        #[cfg(target_os = "vita")]
        if offset.is_none() {
            // Publish complete replacements through the same native staged
            // writer as image saves, preserving the old manifest on failure.
            // Offset writes below must retain their in-place semantics.
            let result = (|| {
                let mut file = crate::WritePlan {
                    path: path.clone(),
                    limit: self.limits.max_read_bytes as u64,
                }
                .create()?;
                file.write_all(bytes)?;
                file.finish()
            })();
            self.invalidate_file(&path);
            return result;
        }
        let mut file = OpenOptions::new()
            .create(offset.is_none())
            .write(true)
            .truncate(offset.is_none())
            .open(&path)?;
        if let Some(offset) = offset {
            file.seek(SeekFrom::Start(offset))?;
        }
        let result = file.write_all(bytes);
        // Existing plans retain their metadata; new resolutions observe the new version.
        self.invalidate_file(&path);
        result?;
        Ok(())
    }
    /// Resolve on the VFS thread; encoding and file IO use only owned metadata.
    pub fn write_plan(&self, path: &[u16]) -> Result<crate::WritePlan> {
        let path = local::resolve(&local::from_storage(&self.full_path(path)?)?)?;
        Ok(crate::WritePlan {
            path,
            limit: self.limits.max_read_bytes as u64,
        })
    }
}

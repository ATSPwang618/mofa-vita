//! `.hed` / `.nam` / `.mrg` archive triples (e.g. `allpac`, `allui`).
//!
//! `.hed`: 8-byte entries `u32 offset_sectors, u16 stored_sectors, u16 unpacked_sectors`,
//! terminated by an all-`0xFF` entry. `.nam`: 32-byte records, NUL-padded names ending
//! in `\r\n`. `.mrg`: raw data, 0x800-byte sectors. Archives without a `.nam`
//! (e.g. `parts`) are indexed by number only.

use std::{
    borrow::Cow,
    collections::HashMap,
    fs::File,
    io::{Cursor, Read, Seek, SeekFrom},
    path::Path,
    sync::Mutex,
};

use knife_perf::{Metric, Scope};

use anyhow::{Context, Result, ensure};
use flate2::bufread::{GzDecoder, ZlibDecoder};
#[cfg(not(target_os = "vita"))]
use memmap2::Mmap;

#[path = "archive_cache.rs"]
mod archive_cache;

pub const SECTOR: u64 = 0x800;

#[derive(Debug, Clone, Copy)]
pub struct ArchiveLimits {
    pub max_stored_bytes: usize,
    pub max_unpacked_bytes: usize,
}

impl ArchiveLimits {
    /// Largest current 540p UI entry is below 24 MiB. Keep each allocation
    /// bounded even though an entire story archive occupies several GiB.
    pub const VITA: Self = Self {
        max_stored_bytes: 32 * 1024 * 1024,
        max_unpacked_bytes: 32 * 1024 * 1024,
    };
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        if cfg!(target_os = "vita") {
            Self::VITA
        } else {
            Self {
                max_stored_bytes: 256 * 1024 * 1024,
                max_unpacked_bytes: 256 * 1024 * 1024,
            }
        }
    }
}

enum Storage {
    #[cfg(not(target_os = "vita"))]
    Mapped(Mmap),
    File(Mutex<File>),
}

impl Storage {
    fn read_range(&self, start: u64, len: usize) -> Result<Cow<'_, [u8]>> {
        match self {
            #[cfg(not(target_os = "vita"))]
            Self::Mapped(map) => {
                let start =
                    usize::try_from(start).context("archive offset exceeds address space")?;
                let end = start
                    .checked_add(len)
                    .context("archive mapped range overflow")?;
                Ok(Cow::Borrowed(
                    map.get(start..end)
                        .context("archive mapped range outside file")?,
                ))
            }
            Self::File(file) => {
                let allocate = Scope::new(Metric::ArchiveBufferZero);
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(len)
                    .context("allocating bounded archive entry")?;
                bytes.resize(len, 0);
                allocate.finish(len as u64, 0);
                // The seek and complete read form one operation. File::try_clone
                // would share its seek position on several supported platforms.
                let lock_wait = Scope::new(Metric::ArchiveLock);
                let mut file = file
                    .lock()
                    .map_err(|_| anyhow::anyhow!("archive file lock poisoned"))?;
                lock_wait.finish(0, 0);
                let started = std::time::Instant::now();
                file.seek(SeekFrom::Start(start))
                    .context("seeking archive entry")?;
                file.read_exact(&mut bytes).context("short archive read")?;
                // Card time excludes NX inflation and decode by construction.
                crate::io_metrics::record_card_read(
                    started.elapsed().as_micros() as u64, len as u64);
                Ok(Cow::Owned(bytes))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub name: String,
    pub offset: u64,
    pub stored_sectors: u16,
    pub unpacked_sectors: u16,
}

/// Header-only admission plan. Compressed input scratch and retained decoded
/// output have separate bounds; the caller must reserve the latter before read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveReadPlan {
    pub stored_bytes: usize,
    pub decoded_bytes: usize,
}

pub struct Archive {
    pub name: String,
    mrg: Storage,
    mrg_len: u64,
    cache_instance: u64,
    limits: ArchiveLimits,
    entries: Vec<ArchiveEntry>,
    by_name: HashMap<String, usize>,
    by_stem: HashMap<String, usize>,
}

impl Archive {
    /// Open `<dir>/<base>.hed`, `.nam` (optional) and `.mrg`.
    /// Vita always uses seek/read; desktop keeps its read-only mapping.
    /// All three files are immutable inputs for this Archive's lifetime, for
    /// both backends (the desktop mapping already requires that contract).
    pub fn open(dir: &Path, base: &str) -> Result<Self> {
        Self::open_with(
            dir,
            base,
            ArchiveLimits::default(),
            !cfg!(target_os = "vita"),
        )
    }

    /// Select the bounded file backend explicitly, including in host tests.
    pub fn open_file(dir: &Path, base: &str) -> Result<Self> {
        Self::open_file_with_limits(dir, base, ArchiveLimits::default())
    }

    pub fn open_file_with_limits(dir: &Path, base: &str, limits: ArchiveLimits) -> Result<Self> {
        Self::open_with(dir, base, limits, false)
    }

    fn open_with(dir: &Path, base: &str, limits: ArchiveLimits, _mapped: bool) -> Result<Self> {
        let hed = std::fs::read(dir.join(format!("{base}.hed")))
            .with_context(|| format!("reading {base}.hed"))?;
        let nam = std::fs::read(dir.join(format!("{base}.nam"))).ok();
        let file = File::open(dir.join(format!("{base}.mrg")))
            .with_context(|| format!("opening {base}.mrg"))?;
        let mrg_len = file
            .metadata()
            .with_context(|| format!("stat {base}.mrg"))?
            .len();
        #[cfg(not(target_os = "vita"))]
        let mrg = if _mapped {
            // SAFETY: the game files are read-only inputs, never modified while mapped.
            Storage::Mapped(unsafe { Mmap::map(&file)? })
        } else {
            Storage::File(Mutex::new(file))
        };
        #[cfg(target_os = "vita")]
        let mrg = Storage::File(Mutex::new(file));

        let mut entries = Vec::new();
        for (i, e) in hed.as_chunks::<8>().0.iter().enumerate() {
            let offset = u32::from_le_bytes(e[0..4].try_into().unwrap());
            if offset == 0xFFFF_FFFF {
                break;
            }
            let name = nam
                .as_ref()
                .and_then(|n| n.get(i * 32..i * 32 + 32))
                .map(|r| {
                    let end = r
                        .iter()
                        .position(|&b| b == 0 || b == b'\r')
                        .unwrap_or(r.len());
                    String::from_utf8_lossy(&r[..end]).into_owned()
                })
                .unwrap_or_default();
            entries.push(ArchiveEntry {
                name,
                offset: offset as u64 * SECTOR,
                stored_sectors: u16::from_le_bytes([e[4], e[5]]),
                unpacked_sectors: u16::from_le_bytes([e[6], e[7]]),
            });
        }
        let mut by_name = HashMap::new();
        let mut by_stem = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            let upper = e.name.to_ascii_uppercase();
            if upper == "NOFILE.EXISTS" {
                continue;
            }
            let stem = upper
                .rsplit_once('.')
                .map_or(upper.as_str(), |(s, _)| s)
                .to_string();
            by_stem.entry(stem).or_insert(i);
            by_name.entry(upper).or_insert(i);
        }
        Ok(Self {
            name: base.to_string(),
            mrg,
            mrg_len,
            cache_instance: archive_cache::instance(),
            limits,
            entries,
            by_name,
            by_stem,
        })
    }

    pub fn entries(&self) -> &[ArchiveEntry] {
        &self.entries
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        self.by_name.get(&name.to_ascii_uppercase()).copied()
    }

    /// First entry whose name without extension matches (case-insensitive).
    pub fn find_stem(&self, stem: &str) -> Option<usize> {
        self.by_stem.get(&stem.to_ascii_uppercase()).copied()
    }

    /// Raw stored bytes, borrowed on desktop or one bounded allocation for file IO.
    /// Missing/NOFILE entries return None; malformed extents and IO fail explicitly.
    pub fn raw(&self, index: usize) -> Result<Option<Cow<'_, [u8]>>> {
        let Some(e) = self.entries.get(index) else {
            return Ok(None);
        };
        if e.name.eq_ignore_ascii_case("NOFILE.EXISTS") {
            return Ok(None);
        }
        self.raw_entry(e, None)
            .map(Some)
            .with_context(|| format!("{} entry {index} ({})", self.name, e.name))
    }

    /// Reads only the small NX header, never a body or decompression allocation.
    pub fn read_plan(&self, index: usize) -> Result<ArchiveReadPlan> {
        let entry = self.entries.get(index).context("missing archive entry")?;
        ensure!(!entry.name.eq_ignore_ascii_case("NOFILE.EXISTS"), "missing archive entry");
        self.entry_plan(entry, None).map(|p| p.sizes)
    }

    /// Unique identity of this immutable open archive, safe across path reuse.
    pub fn cache_identity(&self) -> u64 { self.cache_instance }

    fn cache_key(&self, e: &ArchiveEntry) -> archive_cache::Key {
        archive_cache::Key { instance: self.cache_instance, offset: e.offset,
            stored: e.stored_sectors, unpacked: e.unpacked_sectors }
    }

    fn entry_plan(&self, e: &ArchiveEntry, read_cap: Option<(usize, usize)>) -> Result<archive_cache::Plan> {
        let available = self.mrg_len.checked_sub(e.offset).context("archive entry starts outside file")?;
        let key = self.cache_key(e);
        let plan = if let Some(plan) = archive_cache::header(key) { plan } else {
            let head = self.mrg.read_range(e.offset, available.min(super::nx::HEADER_LEN as u64) as usize)?;
            let header = super::nx::header(&head)?;
            let stored = header.map_or_else(|| u64::from(e.stored_sectors).checked_mul(SECTOR), |h| Some(h.stored_len))
                .context("archive stored length overflow")?;
            let decoded = header.map_or(stored, |h| h.unpacked_len);
            archive_cache::Plan { sizes: ArchiveReadPlan {
                stored_bytes: usize::try_from(stored).context("archive entry exceeds address space")?,
                decoded_bytes: usize::try_from(decoded).context("decoded entry exceeds address space")?,
            }, kind: header.map(|h| h.kind) }
        };
        // Admission checks apply to cached and cold plans alike, before either
        // body IO or output allocation; speculative caps must never be bypassed.
        let ArchiveReadPlan { stored_bytes: stored, decoded_bytes: decoded } = plan.sizes;
        ensure!(decoded <= self.limits.max_unpacked_bytes, "archive unpacked length {decoded} exceeds limit {}", self.limits.max_unpacked_bytes);
        ensure!(stored <= self.limits.max_stored_bytes, "archive stored length {stored} exceeds limit {}", self.limits.max_stored_bytes);
        if let Some((stored_cap, decoded_cap)) = read_cap {
            ensure!(stored <= stored_cap && decoded <= decoded_cap, "archive read cap exceeded (stored/output)");
        }
        let end = e.offset.checked_add(stored as u64).context("archive range overflow")?;
        ensure!(end <= self.mrg_len, "archive entry ends outside file: {end} > {}", self.mrg_len);
        archive_cache::store_header(key, plan);
        Ok(plan)
    }

    fn raw_entry(&self, e: &ArchiveEntry, read_cap: Option<(usize, usize)>) -> Result<Cow<'_, [u8]>> {
        let plan = self.entry_plan(e, read_cap)?;
        let key = self.cache_key(e);
        if let Some(bytes) = archive_cache::tiny(key, plan.sizes.stored_bytes) { return Ok(Cow::Owned(bytes)); }
        let result = self.mrg.read_range(e.offset, plan.sizes.stored_bytes);
        let raw = match result {
            Ok(raw) => raw,
            Err(error) => { archive_cache::invalidate(key); return Err(error); }
        };
        // A full body must agree with the planned header, including on first
        // load. Archive mutation violates the open() contract; refuse mismatched
        // metadata rather than decoding into a reservation sized for old data.
        let header = match super::nx::header(&raw) {
            Ok(header) => header,
            Err(error) => { archive_cache::invalidate(key); return Err(error); }
        };
        let agrees = match (plan.kind, header) {
            (None, None) => true,
            (Some(kind), Some(h)) => h.kind == kind
                && h.stored_len == plan.sizes.stored_bytes as u64
                && h.unpacked_len == plan.sizes.decoded_bytes as u64,
            _ => false,
        };
        if !agrees { archive_cache::invalidate(key); }
        ensure!(agrees, "archive input changed after header planning");
        // Only read()/range's successful full NX validation admits tiny bytes.
        Ok(raw)
    }

    /// Entry contents with NX compression removed.
    pub fn read(&self, index: usize) -> Result<Vec<u8>> {
        let raw = self.raw(index)?.context("missing archive entry")?;
        if super::nx::kind(&raw).is_some() {
            let audit = Scope::new(Metric::NxFull);
            let started = std::time::Instant::now();
            let out = super::nx::decompress_bounded(&raw, self.limits.max_unpacked_bytes);
            crate::io_metrics::record_inflate(started.elapsed().as_micros() as u64);
            if out.is_ok() { archive_cache::store_tiny(self.cache_key(&self.entries[index]), &raw); }
            else { archive_cache::invalidate(self.cache_key(&self.entries[index])); }
            if knife_perf::enabled() {
                let name = &self.entries[index].name;
                audit.finish_hot(&format!("{}:{index}:{name}", self.name), out.as_ref().map_or(0, |v| v.len() as u64), raw.len() as u64);
            }
            out
        } else {
            archive_cache::store_tiny(self.cache_key(&self.entries[index]), &raw);
            Ok(raw.into_owned())
        }
    }

    /// Per-operation cap, checked against both NX lengths before reading the
    /// body or allocating a decompression output. Speculative image jobs use a
    /// tighter cap than archive/UI demand loads; the archive remains reusable.
    pub fn read_bounded(&self, index: usize, max_bytes: usize) -> Result<Vec<u8>> {
        self.read_bounded_lengths(index, max_bytes, max_bytes)
    }

    /// Separate body/output caps, checked again against the current header before
    /// allocating either. This preserves an atlas's exact decoded reservation.
    pub fn read_bounded_lengths(&self, index: usize, stored_cap: usize, decoded_cap: usize) -> Result<Vec<u8>> {
        let e = self.entries.get(index).context("missing archive entry")?;
        ensure!(!e.name.eq_ignore_ascii_case("NOFILE.EXISTS"), "missing archive entry");
        let raw = self.raw_entry(e, Some((stored_cap, decoded_cap)))?;
        if super::nx::kind(&raw).is_some() {
            let audit = Scope::new(Metric::NxFull);
            let started = std::time::Instant::now();
            let out = super::nx::decompress_bounded(&raw, decoded_cap.min(self.limits.max_unpacked_bytes));
            crate::io_metrics::record_inflate(started.elapsed().as_micros() as u64);
            if out.is_ok() { archive_cache::store_tiny(self.cache_key(e), &raw); }
            else { archive_cache::invalidate(self.cache_key(e)); }
            if knife_perf::enabled() {
                audit.finish_hot(&format!("{}:{index}:{}", self.name, e.name), out.as_ref().map_or(0, |v| v.len() as u64), raw.len() as u64);
            }
            out
        } else {
            archive_cache::store_tiny(self.cache_key(&self.entries[index]), &raw);
            Ok(raw.into_owned())
        }
    }

    /// Return just one decoded image or other subrange of an entry. NX is re-read
    /// and streamed through a small scratch buffer, checking the full declared
    /// output and compressed input; the complete decoded atlas is never retained.
    /// An uncompressed entry seeks straight to the requested bytes.
    pub fn read_uncompressed_range(&self, index: usize, offset: usize, len: usize) -> Result<Vec<u8>> {
        let e = self.entries.get(index).context("missing archive entry")?;
        ensure!(!e.name.eq_ignore_ascii_case("NOFILE.EXISTS"), "missing archive entry");
        let plan = self.entry_plan(e, None)?;
        let end = offset.checked_add(len).context("archive subrange overflow")?;
        if let Some(kind) = plan.kind {
            ensure!(end <= plan.sizes.decoded_bytes, "NX subrange exceeds decoded entry");
            let raw = self.raw(index)?.context("missing archive entry")?;
            let stored = plan.sizes.stored_bytes;
            let body = raw.get(super::nx::HEADER_LEN..stored).context("truncated NX entry")?;
            let expected = plan.sizes.decoded_bytes;
            let audit = Scope::new(Metric::NxRange);
            let started = std::time::Instant::now();
            let input = Cursor::new(body);
            let decoded = (|| -> Result<(Vec<u8>, u64)> { Ok(match kind {
                super::nx::NxKind::Gzip => {
                    let mut decoder = GzDecoder::new(input);
                    let bytes = collect_nx_range(&mut decoder, expected, offset, len)?;
                    (bytes, decoder.get_ref().position())
                }
                super::nx::NxKind::Zlib => {
                    let mut decoder = ZlibDecoder::new(input);
                    let bytes = collect_nx_range(&mut decoder, expected, offset, len)?;
                    (bytes, decoder.get_ref().position())
                }
            }) })();
            crate::io_metrics::record_inflate(started.elapsed().as_micros() as u64);
            let (bytes, consumed) = match decoded {
                Ok(decoded) => decoded,
                Err(error) => { archive_cache::invalidate(self.cache_key(e)); return Err(error); }
            };
            if consumed != body.len() as u64 { archive_cache::invalidate(self.cache_key(e)); }
            ensure!(consumed == body.len() as u64, "NX compressed entry has trailing bytes");
            archive_cache::store_tiny(self.cache_key(e), &raw);
            if audit.active() {
                audit.finish_hot(&format!("{}:{}:{}", self.name, index, e.name), expected as u64, len as u64);
            }
            Ok(bytes)
        } else {
            ensure!(end <= plan.sizes.stored_bytes, "archive subrange outside raw entry");
            let start = e.offset.checked_add(offset as u64).context("archive subrange offset overflow")?;
            Ok(self.mrg.read_range(start, len)?.into_owned())
        }
    }

    /// Required seekable UI path: refuse an NX entry instead of silently doing
    /// a full compressed read/inflate for every thumbnail. Only the member is
    /// allocated, and header/extent checks still precede the card read.
    pub fn read_direct_range(&self, index: usize, offset: usize, len: usize) -> Result<Vec<u8>> {
        let entry = self.entries.get(index).context("missing archive entry")?;
        ensure!(!entry.name.eq_ignore_ascii_case("NOFILE.EXISTS"), "missing archive entry");
        let plan = self.entry_plan(entry, None)?;
        ensure!(plan.kind.is_none(), "direct UI range requires offline raw KATL; entry is still NX compressed");
        let end = offset.checked_add(len).context("direct range overflow")?;
        ensure!(end <= plan.sizes.stored_bytes, "direct range outside archive entry");
        let start = entry.offset.checked_add(offset as u64).context("direct archive offset overflow")?;
        Ok(self.mrg.read_range(start, len)?.into_owned())
    }

    pub fn read_by_name(&self, name: &str) -> Result<Vec<u8>> {
        let i = self
            .find(name)
            .with_context(|| format!("{name} not in {}", self.name))?;
        self.read(i)
    }
}

fn collect_nx_range(decoder: &mut impl Read, expected: usize, offset: usize, len: usize) -> Result<Vec<u8>> {
    let mut selected = Vec::new();
    selected.try_reserve_exact(len).context("allocating archive subrange")?;
    let mut decoded = 0usize;
    let mut scratch = [0u8; 8192];
    loop {
        let n = decoder.read(&mut scratch).context("decoding archive subrange")?;
        if n == 0 { break; }
        ensure!(n <= expected.saturating_sub(decoded), "NX expansion exceeds declared length");
        let to = decoded + n;
        let from = decoded.max(offset);
        let until = to.min(offset + len);
        if from < until { selected.extend_from_slice(&scratch[from - decoded..until - decoded]); }
        decoded = to;
    }
    ensure!(decoded == expected && selected.len() == len, "NX decoded entry length mismatch");
    knife_perf::add(Metric::RangeSelect, selected.len() as u64, 0);
    Ok(selected)
}

#[cfg(test)]
mod range_tests {
    use super::*;
    use std::{io::Write as _, sync::atomic::{AtomicUsize, Ordering}};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new(data: &[u8]) -> Self {
            let dir = std::env::temp_dir().join(format!("knife-range-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir(&dir).unwrap();
            let mut hed = vec![0u8; 16];
            hed[4..6].copy_from_slice(&1u16.to_le_bytes());
            hed[6..8].copy_from_slice(&1u16.to_le_bytes());
            hed[8..12].fill(255);
            std::fs::write(dir.join("ui.hed"), hed).unwrap();
            std::fs::write(dir.join("ui.nam"), b"IMAGE.NXZ\0").unwrap();
            std::fs::write(dir.join("ui.mrg"), data).unwrap();
            Self(dir)
        }
        fn open(&self) -> Archive { Archive::open_file_with_limits(&self.0, "ui", ArchiveLimits { max_stored_bytes: 2048, max_unpacked_bytes: 2048 }).unwrap() }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for suffix in ["hed", "nam", "mrg"] { let _ = std::fs::remove_file(self.0.join(format!("ui.{suffix}"))); }
            let _ = std::fs::remove_dir(&self.0);
        }
    }
    fn wrapped(input: &[u8], gzip: bool) -> Vec<u8> {
        let compressed = if gzip {
            let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(input).unwrap(); e.finish().unwrap()
        } else {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            e.write_all(input).unwrap(); e.finish().unwrap()
        };
        let mut out = if gzip { b"NXGX".to_vec() } else { b"NXCX".to_vec() };
        out.extend_from_slice(&(input.len() as u32).to_le_bytes());
        out.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&compressed);
        out
    }
    #[test]
    fn tiny_io_reuse_keeps_validation_caps_and_never_reuses_another_archive() {
        struct Flags(u64);
        impl Drop for Flags { fn drop(&mut self) { knife_perf::optimizations::set(self.0); } }
        let _flags = Flags(knife_perf::optimizations::mask());
        knife_perf::optimizations::set(knife_perf::optimizations::RESOURCE_IO_CACHE);
        let payload = wrapped(&[42u8;1000],false);
        let fixture = Fixture::new(&payload); let archive = fixture.open();
        let before = crate::io_metrics::snapshot();
        assert_eq!(archive.read(0).unwrap(),vec![42;1000]);
        let cold = crate::io_metrics::snapshot();
        assert_eq!(cold.card_read_calls-before.card_read_calls,2);
        assert_eq!(archive.read_uncompressed_range(0,20,7).unwrap(),[42;7]);
        assert_eq!(archive.read_bounded(0,1000).unwrap(),vec![42;1000]);
        let warm = crate::io_metrics::snapshot();
        assert_eq!(warm.card_read_calls,cold.card_read_calls);
        assert!(archive.read_bounded(0,999).is_err());
        assert!(archive.read_uncompressed_range(0,999,2).is_err());
        // A new Archive of the same path has a distinct identity. There is no
        // stale pointer or entry-index alias after instances are dropped.
        let other = fixture.open();
        assert_eq!(other.read(0).unwrap(),vec![42;1000]);
        assert_eq!(crate::io_metrics::snapshot().card_read_calls,warm.card_read_calls+2);
        // Full headers can succeed while compressed CRC/output validation fails.
        // Such input must never become a tiny-cache hit, even on repeat demand.
        let mut corrupt = payload.clone(); let last=corrupt.len()-1;corrupt[last]^=0x80;
        let broken=Fixture::new(&corrupt);let broken=broken.open();
        let start=crate::io_metrics::snapshot().card_read_calls;
        assert!(broken.read(0).is_err());assert!(broken.read(0).is_err());
        assert_eq!(crate::io_metrics::snapshot().card_read_calls,start+4);
        let truncated=Fixture::new(&payload[..16]);let truncated=truncated.open();
        let start=crate::io_metrics::snapshot().card_read_calls;
        assert!(truncated.read(0).is_err());assert!(truncated.read(0).is_err());
        assert_eq!(crate::io_metrics::snapshot().card_read_calls,start+2);
    }

    #[test]
    fn per_read_cap_rejects_nx_before_reading_missing_body_and_keeps_archive_usable() {
        let mut payload = wrapped(&[42u8; 1000], false);
        let fixture = Fixture::new(&payload);
        let archive = fixture.open();
        assert!(archive.read_bounded(0, 999).unwrap_err().to_string().contains("read cap"));
        assert_eq!(archive.read_bounded(0, 1000).unwrap(), vec![42u8; 1000]);
        // A huge declared stored body must be refused before allocation / IO.
        payload[8..12].copy_from_slice(&1900u32.to_le_bytes());
        let fixture = Fixture::new(&payload[..16]);
        let archive = fixture.open();
        assert!(archive.read_bounded(0, 1024).unwrap_err().to_string().contains("read cap"));
        let fixture = Fixture::new(&vec![7u8; 2048]);
        let archive = fixture.open();
        assert!(archive.read_bounded(0, 1024).unwrap_err().to_string().contains("read cap"));
        assert_eq!(archive.read_bounded(0, 2048).unwrap(), vec![7u8; 2048]);
    }

    #[test]
    fn planned_lengths_reserve_real_output_and_independent_input_budget() {
        for gzip in [false, true] {
            let payload=wrapped(&[42;1000],gzip);
            let fixture=Fixture::new(&payload);let archive=fixture.open();
            let plan=archive.read_plan(0).unwrap();
            assert_eq!(plan,ArchiveReadPlan{stored_bytes:payload.len(),decoded_bytes:1000});
            assert_eq!(archive.read_bounded_lengths(0,plan.stored_bytes,plan.decoded_bytes).unwrap(),vec![42;1000]);
            assert!(archive.read_bounded_lengths(0,plan.stored_bytes-1,plan.decoded_bytes).is_err());
            assert!(archive.read_bounded_lengths(0,plan.stored_bytes,plan.decoded_bytes-1).is_err());
        }
        // A body can be larger than the decoded output; its temporary read cap
        // must not force reclaiming that same amount of retained output cache.
        let payload=wrapped(b"x",false);let fixture=Fixture::new(&payload);let archive=fixture.open();
        let plan=archive.read_plan(0).unwrap();assert!(plan.stored_bytes>plan.decoded_bytes);
        assert_eq!(archive.read_bounded_lengths(0,plan.stored_bytes,1).unwrap(),b"x");
        let fixture=Fixture::new(&payload[..16]);assert!(fixture.open().read_plan(0).is_err());
    }

    #[test]
    fn nx_range_reads_only_selected_bytes_and_validates_full_stream() {
        let input: Vec<_> = (0..1000).map(|i| (i % 251) as u8).collect();
        for gzip in [false, true] {
            let fixture = Fixture::new(&wrapped(&input, gzip));
            let archive = fixture.open();
            for (offset, len) in [(0, 7), (241, 59), (999, 1), (1000, 0)] {
                assert_eq!(archive.read_uncompressed_range(0, offset, len).unwrap(), input[offset..offset + len]);
            }
            assert!(archive.read_uncompressed_range(0, 999, 2).is_err());
        }
    }
    #[test]
    fn truncated_and_overdeclared_nx_ranges_fail_bounded() {
        let mut payload = wrapped(&[1u8; 100], false);
        payload.truncate(payload.len() - 2);
        let fixture = Fixture::new(&payload);
        let archive = fixture.open();
        assert!(archive.read_uncompressed_range(0, 10, 4).is_err());
        let mut payload = wrapped(&[1u8; 100], false);
        payload[4..8].copy_from_slice(&4096u32.to_le_bytes());
        let fixture = Fixture::new(&payload);
        let archive = fixture.open();
        assert!(archive.read_uncompressed_range(0, 10, 4).is_err());
    }
    /// Exercise the exact compressed UI entries that collectively exceeded the
    /// renderer's encoded-source budget after opening gallery and sound UI.
    #[test]
    fn real_compact_atlases_reload_images_without_whole_decoded_entries() {
        let Some(root) = std::env::var_os("KNIFE_VITA_ASSETS") else { return; };
        let archive = Archive::open_file_with_limits(Path::new(&root), "allui", ArchiveLimits::VITA).unwrap();
        let mut encoded_total = 0usize;
        for stem in ["TITLE_PARTS_EN", "TITLE_PARTS_ZC", "DISP_PARTS_EN", "CG_PARTS", "SOUND_PARTS_EN", "SOUND_PARTS_ZC"] {
            let index = archive.find_stem(stem).unwrap();
            let entry = archive.read(index).unwrap();
            let images = super::super::compact::parse_atlas(&entry).unwrap();
            assert!(!images.is_empty(), "{stem}");
            for (_, image) in &images { encoded_total += image.len(); }
            for &position in &[0, images.len() - 1] {
                let image = images[position].1;
                let offset = (image.as_ptr() as usize) - (entry.as_ptr() as usize);
                assert!(offset + image.len() <= entry.len());
                let reloaded = archive.read_uncompressed_range(index, offset, image.len()).unwrap();
                assert_eq!(reloaded, image, "{stem} image {position}");
            }
        }
        assert!(encoded_total > 32 * 1024 * 1024, "selected UI image sources must reproduce budget pressure: {encoded_total}");
    }
    #[test]
    fn raw_range_seeks_directly_with_sector_bounds() {
        let mut payload = vec![0u8; 2048];
        payload[120..124].copy_from_slice(b"KIMG");
        let fixture = Fixture::new(&payload);
        let archive = fixture.open();
        assert_eq!(archive.read_uncompressed_range(0, 120, 4).unwrap(), b"KIMG");
        assert!(archive.read_uncompressed_range(0, 2047, 2).is_err());
    }

    #[test]
    fn direct_ui_range_never_inflates_a_compressed_entry_or_exceeds_bounds() {
        let mut payload = vec![0u8; 2048];
        payload[120..124].copy_from_slice(b"KIMG");
        let raw = Fixture::new(&payload);
        let archive = raw.open();
        assert_eq!(archive.read_direct_range(0, 120, 4).unwrap(), b"KIMG");
        assert!(archive.read_direct_range(0, 2047, 2).is_err());
        assert!(archive.read_direct_range(0, usize::MAX, 4).is_err());
        for gzip in [false, true] {
            let nx = Fixture::new(&wrapped(&payload, gzip));
            let error = nx.open().read_direct_range(0, 120, 4).unwrap_err();
            assert!(error.to_string().contains("still NX compressed"));
        }
    }
}

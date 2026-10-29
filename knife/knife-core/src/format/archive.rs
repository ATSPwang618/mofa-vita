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

use anyhow::{Context, Result, ensure};
use flate2::bufread::{GzDecoder, ZlibDecoder};
#[cfg(not(target_os = "vita"))]
use memmap2::Mmap;

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
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(len)
                    .context("allocating bounded archive entry")?;
                bytes.resize(len, 0);
                // The seek and complete read form one operation. File::try_clone
                // would share its seek position on several supported platforms.
                let mut file = file
                    .lock()
                    .map_err(|_| anyhow::anyhow!("archive file lock poisoned"))?;
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

pub struct Archive {
    pub name: String,
    mrg: Storage,
    mrg_len: u64,
    limits: ArchiveLimits,
    entries: Vec<ArchiveEntry>,
    by_name: HashMap<String, usize>,
    by_stem: HashMap<String, usize>,
}

impl Archive {
    /// Open `<dir>/<base>.hed`, `.nam` (optional) and `.mrg`.
    /// Vita always uses seek/read; desktop keeps its read-only mapping.
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

    fn raw_entry(&self, e: &ArchiveEntry, read_cap: Option<usize>) -> Result<Cow<'_, [u8]>> {
        let available = self
            .mrg_len
            .checked_sub(e.offset)
            .context("archive entry starts outside file")?;
        let head = self.mrg.read_range(
            e.offset,
            available.min(super::nx::HEADER_LEN as u64) as usize,
        )?;
        let len = if let Some(header) = super::nx::header(&head)? {
            if let Some(cap) = read_cap {
                ensure!(header.unpacked_len <= cap as u64 && header.stored_len <= cap as u64,
                    "archive read cap {cap} exceeded by stored/decoded NX entry");
            }
            ensure!(
                header.unpacked_len <= self.limits.max_unpacked_bytes as u64,
                "archive unpacked length {} exceeds limit {}",
                header.unpacked_len,
                self.limits.max_unpacked_bytes
            );
            header.stored_len
        } else {
            let len = u64::from(e.stored_sectors)
                .checked_mul(SECTOR)
                .context("archive stored length overflow")?;
            ensure!(
                len <= self.limits.max_unpacked_bytes as u64,
                "archive unpacked length {len} exceeds limit {}",
                self.limits.max_unpacked_bytes
            );
            len
        };
        if let Some(cap) = read_cap {
            ensure!(len <= cap as u64, "archive read cap {cap} exceeded by raw entry");
        }
        ensure!(
            len <= self.limits.max_stored_bytes as u64,
            "archive stored length {len} exceeds limit {}",
            self.limits.max_stored_bytes
        );
        let end = e
            .offset
            .checked_add(len)
            .context("archive range overflow")?;
        ensure!(
            end <= self.mrg_len,
            "archive entry ends outside file: {end} > {}",
            self.mrg_len
        );
        let len = usize::try_from(len).context("archive entry exceeds address space")?;
        self.mrg.read_range(e.offset, len)
    }

    /// Entry contents with NX compression removed.
    pub fn read(&self, index: usize) -> Result<Vec<u8>> {
        let raw = self.raw(index)?.context("missing archive entry")?;
        if super::nx::kind(&raw).is_some() {
            let started = std::time::Instant::now();
            let out = super::nx::decompress_bounded(&raw, self.limits.max_unpacked_bytes);
            crate::io_metrics::record_inflate(started.elapsed().as_micros() as u64);
            out
        } else {
            Ok(raw.into_owned())
        }
    }

    /// Per-operation cap, checked against both NX lengths before reading the
    /// body or allocating a decompression output. Speculative image jobs use a
    /// tighter cap than archive/UI demand loads; the archive remains reusable.
    pub fn read_bounded(&self, index: usize, max_bytes: usize) -> Result<Vec<u8>> {
        let e = self.entries.get(index).context("missing archive entry")?;
        ensure!(!e.name.eq_ignore_ascii_case("NOFILE.EXISTS"), "missing archive entry");
        let raw = self.raw_entry(e, Some(max_bytes))?;
        if super::nx::kind(&raw).is_some() {
            let started = std::time::Instant::now();
            let out = super::nx::decompress_bounded(&raw, max_bytes.min(self.limits.max_unpacked_bytes));
            crate::io_metrics::record_inflate(started.elapsed().as_micros() as u64);
            out
        } else {
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
        let available = self.mrg_len.checked_sub(e.offset).context("archive entry starts outside file")?;
        let head = self.mrg.read_range(e.offset, available.min(super::nx::HEADER_LEN as u64) as usize)?;
        let end = offset.checked_add(len).context("archive subrange overflow")?;
        if let Some(header) = super::nx::header(&head)? {
            ensure!(header.unpacked_len <= self.limits.max_unpacked_bytes as u64, "NX unpacked length exceeds archive limit");
            ensure!(end as u64 <= header.unpacked_len, "NX subrange exceeds decoded entry");
            let raw = self.raw(index)?.context("missing archive entry")?;
            let stored = usize::try_from(header.stored_len).context("NX stored length exceeds address space")?;
            let body = raw.get(super::nx::HEADER_LEN..stored).context("truncated NX entry")?;
            let expected = usize::try_from(header.unpacked_len).context("NX decoded length exceeds address space")?;
            let input = Cursor::new(body);
            let (bytes, consumed) = match header.kind {
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
            };
            ensure!(consumed == body.len() as u64, "NX compressed entry has trailing bytes");
            Ok(bytes)
        } else {
            let stored = u64::from(e.stored_sectors).checked_mul(SECTOR).context("archive stored length overflow")?;
            ensure!(stored <= self.limits.max_stored_bytes as u64 && stored <= self.limits.max_unpacked_bytes as u64, "archive raw entry exceeds limits");
            ensure!(stored <= available && end as u64 <= stored, "archive subrange outside raw entry");
            let start = e.offset.checked_add(offset as u64).context("archive subrange offset overflow")?;
            Ok(self.mrg.read_range(start, len)?.into_owned())
        }
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
}

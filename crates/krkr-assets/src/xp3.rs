//! XP3 index loading; segment streams use independent file handles.
mod index;
#[cfg(not(target_os = "vita"))]
pub mod offline;
mod portable;
mod reader;
mod writer;
use crate::{
    Error, Limits, Result,
    binary::{Cursor, inflate, size},
    file::{self, File},
    name,
};
pub use index::Entries;
pub use reader::Reader;
use std::{
    collections::HashSet,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};
pub use writer::{Compression, Writer};

pub const SIGNATURE: &[u8; 11] = b"XP3\r\n \n\x1a\x8b\x67\x01";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}
impl Version {
    pub fn of(file: &File) -> Result<Self> {
        #[cfg(target_os = "vita")]
        {
            Ok(file.version()?)
        }
        #[cfg(not(target_os = "vita"))]
        {
            Ok(Self::from_metadata(&file.metadata()?))
        }
    }
    #[cfg(not(target_os = "vita"))]
    pub(crate) fn from_metadata(meta: &std::fs::Metadata) -> Self {
        Self {
            bytes: meta.len(),
            modified: meta.modified().ok(),
        }
    }
    pub fn check(&self, file: &File) -> Result<()> {
        if self != &Self::of(file)? {
            return Err(Error::Changed);
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct Segment {
    pub offset: u64,
    pub logical: u64,
    pub original: u64,
    pub stored: u64,
    pub compressed: bool,
}
#[derive(Debug)]
pub struct Entry {
    pub name: Vec<u16>,
    pub size: u64,
    pub hash: u32,
    pub protected: bool,
    pub segments: Vec<Segment>,
}
pub struct Archive {
    pub path: PathBuf,
    pub version: Version,
    pub entries: Entries,
    pub index_bytes: usize,
}

/// Factories create independent per-stream state; filters run after decompression
/// and receive uncompressed offsets, entry metadata and normalized resource names.
pub trait FilterFactory: Send + Sync {
    fn create(&self, storage: &[u16], entry: &Entry) -> Result<Box<dyn Filter>>;
}
pub trait Filter: Send {
    fn fetch_full_data(&self) -> bool {
        false
    }
    fn apply(&mut self, logical_offset: u64, bytes: &mut [u8]) -> std::io::Result<()>;
}

fn offset(file: &mut File) -> Result<u64> {
    let mut header = [0; 11];
    file.read_exact(&mut header)?;
    if &header == SIGNATURE {
        return Ok(0);
    }
    if !header.starts_with(b"MZ") {
        return Err(Error::Format("XP3 signature missing"));
    }
    file.seek(SeekFrom::Start(16))?;
    let mut buffer = vec![0; 64 * 1024 + SIGNATURE.len() - 1];
    let mut start = 16u64;
    let mut kept = 0;
    loop {
        let read = file.read(&mut buffer[kept..])?;
        if read == 0 {
            return Err(Error::Format("embedded XP3 signature missing"));
        }
        let filled = kept + read;
        for at in memchr::memmem::find_iter(&buffer[..filled], SIGNATURE) {
            let position = start + at as u64;
            if position % 16 == 0 {
                return Ok(position);
            }
        }
        kept = filled.min(SIGNATURE.len() - 1);
        buffer.copy_within(filled - kept..filled, 0);
        start += (filled - kept) as u64;
    }
}
fn u64_at(file: &mut File) -> Result<u64> {
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}
fn range(start: u64, len: u64, total: u64) -> Result<()> {
    if start.checked_add(len).is_none_or(|end| end > total) {
        return Err(Error::Format("XP3 range exceeds file"));
    }
    Ok(())
}
impl Archive {
    pub fn load(path: &Path, limits: Limits) -> Result<Self> {
        Self::load_impl(path, limits, false)
    }
    /// Offline export must reject unsafe names and normalized duplicates instead
    /// of silently choosing the first entry, as the runtime lookup does.
    pub fn load_strict(path: &Path, limits: Limits) -> Result<Self> {
        Self::load_impl(path, limits, true)
    }
    fn load_impl(path: &Path, limits: Limits, strict: bool) -> Result<Self> {
        let mut file = file::open(path)?;
        let version = Version::of(&file)?;
        #[cfg(target_os = "vita")]
        krkr_protocol::diagnostic!(
            "[VITA][XP3] open {} bytes={}",
            path.display(),
            version.bytes
        );
        let base = offset(&mut file)?;
        file.seek(SeekFrom::Start(base + 11))?;
        let mut next = u64_at(&mut file)?;
        let mut visited = HashSet::new();
        let mut entries = index::Builder::default();
        let mut decoded_bytes = 0;
        loop {
            if !visited.insert(next) {
                return Err(Error::Format("cyclic XP3 index"));
            }
            if visited.len() > limits.max_entries {
                return Err(Error::Limit("index chain"));
            }
            let index_at = base
                .checked_add(next)
                .ok_or(Error::Format("XP3 index overflow"))?;
            range(index_at, 9, version.bytes)?;
            #[cfg(target_os = "vita")]
            krkr_protocol::diagnostic!(
                "[VITA][XP3] index seek BEGIN {} offset={index_at}",
                path.display()
            );
            let actual = file.seek(SeekFrom::Start(index_at))?;
            if actual != index_at {
                return Err(std::io::Error::other(format!(
                    "XP3 index seek returned wrong position: {} requested={index_at} actual={actual}",
                    path.display()
                )).into());
            }
            #[cfg(target_os = "vita")]
            krkr_protocol::diagnostic!("[VITA][XP3] index seek OK");
            let mut header = [0u8; 9];
            file.read_exact(&mut header)?;
            let flags = header[0];
            let stored = u64::from_le_bytes(header[1..].try_into().unwrap());
            let original = match flags & 7 {
                0 => stored,
                1 => u64_at(&mut file)?,
                _ => {
                    // Fail rather than guessing an encoding. A bounded second
                    // read distinguishes stable file bytes from a bad seek or
                    // inconsistent IO in the device log, without masking it.
                    let mut reread = [0u8; 9];
                    let check = file.seek(SeekFrom::Start(index_at)).and_then(|at| {
                        if at != index_at {
                            return Err(std::io::Error::other(format!("seek returned {at}")));
                        }
                        file.read_exact(&mut reread)
                    });
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!(
                        "unsupported XP3 index encoding: {} offset={index_at} file_bytes={} flags=0x{flags:02x} header={header:02x?} reread={reread:02x?} check={check:?}",
                        path.display(), version.bytes
                    )).into());
                }
            };
            #[cfg(target_os = "vita")]
            krkr_protocol::diagnostic!(
                "[VITA][XP3] index flags=0x{flags:02x} stored={stored} decoded={original}"
            );
            let read_size = size(stored, limits.max_index_bytes, "index")?;
            let original = size(
                original,
                limits.max_index_bytes.saturating_sub(decoded_bytes),
                "index",
            )?;
            decoded_bytes += original;
            range(file.stream_position()?, stored, version.bytes)?;
            let mut data = vec![0; read_size];
            file.read_exact(&mut data)?;
            let data = if flags & 7 == 1 {
                inflate(&data, original)?
            } else {
                data
            };
            let mut chunks = Cursor(&data);
            entries.reserve_chunk(&data, limits)?;
            while let Some((tag, chunk)) = chunks.next_chunk()? {
                if &tag != b"File" {
                    continue;
                }
                let mut info = None;
                let mut segments = None;
                let mut hash = None;
                let mut chunks = Cursor(chunk);
                while let Some((tag, data)) = chunks.next_chunk()? {
                    match &tag {
                        b"info" => info = Some(data),
                        b"segm" => segments = Some(data),
                        b"adlr" => hash = Some(data),
                        _ => {}
                    }
                }
                let mut info = Cursor(info.ok_or(Error::Format("XP3 info chunk missing"))?);
                let protected = info.u32()? & 0x80000000 != 0;
                let len = info.u64()?;
                let archived = info.u64()?;
                let name_len = info.u16()? as usize;
                let raw_name = info
                    .take(name_len * 2)?
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]]))
                    .collect::<Vec<_>>();
                let name = if strict {
                    portable::portable_name(&raw_name)?
                } else {
                    name::archive(&raw_name)?
                };
                let hash = Cursor(hash.ok_or(Error::Format("XP3 adlr chunk missing"))?).u32()?;
                let mut raw = Cursor(segments.ok_or(Error::Format("XP3 segm chunk missing"))?);
                if raw.0.len() % 28 != 0 {
                    return Err(Error::Format("invalid XP3 segment table"));
                }
                let segment_start = entries.segments.len();
                let mut logical = 0u64;
                let mut total_stored = 0u64;
                while !raw.0.is_empty() {
                    let compressed = match raw.u32()? & 7 {
                        0 => false,
                        1 => true,
                        _ => return Err(Error::Format("unsupported XP3 segment encoding")),
                    };
                    let offset = base
                        .checked_add(raw.u64()?)
                        .ok_or(Error::Format("XP3 segment overflow"))?;
                    let original = raw.u64()?;
                    let stored = raw.u64()?;
                    range(offset, stored, version.bytes)?;
                    if !compressed && original != stored {
                        return Err(Error::Format("raw XP3 segment size mismatch"));
                    }
                    entries.segments.push(Segment {
                        offset,
                        logical,
                        original,
                        stored,
                        compressed,
                    });
                    logical = logical
                        .checked_add(original)
                        .ok_or(Error::Format("XP3 logical size overflow"))?;
                    total_stored = total_stored
                        .checked_add(stored)
                        .ok_or(Error::Format("XP3 stored size overflow"))?;
                }
                if logical != len || total_stored != archived {
                    return Err(Error::Format("XP3 entry size mismatch"));
                }
                entries.push(&name, len, hash, protected, segment_start)?;
            }
            if flags & 0x80 == 0 {
                break;
            }
            next = u64_at(&mut file)?;
        }
        version.check(&file)?;
        let entries = entries.finish(strict)?;
        let index_bytes = entries.retained_bytes();
        #[cfg(target_os = "vita")]
        krkr_protocol::diagnostic!(
            "[VITA][XP3] index ready entries={} retained={}KiB",
            entries.len(),
            index_bytes / 1024
        );
        Ok(Self {
            path: path.to_owned(),
            version,
            entries,
            index_bytes,
        })
    }
}

//! Seekable ZIP streams with shared immutable directory metadata. Each entry
//! keeps its own decoder; backward seeks restart it, never cache the whole file.
use krkr_engine::assets::{self, Limits, ReadPlan, ReadSource, Stream};
use self_cell::{MutBorrow, self_cell};
use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

pub(super) fn error(e: impl std::fmt::Display) -> assets::Error {
    io::Error::other(e.to_string()).into()
}
pub(super) fn cancel(check: &dyn Fn() -> bool) -> io::Result<()> {
    if check() {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "ZIP operation cancelled",
        ))
    } else {
        Ok(())
    }
}
/// Admit the directory before the ZIP library allocates its metadata tables.
/// Read only the bounded end record/comment and, when present, ZIP64 header.
pub(super) fn admit(reader: &mut (impl Read + Seek), limits: Limits) -> assets::Result<()> {
    let length = reader.seek(SeekFrom::End(0))?;
    let count = length.min(65557) as usize;
    let mut tail = vec![0; count];
    reader.seek(SeekFrom::Start(length - count as u64))?;
    reader.read_exact(&mut tail)?;
    let at = (0..count.saturating_sub(21))
        .rev()
        .find(|&at| {
            tail[at..at + 4] == *b"PK\x05\x06"
                && at + 22 + usize::from(u16::from_le_bytes([tail[at + 20], tail[at + 21]]))
                    == count
        })
        .ok_or(assets::Error::Format("ZIP end record missing"))?;
    let mut entries = u64::from(u16::from_le_bytes([tail[at + 10], tail[at + 11]]));
    let mut directory = u64::from(u32::from_le_bytes(
        tail[at + 12..at + 16].try_into().expect("four bytes"),
    ));
    let end = length - count as u64 + at as u64;
    if end >= 20 {
        let mut locator = [0; 20];
        reader.seek(SeekFrom::Start(end - 20))?;
        reader.read_exact(&mut locator)?;
        if locator[..4] == *b"PK\x06\x07" {
            let offset = u64::from_le_bytes(locator[8..16].try_into().expect("eight bytes"));
            let mut header = [0; 56];
            reader.seek(SeekFrom::Start(offset))?;
            reader.read_exact(&mut header)?;
            if header[..4] != *b"PK\x06\x06" && end >= 76 {
                // ZIP64 files may have a self-extracting executable prefix.
                reader.seek(SeekFrom::Start(end - 76))?;
                reader.read_exact(&mut header)?;
            }
            if header[..4] != *b"PK\x06\x06" {
                return Err(assets::Error::Format("ZIP64 end record missing"));
            }
            entries = u64::from_le_bytes(header[32..40].try_into().expect("eight bytes"));
            directory = u64::from_le_bytes(header[40..48].try_into().expect("eight bytes"));
        }
    }
    if entries > limits.max_entries as u64 {
        return Err(assets::Error::Limit("ZIP entries"));
    }
    if entries
        .saturating_mul(512)
        .saturating_add(directory.saturating_mul(4))
        > limits.max_index_bytes as u64
    {
        return Err(assets::Error::Limit("ZIP index"));
    }
    reader.rewind()?;
    Ok(())
}
#[derive(Clone)]
pub(super) struct Reader {
    input: Arc<Mutex<Box<dyn Stream>>>,
    position: u64,
}
impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut input = self
            .input
            .lock()
            .map_err(|_| io::Error::other("ZIP stream poisoned"))?;
        input.seek(SeekFrom::Start(self.position))?;
        let count = input.read(out)?;
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for Reader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let mut input = self
            .input
            .lock()
            .map_err(|_| io::Error::other("ZIP stream poisoned"))?;
        input.seek(SeekFrom::Start(self.position))?;
        self.position = input.seek(from)?;
        Ok(self.position)
    }
}
pub(super) struct Entry {
    pub name: String,
    pub size: u64,
    pub compressed: u64,
    pub crc: u32,
    pub flags: u16,
    pub deflated: bool,
    pub date: Option<i64>,
}
pub(super) struct Archive {
    zip: zip::ZipArchive<Reader>,
    pub entries: Vec<Entry>,
    pub index_bytes: usize,
    limit: usize,
}
impl Archive {
    pub fn load(plan: ReadPlan, limits: Limits, check: &dyn Fn() -> bool) -> assets::Result<Self> {
        let mut reader = Reader {
            input: Arc::new(Mutex::new(plan.open_interruptible(check)?)),
            position: 0,
        };
        admit(&mut reader, limits)?;
        cancel(check)?;
        let mut zip = zip::ZipArchive::new(reader.clone()).map_err(error)?;
        if zip.len() > limits.max_entries {
            return Err(assets::Error::Limit("ZIP entries"));
        }
        let mut entries = Vec::with_capacity(zip.len());
        let mut index_bytes = 0usize;
        for i in 0..zip.len() {
            cancel(check)?;
            let file = zip.by_index_raw(i).map_err(error)?;
            reader.seek(SeekFrom::Start(file.central_header_start() + 8))?;
            let mut flags = [0; 2];
            reader.read_exact(&mut flags)?;
            let flags = u16::from_le_bytes(flags);
            // Legacy Japanese games use CP932. Per-entry UTF-8 flags also
            // handle mixed archives written by modern ZIP tools correctly.
            let name = if flags & 0x800 != 0 {
                String::from_utf8_lossy(file.name_raw())
            } else {
                encoding_rs::SHIFT_JIS.decode(file.name_raw()).0
            }
            .into_owned();
            index_bytes = index_bytes
                .checked_add(name.len().saturating_mul(4) + 512)
                .ok_or(assets::Error::Limit("ZIP index"))?;
            if index_bytes > limits.max_index_bytes {
                return Err(assets::Error::Limit("ZIP index"));
            }
            let date = file.last_modified().and_then(|d| {
                let dt = jiff::civil::DateTime::new(
                    d.year() as i16,
                    d.month() as i8,
                    d.day() as i8,
                    d.hour() as i8,
                    d.minute() as i8,
                    d.second() as i8,
                    0,
                )
                .ok()?;
                tjs_bind::date::timezone::system()
                    .to_timestamp(dt)
                    .ok()
                    .map(|t| t.as_millisecond())
            });
            entries.push(Entry {
                name,
                size: file.size(),
                compressed: file.compressed_size(),
                crc: file.crc32(),
                flags,
                deflated: file.compression() == zip::CompressionMethod::Deflated,
                date,
            });
        }
        Ok(Self {
            zip,
            entries,
            index_bytes,
            limit: limits.max_read_bytes,
        })
    }
    pub fn find(&self, name: &str) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.name.eq_ignore_ascii_case(name))
    }
    pub fn open(&self, index: usize, password: Option<Vec<u8>>) -> assets::Result<EntryStream> {
        if self.entries[index].size > self.limit as u64 {
            return Err(assets::Error::Limit("ZIP entry bytes"));
        }
        EntryStream::new(self.zip.clone(), index, password, self.entries[index].size).map_err(error)
    }
}
type File<'a> = zip::read::ZipFile<'a, Reader>;
self_cell! {
    struct Cell {
        owner: MutBorrow<zip::ZipArchive<Reader>>,
        #[not_covariant]
        dependent: File,
    }
}
pub(super) struct EntryStream {
    template: zip::ZipArchive<Reader>,
    cell: Cell,
    index: usize,
    password: Option<Vec<u8>>,
    size: u64,
    position: u64,
}
impl EntryStream {
    fn cell(
        zip: zip::ZipArchive<Reader>,
        index: usize,
        password: Option<&[u8]>,
    ) -> io::Result<Cell> {
        Cell::try_new(MutBorrow::new(zip), |owner| {
            let zip = owner.borrow_mut();
            match password {
                Some(p) => zip.by_index_decrypt(index, p),
                None => zip.by_index(index),
            }
        })
        .map_err(io::Error::other)
    }
    fn new(
        template: zip::ZipArchive<Reader>,
        index: usize,
        password: Option<Vec<u8>>,
        size: u64,
    ) -> io::Result<Self> {
        let cell = Self::cell(template.clone(), index, password.as_deref())?;
        Ok(Self {
            template,
            cell,
            index,
            password,
            size,
            position: 0,
        })
    }
}
impl Read for EntryStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.position > self.size {
            return Ok(0);
        }
        if out.is_empty() {
            return Ok(0);
        }
        if self.position == self.size {
            let mut tail = [0];
            if self
                .cell
                .with_dependent_mut(|_, file| file.read(&mut tail))?
                != 0
            {
                return Err(io::Error::other("ZIP entry exceeds declared size"));
            }
            return Ok(0);
        }
        let available = (self.size - self.position).min(out.len() as u64) as usize;
        let n = self
            .cell
            .with_dependent_mut(|_, file| file.read(&mut out[..available]))?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated ZIP entry",
            ));
        }
        self.position += n as u64;
        Ok(n)
    }
}
impl Seek for EntryStream {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.size) + i128::from(n),
        };
        let target = u64::try_from(target).map_err(|_| io::Error::other("invalid ZIP seek"))?;
        if target < self.position {
            self.cell = Self::cell(self.template.clone(), self.index, self.password.as_deref())?;
            self.position = 0;
        }
        let mut buffer = [0; 16384];
        while self.position < target.min(self.size) {
            let n = (target.min(self.size) - self.position).min(buffer.len() as u64) as usize;
            self.read_exact(&mut buffer[..n])?;
        }
        self.position = target;
        Ok(target)
    }
}
pub(super) struct Source {
    pub archive: Arc<Archive>,
    pub index: usize,
}
impl ReadSource for Source {
    fn open(&self) -> assets::Result<Box<dyn Stream>> {
        Ok(Box::new(self.archive.open(self.index, None)?))
    }
}

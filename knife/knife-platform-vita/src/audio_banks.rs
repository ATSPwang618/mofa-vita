//! Seekable raw Ogg banks. Index records are sorted by name and payloads are dense.
use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

const MAGIC: &[u8; 8] = b"KAUDMRG1";
const HEADER: usize = 16;
const RECORD: usize = 40;
const NAME_BYTES: usize = 32;
const MAX_MEMBERS: usize = 16_384;
const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const READ_CHUNK: usize = 32 * 1024;
pub(crate) const VOICE: &str = "voice.mrg";
pub(crate) const BGM: &str = "bgm.mrg";

struct BankFile {
    file: BufReader<File>,
    position: Option<u64>,
}

struct Bank {
    index: Box<[u8]>,
    input: Mutex<BankFile>,
}

fn u32_le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("4 byte bank field"))
}

fn name(record: &[u8]) -> &[u8] {
    record[..NAME_BYTES].split(|&byte| byte == 0).next().unwrap()
}

fn extent(record: &[u8]) -> (u32, u32) {
    (u32_le(&record[32..36]), u32_le(&record[36..40]))
}

impl Bank {
    fn open(path: &Path) -> io::Result<Self> {
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid audio bank index");
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        if !(HEADER as u64..=MAX_BYTES).contains(&length) {
            return Err(invalid());
        }
        let mut header = [0; HEADER];
        file.read_exact(&mut header)?;
        let count = u32_le(&header[8..12]) as usize;
        if &header[..8] != MAGIC || !(1..=MAX_MEMBERS).contains(&count) {
            return Err(invalid());
        }
        let index_bytes = count * RECORD;
        let data_start = (HEADER + index_bytes) as u64;
        if data_start >= length {
            return Err(invalid());
        }
        let mut index = vec![0; index_bytes].into_boxed_slice();
        file.read_exact(&mut index)?;
        if crc32fast::hash(&index) != u32_le(&header[12..16]) {
            return Err(invalid());
        }
        let mut previous: &[u8] = b"";
        let mut end = data_start;
        for record in index.chunks_exact(RECORD) {
            let key = name(record);
            let (offset, size) = extent(record);
            if key <= previous || key.len() == NAME_BYTES
                || record[key.len()..NAME_BYTES].iter().any(|&byte| byte != 0)
                || !key.iter().all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(byte))
                || !(key.ends_with(b".ogg") || key.ends_with(b".bgmstart.mrg"))
                || u64::from(offset) != end || size == 0
            {
                return Err(invalid());
            }
            end += u64::from(size);
            previous = key;
        }
        if end != length {
            return Err(invalid());
        }
        let file = BufReader::with_capacity(8192, file);
        Ok(Self { index, input: Mutex::new(BankFile { file, position: Some(data_start) }) })
    }

    fn count(&self) -> usize {
        self.index.len() / RECORD
    }

    fn find(&self, key: &[u8]) -> Option<(u32, u32)> {
        let (mut low, mut high) = (0, self.count());
        while low < high {
            let mid = low + (high - low) / 2;
            let record = &self.index[mid * RECORD..(mid + 1) * RECORD];
            match name(record).cmp(key) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Some(extent(record)),
            }
        }
        None
    }
}

pub struct AudioBanks {
    stream: PathBuf,
    voice: Arc<Bank>,
    bgm: Arc<Bank>,
}

impl AudioBanks {
    /// Both files are opened once. A partial installation is an error.
    pub fn open(root: &Path) -> Result<Option<Self>, (&'static str, io::Error)> {
        let voice = Bank::open(&root.join(VOICE));
        let bgm = Bank::open(&root.join(BGM));
        if matches!(&voice, Err(error) if error.kind() == io::ErrorKind::NotFound)
            && matches!(&bgm, Err(error) if error.kind() == io::ErrorKind::NotFound)
        {
            return Ok(None);
        }
        Ok(Some(Self {
            stream: root.join("stream"),
            voice: Arc::new(voice.map_err(|error| (VOICE, error))?),
            bgm: Arc::new(bgm.map_err(|error| (BGM, error))?),
        }))
    }

    pub fn counts(&self) -> [usize; 2] {
        [self.voice.count(), self.bgm.count()]
    }

    fn handles(&self, path: &Path) -> bool {
        path.parent() == Some(self.stream.as_path())
    }

    pub fn entry(&self, path: &Path) -> Option<AudioRange> {
        if !self.handles(path) {
            return None;
        }
        let key = path.file_name()?.to_str()?.as_bytes();
        for bank in [&self.bgm, &self.voice] {
            if let Some((offset, length)) = bank.find(key) {
                return Some(AudioRange { bank: bank.clone(), offset, length });
            }
        }
        None
    }
}

/// Vita boots from a single assets directory. Publishing the validated banks
/// gives all audio workers access to the same file handles.
static BANKS: OnceLock<AudioBanks> = OnceLock::new();

pub fn install(banks: AudioBanks) -> io::Result<()> {
    BANKS.set(banks).map_err(|_| io::Error::other("audio banks already mounted"))
}

pub fn packed_path(path: &Path) -> bool {
    BANKS.get().is_some_and(|banks| banks.handles(path))
}

pub fn entry(path: &Path) -> Option<AudioRange> {
    BANKS.get()?.entry(path)
}

pub fn encoded_size(path: &Path) -> io::Result<u64> {
    if packed_path(path) {
        return entry(path).map(|range| u64::from(range.length)).ok_or_else(missing);
    }
    Ok(std::fs::metadata(path)?.len())
}

pub fn read_encoded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    if packed_path(path) {
        return entry(path).ok_or_else(missing)?.read_all(limit);
    }
    let file = File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(over_limit());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(over_limit());
    }
    Ok(bytes)
}

fn over_limit() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "encoded audio exceeds read budget")
}

fn missing() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "audio bank member missing")
}

pub struct AudioRange {
    bank: Arc<Bank>,
    offset: u32,
    length: u32,
}

impl AudioRange {
    pub fn len(&self) -> u32 {
        self.length
    }

    pub fn read_at(&self, position: u32, out: &mut [u8]) -> io::Result<usize> {
        let remaining = self.length.checked_sub(position).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "audio offset outside bank member")
        })?;
        let count = out.len().min(remaining as usize);
        if count == 0 {
            return Ok(0);
        }
        let probe = knife_perf::Scope::new(knife_perf::Metric::AudioBankRead);
        let offset = u64::from(self.offset) + u64::from(position);
        let mut input = self.bank.input.lock().map_err(|_| io::Error::other("audio bank lock"))?;
        let previous = input.position.take();
        match previous {
            Some(previous) if previous == offset => {}
            Some(previous) => input.file.seek_relative(offset as i64 - previous as i64)?,
            None => { input.file.seek(SeekFrom::Start(offset))?; }
        }
        input.file.read_exact(&mut out[..count])?;
        input.position = Some(offset + count as u64);
        drop(input);
        probe.finish(count as u64, u64::from(previous != Some(offset)));
        Ok(count)
    }

    pub fn read_all(&self, limit: usize) -> io::Result<Vec<u8>> {
        let length = self.length as usize;
        if length > limit {
            return Err(over_limit());
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(length).map_err(io::Error::other)?;
        bytes.resize(length, 0);
        for (chunk, out) in bytes.chunks_mut(READ_CHUNK).enumerate() {
            self.read_at((chunk * READ_CHUNK) as u32, out)?;
        }
        Ok(bytes)
    }
}

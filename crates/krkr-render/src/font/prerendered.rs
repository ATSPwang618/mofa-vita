//! TVP pre-rendered font versions 0/1, read without casting packed pointers.
use super::*;
use std::io::{BufReader, Cursor, Read, Seek, SeekFrom};
use std::sync::Mutex;

const STREAM_BUFFER: usize = 4096;
trait Stream: Read + Seek + Send {}
impl<T: Read + Seek + Send> Stream for T {}

pub struct Font {
    // Full bytes for an explicitly supplied memory font; only the character
    // and item indexes for a storage font. Glyph masks use the shared LRU.
    data: Vec<u8>,
    stream: Option<Mutex<BufReader<Box<dyn Stream>>>>,
    bytes: u64,
    count: usize,
    codes: usize,
    items: usize,
    version: u8,
    _permit: Permit,
}
#[derive(Clone, Copy)]
pub struct Item {
    offset: usize,
    pub size: Size,
    origin: [i32; 2],
    advance: [i32; 2],
    pub inc: i32,
}
impl Font {
    pub fn parse(data: Vec<u8>, permit: Permit) -> Result<Self> {
        let bytes = data.len() as u64;
        let (count, codes, items, version) = Self::header(&data, bytes)?;
        Self::validate_codes(&data, codes, count)?;
        Ok(Self {
            count,
            codes,
            items,
            version,
            bytes,
            data,
            stream: None,
            _permit: permit,
        })
    }
    /// Retain the small indexes and a private seek cursor, not the font's
    /// entire compressed glyph store. Called only from the font IO worker.
    pub fn open(
        mut stream: impl Read + Seek + Send + 'static,
        bytes: u64,
        reserve: impl FnOnce(usize) -> Result<Permit>,
        stop: &AtomicBool,
    ) -> Result<Self> {
        cancelled(stop)?;
        let mut header = [0; 36];
        stream.read_exact(&mut header).map_err(io_error)?;
        let (count, codes, items, version) = Self::header(&header, bytes)?;
        let index_bytes = count * 22;
        let permit = reserve(index_bytes + STREAM_BUFFER)?;
        let mut data = vec![0; index_bytes];
        for (offset, range) in [(codes, 0..count * 2), (items, count * 2..index_bytes)] {
            cancelled(stop)?;
            stream
                .seek(SeekFrom::Start(offset as u64))
                .map_err(io_error)?;
            for chunk in data[range].chunks_mut(64 * 1024) {
                cancelled(stop)?;
                stream.read_exact(chunk).map_err(io_error)?;
            }
        }
        Self::validate_codes(&data, 0, count)?;
        Ok(Self {
            data,
            count,
            codes: 0,
            items: count * 2,
            version,
            bytes,
            stream: Some(Mutex::new(BufReader::with_capacity(
                STREAM_BUFFER,
                Box::new(stream),
            ))),
            _permit: permit,
        })
    }
    fn header(data: &[u8], bytes: u64) -> Result<(usize, usize, usize, u8)> {
        if data.len() < 36
            || &data[..22] != b"TVP pre-rendered font\x1a"
            || data[22] > 1
            || data[23] != 2
        {
            return Err(Error::Message("invalid TVP prerendered font header"));
        }
        let u32at = |i| u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
        let count = u32at(24);
        let codes = u32at(28);
        let items = u32at(32);
        if count > 65536
            || codes as u64 > bytes
            || count as u64 > bytes.saturating_sub(codes as u64) / 2
            || items as u64 > bytes
            || count as u64 > bytes.saturating_sub(items as u64) / 20
        {
            return Err(Error::Message("truncated prerendered font index"));
        }
        Ok((count, codes, items, data[22]))
    }
    fn validate_codes(data: &[u8], codes: usize, count: usize) -> Result<()> {
        let mut previous = None;
        for i in 0..count {
            let c = u16::from_le_bytes(data[codes + i * 2..codes + i * 2 + 2].try_into().unwrap());
            if previous.is_some_and(|p| p >= c) {
                return Err(Error::Message("unsorted prerendered character index"));
            }
            previous = Some(c);
        }
        Ok(())
    }
    pub fn find(&self, code: u16) -> Option<Item> {
        let mut lo = 0;
        let mut hi = self.count;
        while lo < hi {
            let mid = (lo + hi) / 2;
            let i = self.codes + mid * 2;
            let c = u16::from_le_bytes(self.data[i..i + 2].try_into().unwrap());
            if c < code {
                lo = mid + 1;
            } else if c > code {
                hi = mid;
            } else {
                let p = self.items + mid * 20;
                let word = |i| u16::from_le_bytes(self.data[p + i..p + i + 2].try_into().unwrap());
                return Some(Item {
                    offset: u32::from_le_bytes(self.data[p..p + 4].try_into().unwrap()) as usize,
                    size: Size {
                        width: word(4).into(),
                        height: word(6).into(),
                    },
                    origin: [word(8) as i16 as i32, word(10) as i16 as i32],
                    advance: [word(12) as i16 as i32, word(14) as i16 as i32],
                    inc: word(16) as i16 as i32,
                });
            }
        }
        None
    }
    pub fn glyph(&self, item: Item, baseline: [i32; 2], permit: Permit) -> Result<Glyph> {
        self.glyph_interruptible(item, baseline, permit, &AtomicBool::new(false))
    }
    pub fn glyph_interruptible(
        &self,
        item: Item,
        baseline: [i32; 2],
        permit: Permit,
        stop: &AtomicBool,
    ) -> Result<Glyph> {
        let len = item.size.width as usize * item.size.height as usize;
        let mut data = vec![0; len];
        if item.offset as u64 > self.bytes {
            return Err(Error::Message("invalid prerendered glyph offset"));
        }
        if let Some(stream) = &self.stream {
            let mut stream = stream.lock().unwrap();
            let position = stream.stream_position().map_err(io_error)?;
            // Keep nearby glyph reads in the same small buffered page.
            stream
                .seek_relative(item.offset as i64 - position as i64)
                .map_err(io_error)?;
            let reader = stream.by_ref().take(self.bytes - item.offset as u64);
            self.decode(reader, &mut data, stop)?;
        } else {
            self.decode(Cursor::new(&self.data[item.offset..]), &mut data, stop)?;
        }
        Ok(Glyph {
            id: glyph_id(),
            size: item.size,
            origin: [item.origin[0] + baseline[0], -item.origin[1] + baseline[1]],
            advance: item.advance,
            levels: 65,
            mask: Bytes::with_permit(data, permit),
        })
    }
    fn decode(&self, mut input: impl Read, data: &mut [u8], stop: &AtomicBool) -> Result<()> {
        let len = data.len();
        let mut output = 0;
        while output < len {
            if output % 4096 == 0 {
                cancelled(stop)?;
            }
            let mut value = [0];
            input.read_exact(&mut value).map_err(io_error)?;
            let value = value[0];
            let count = if self.version == 0 && value == 0x41 {
                let mut n = [0];
                input.read_exact(&mut n).map_err(io_error)?;
                Some(n[0] as usize)
            } else if self.version == 1 && value >= 0x41 {
                Some((value - 0x40) as usize)
            } else {
                None
            };
            if let Some(count) = count {
                if output == 0 || count > len - output {
                    return Err(Error::Message("invalid prerendered glyph run"));
                }
                let value = data[output - 1];
                data[output..output + count].fill(value);
                output += count;
            } else {
                if value > 64 {
                    return Err(Error::Message("invalid prerendered glyph coverage"));
                }
                data[output] = value;
                output += 1;
            }
        }
        Ok(())
    }
}
fn io_error(error: std::io::Error) -> Error {
    Error::Backend(error.to_string())
}

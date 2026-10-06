use super::{Archive, Entry, Filter};
use std::{
    io::{self, Read, Seek, SeekFrom, Take},
    sync::Arc,
};

enum SegmentStream {
    Plain(Take<crate::file::File>),
    Compressed(Box<flate2::read::ZlibDecoder<Take<crate::file::File>>>),
}
impl Read for SegmentStream {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(source) => source.read(out),
            Self::Compressed(source) => source.read(out),
        }
    }
}
struct Active {
    index: usize,
    decoded: u64,
    stream: SegmentStream,
}
pub struct Reader {
    archive: Arc<Archive>,
    entry: Arc<Entry>,
    position: u64,
    active: Option<Active>,
    filter: Option<Box<dyn Filter>>,
}
fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
impl Reader {
    pub fn new(archive: Arc<Archive>, entry: Arc<Entry>, filter: Option<Box<dyn Filter>>) -> Self {
        Self {
            archive,
            entry,
            position: 0,
            active: None,
            filter,
        }
    }
    fn segment(&mut self, index: usize) -> io::Result<&mut Active> {
        let segment = &self.entry.segments[index];
        let wanted = self.position - segment.logical;
        if self.active.as_ref().is_none_or(|active| {
            active.index != index
                || active.decoded > wanted
                || (!segment.compressed && active.decoded != wanted)
        }) {
            let mut file = crate::file::open(&self.archive.path)?;
            self.archive
                .version
                .check(&file)
                .map_err(io::Error::other)?;
            let decoded = if segment.compressed { 0 } else { wanted };
            file.seek(SeekFrom::Start(segment.offset + decoded))?;
            let source = file.take(segment.stored - decoded);
            // Rewinds still open and validate the current file. Reuse only
            // decoder storage, never stale bytes or a previous file handle.
            if segment.compressed
                && let Some(active) = &mut self.active
                && let SegmentStream::Compressed(decoder) = &mut active.stream
            {
                drop(decoder.reset(source));
                active.index = index;
                active.decoded = decoded;
            } else {
                let stream = if segment.compressed {
                    SegmentStream::Compressed(Box::new(flate2::read::ZlibDecoder::new(source)))
                } else {
                    SegmentStream::Plain(source)
                };
                self.active = Some(Active {
                    index,
                    decoded,
                    stream,
                });
            }
        }
        let active = self.active.as_mut().unwrap();
        let skip = wanted - active.decoded;
        if skip != 0 {
            let copied = io::copy(&mut active.stream.by_ref().take(skip), &mut io::sink())?;
            if copied != skip {
                return Err(invalid("truncated XP3 segment"));
            }
            active.decoded += skip;
        }
        Ok(active)
    }
}
impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.position >= self.entry.size {
            return Ok(0);
        }
        let index = self
            .active
            .as_ref()
            .filter(|active| {
                let segment = &self.entry.segments[active.index];
                self.position >= segment.logical
                    && self.position < segment.logical + segment.original
            })
            .map_or_else(
                || {
                    self.entry
                        .segments
                        .partition_point(|s| s.logical + s.original <= self.position)
                },
                |active| active.index,
            );
        let segment = &self.entry.segments[index];
        let remaining = segment.logical + segment.original - self.position;
        let count = remaining.min(out.len() as u64) as usize;
        let active = self.segment(index)?;
        let count = active.stream.read(&mut out[..count])?;
        if count == 0 {
            return Err(invalid("truncated XP3 segment"));
        }
        active.decoded += count as u64;
        if count as u64 == remaining {
            let mut extra = [0];
            if active.stream.read(&mut extra)? != 0 {
                return Err(invalid("XP3 decompressed size mismatch"));
            }
        }
        if let Some(filter) = &mut self.filter {
            filter.apply(self.position, &mut out[..count])?;
        }
        self.position += count as u64;
        Ok(count)
    }
}
impl Seek for Reader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let position = match pos {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
            SeekFrom::End(n) => i128::from(self.entry.size) + i128::from(n),
        };
        self.position = u64::try_from(position)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid XP3 seek"))?;
        Ok(self.position)
    }
}

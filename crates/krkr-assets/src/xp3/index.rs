//! Contiguous archive metadata. Names and segments are stored once; opening a
//! file materializes only that entry, not an Arc and two Vecs for every asset.
use super::{Entry, Segment};
use crate::{Error, Limits, Result, binary::Cursor};
use std::sync::Arc;

#[derive(Default)]
pub struct Entries {
    names: Box<[u16]>,
    segments: Box<[Segment]>,
    records: Box<[Record]>,
}
struct Record {
    size: u64,
    name: u32,
    segment: u32,
    segments: u32,
    hash: u32,
    order: u32,
    name_len: u16,
    protected: bool,
}
impl Record {
    fn name<'a>(&self, names: &'a [u16]) -> &'a [u16] {
        &names[self.name as usize..self.name as usize + usize::from(self.name_len)]
    }
}
impl Entries {
    pub fn len(&self) -> usize {
        self.records.len()
    }
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &[u16]> {
        self.records.iter().map(|r| r.name(&self.names))
    }
    pub fn names_from<'a>(&'a self, prefix: &[u16]) -> impl Iterator<Item = &'a [u16]> {
        let start = self
            .records
            .partition_point(|r| r.name(&self.names) < prefix);
        self.records[start..].iter().map(|r| r.name(&self.names))
    }
    pub fn get(&self, name: &[u16]) -> Option<Arc<Entry>> {
        let index = self
            .records
            .binary_search_by(|r| r.name(&self.names).cmp(name))
            .ok()?;
        Some(self.entry(&self.records[index]))
    }
    pub fn values(&self) -> impl ExactSizeIterator<Item = Arc<Entry>> + '_ {
        self.records.iter().map(|r| self.entry(r))
    }
    fn entry(&self, record: &Record) -> Arc<Entry> {
        Arc::new(Entry {
            name: record.name(&self.names).to_vec(),
            size: record.size,
            hash: record.hash,
            protected: record.protected,
            segments: self.segments
                [record.segment as usize..record.segment as usize + record.segments as usize]
                .to_vec(),
        })
    }
    pub fn retained_bytes(&self) -> usize {
        footprint(self.names.len(), self.segments.len(), self.records.len())
    }
}
fn footprint(names: usize, segments: usize, records: usize) -> usize {
    // Three backing allocations; the records have the same layout on ARM32
    // and desktop. Include allocation overhead rather than per-entry guesses.
    (size_of::<Entries>() + 3 * 16)
        .saturating_add(names.saturating_mul(2))
        .saturating_add(segments.saturating_mul(size_of::<Segment>()))
        .saturating_add(records.saturating_mul(size_of::<Record>()))
}
#[derive(Default)]
pub(super) struct Builder {
    names: Vec<u16>,
    pub segments: Vec<Segment>,
    records: Vec<Record>,
}
impl Builder {
    pub fn reserve_chunk(&mut self, data: &[u8], limits: Limits) -> Result<()> {
        let mut files = 0usize;
        let mut names = 0usize;
        let mut segments = 0usize;
        let mut chunks = Cursor(data);
        while let Some((tag, data)) = chunks.next_chunk()? {
            if &tag != b"File" {
                continue;
            }
            files += 1;
            let mut info = None;
            let mut segm = None;
            let mut fields = Cursor(data);
            while let Some((tag, data)) = fields.next_chunk()? {
                match &tag {
                    b"info" => info = Some(data),
                    b"segm" => segm = Some(data),
                    _ => {}
                }
            }
            let mut info = Cursor(info.ok_or(Error::Format("XP3 info chunk missing"))?);
            info.take(20)?;
            let n = usize::from(info.u16()?);
            info.take(n * 2)?;
            names += n;
            let table = segm.ok_or(Error::Format("XP3 segm chunk missing"))?;
            if table.len() % 28 != 0 {
                return Err(Error::Format("invalid XP3 segment table"));
            }
            segments += table.len() / 28;
        }
        if files > limits.max_entries.saturating_sub(self.records.len()) {
            return Err(Error::Limit("entry count"));
        }
        let totals = (
            self.names.len().checked_add(names),
            self.segments.len().checked_add(segments),
            self.records.len().checked_add(files),
        );
        let (Some(n), Some(s), Some(r)) = totals else {
            return Err(Error::Limit("retained index"));
        };
        // Also bound packed offsets and all size arithmetic before allocation.
        if n > u32::MAX as usize
            || s > u32::MAX as usize
            || r > u32::MAX as usize
            || n > limits.max_index_bytes / 2
            || s > limits.max_index_bytes / size_of::<Segment>()
            || r > limits.max_index_bytes / size_of::<Record>()
            || footprint(n, s, r) > limits.max_index_bytes
        {
            return Err(Error::Limit("retained index"));
        }
        // Exact growth avoids a power-of-two capacity doubling for large XP3s.
        self.names
            .try_reserve_exact(names)
            .map_err(|_| Error::Limit("retained index"))?;
        self.segments
            .try_reserve_exact(segments)
            .map_err(|_| Error::Limit("retained index"))?;
        self.records
            .try_reserve_exact(files)
            .map_err(|_| Error::Limit("retained index"))?;
        Ok(())
    }
    pub fn push(
        &mut self,
        name: &[u16],
        size: u64,
        hash: u32,
        protected: bool,
        segment: usize,
    ) -> Result<()> {
        self.records.push(Record {
            size,
            name: self.names.len() as u32,
            name_len: u16::try_from(name.len()).map_err(|_| Error::Limit("entry name"))?,
            segment: segment as u32,
            segments: (self.segments.len() - segment) as u32,
            hash,
            protected,
            order: self.records.len() as u32,
        });
        self.names.extend_from_slice(name);
        Ok(())
    }
    pub fn finish(mut self, strict: bool) -> Result<Entries> {
        // The ordinal preserves the first definition even across chained
        // indexes, without allocating the temporary buffer of a stable sort.
        self.records.sort_unstable_by(|a, b| {
            a.name(&self.names)
                .cmp(b.name(&self.names))
                .then(a.order.cmp(&b.order))
        });
        if strict
            && self
                .records
                .windows(2)
                .any(|r| r[0].name(&self.names) == r[1].name(&self.names))
        {
            return Err(Error::Name("duplicate normalized XP3 entry"));
        }
        self.records
            .dedup_by(|a, b| a.name(&self.names) == b.name(&self.names));
        Ok(Entries {
            names: self.names.into_boxed_slice(),
            segments: self.segments.into_boxed_slice(),
            records: self.records.into_boxed_slice(),
        })
    }
}

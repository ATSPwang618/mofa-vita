//! Source ranges are UTF-16 code units, never UTF-8 byte offsets.

use std::ops::Range;

use slotmap::{SlotMap, new_key_type};

pub const MAX_SOURCE_UNITS: usize = 1_048_576;

new_key_type! { struct SourceKey; }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(SourceKey);

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Utf16Offset(u32);

impl Utf16Offset {
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Construct ranges through SourceMap so source identity and bounds are checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    source: SourceId,
    start: Utf16Offset,
    end: Utf16Offset,
}

impl Span {
    pub const fn source(self) -> SourceId {
        self.source
    }

    pub const fn start(self) -> Utf16Offset {
        self.start
    }

    pub const fn end(self) -> Utf16Offset {
        self.end
    }

    pub fn range(self) -> Range<usize> {
        self.start.0 as usize..self.end.0 as usize
    }

    pub fn join(self, other: Self) -> Option<Self> {
        (self.source == other.source).then(|| Self {
            source: self.source,
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        })
    }
}

#[derive(Debug)]
pub struct SourceFile {
    name: String,
    units: Vec<u16>,
    line_starts: Vec<usize>,
    line_offset: i32,
}

impl SourceFile {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn units(&self) -> &[u16] {
        &self.units
    }

    pub fn line_offset(&self) -> i32 {
        self.line_offset
    }

    /// Owned source/debug allocations, including unused vector capacity.
    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.name.capacity()
            + self.units.capacity() * std::mem::size_of::<u16>()
            + self.line_starts.capacity() * std::mem::size_of::<usize>()
    }

    /// Returns one-based line and UTF-16 column, including the EOF position.
    pub fn line_column(&self, offset: Utf16Offset) -> Option<(i64, usize)> {
        let position = offset.0 as usize;
        if position > self.units.len() {
            return None;
        }
        let line = self.line_starts.partition_point(|&start| start <= position) - 1;
        Some((
            line as i64 + 1 + i64::from(self.line_offset),
            position - self.line_starts[line] + 1,
        ))
    }

    pub fn line_range(&self, line: usize) -> Option<Range<usize>> {
        let start = *self.line_starts.get(line.checked_sub(1)?)?;
        let mut end = self
            .line_starts
            .get(line)
            .copied()
            .unwrap_or(self.units.len());
        while end > start && matches!(self.units[end - 1], 10 | 13) {
            end -= 1;
        }
        Some(start..end)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("source has {units} UTF-16 units; limit is {limit}")]
    TooLarge { units: usize, limit: usize },
    #[error("source handle is no longer valid")]
    UnknownSource,
    #[error("source range is out of bounds or reversed")]
    InvalidRange,
}

#[derive(Default)]
pub struct SourceMap {
    files: SlotMap<SourceKey, SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_utf8(
        &mut self,
        name: impl Into<String>,
        text: &str,
    ) -> Result<SourceId, SourceError> {
        // Count before allocation, so a rejected source does not allocate a large UTF-16 copy.
        let count = text.encode_utf16().count();
        Self::check_size(count)?;
        self.add_utf16(name, text.encode_utf16().collect())
    }

    pub fn add_utf16(
        &mut self,
        name: impl Into<String>,
        units: Vec<u16>,
    ) -> Result<SourceId, SourceError> {
        Self::check_size(units.len())?;
        let mut line_starts = vec![0];
        let mut i = 0;
        while i < units.len() {
            match units[i] {
                13 => {
                    i += 1;
                    if units.get(i) == Some(&10) {
                        i += 1;
                    }
                    line_starts.push(i);
                }
                10 => {
                    i += 1;
                    line_starts.push(i);
                }
                _ => i += 1,
            }
        }
        Ok(SourceId(self.files.insert(SourceFile {
            name: name.into(),
            units,
            line_starts,
            line_offset: 0,
        })))
    }

    pub fn set_line_offset(&mut self, source: SourceId, offset: i32) -> Result<(), SourceError> {
        self.files
            .get_mut(source.0)
            .ok_or(SourceError::UnknownSource)?
            .line_offset = offset;
        Ok(())
    }

    fn check_size(units: usize) -> Result<(), SourceError> {
        if units > MAX_SOURCE_UNITS {
            Err(SourceError::TooLarge {
                units,
                limit: MAX_SOURCE_UNITS,
            })
        } else {
            Ok(())
        }
    }

    pub fn get(&self, id: SourceId) -> Option<&SourceFile> {
        self.files.get(id.0)
    }

    pub fn remove(&mut self, id: SourceId) -> Option<SourceFile> {
        self.files.remove(id.0)
    }

    pub fn span(&self, source: SourceId, range: Range<usize>) -> Result<Span, SourceError> {
        let file = self.get(source).ok_or(SourceError::UnknownSource)?;
        if range.start > range.end || range.end > file.units.len() {
            return Err(SourceError::InvalidRange);
        }
        Ok(Span {
            source,
            start: Utf16Offset(range.start as u32),
            end: Utf16Offset(range.end as u32),
        })
    }

    pub fn slice(&self, span: Span) -> Option<&[u16]> {
        self.get(span.source)?.units.get(span.range())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_positions_and_crlf_preserve_raw_units() {
        let mut sources = SourceMap::new();
        let id = sources.add_utf8("sample", "中😀\r\nx\ry\n").unwrap();
        let file = sources.get(id).unwrap();
        assert_eq!(file.units().len(), 9);
        for (offset, expected) in [
            (0, (1, 1)),
            (3, (1, 4)),
            (5, (2, 1)),
            (7, (3, 1)),
            (9, (4, 1)),
        ] {
            let span = sources.span(id, offset..offset).unwrap();
            assert_eq!(file.line_column(span.start()), Some(expected));
        }
        assert_eq!(file.line_range(1), Some(0..3));
        let lone = sources.add_utf16("lone", vec![0xd800, 0, 0xdc00]).unwrap();
        assert_eq!(sources.get(lone).unwrap().units(), &[0xd800, 0, 0xdc00]);
    }

    #[test]
    fn removed_handles_and_invalid_ranges_fail() {
        let mut sources = SourceMap::new();
        let old = sources.add_utf8("old", "a").unwrap();
        let span = sources.span(old, 0..1).unwrap();
        sources.remove(old);
        let new = sources.add_utf8("new", "b").unwrap();
        assert_ne!(old, new);
        assert!(sources.get(old).is_none());
        assert!(sources.slice(span).is_none());
        assert!(sources.span(new, 0..2).is_err());
        assert!(sources.span(new, Range { start: 1, end: 0 }).is_err());
        let eof = sources.span(new, 1..1).unwrap();
        assert_eq!(sources.slice(eof), Some(&[][..]));
    }
}

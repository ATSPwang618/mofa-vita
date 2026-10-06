//! Primitive string work owns only IDs, cursors and its output. No source
//! buffer is cloned or borrowed across a VM budget boundary.
use super::escape_unit;
use crate::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, StrId, Trace, Value,
};
use smallvec::SmallVec;

const WORK: usize = 1024;

pub(super) enum Operation {
    Copy { end: usize, stop_at_nul: bool },
    Case { upper: bool },
    Trim { end: usize, trailing: bool },
    Reverse { visible: Option<usize> },
    Repeat { total: usize },
    Escape { hex: bool },
}
pub(super) struct Text {
    source: StrId,
    position: usize,
    output: Vec<u16>,
    operation: Operation,
}
impl Trace for Text {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.source.trace(visit);
    }
}
impl NativeContinuation for Text {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx.heap_mut())
    }
}
impl Text {
    pub fn start(
        heap: &mut Heap,
        source: StrId,
        position: usize,
        operation: Operation,
    ) -> NativeResult<NativeStep> {
        Self {
            source,
            position,
            output: Vec::new(),
            operation,
        }
        .advance(heap)
    }
    fn finish(self, heap: &mut Heap) -> NativeStep {
        NativeStep::Return(Value::Str(heap.alloc_string(self.output)))
    }
    fn advance(mut self, heap: &mut Heap) -> NativeResult<NativeStep> {
        let source = heap.string(self.source)?;
        match self.operation {
            Operation::Trim { mut end, trailing } => {
                let mut work = 0;
                if trailing {
                    while end > self.position && (1..=32).contains(&source[end - 1]) && work < WORK
                    {
                        end -= 1;
                        work += 1;
                    }
                    if end > self.position && (1..=32).contains(&source[end - 1]) {
                        self.operation = Operation::Trim {
                            end,
                            trailing: true,
                        };
                        return Ok(NativeStep::Continue(Box::new(self)));
                    }
                }
                while self.position < end
                    && (1..=32).contains(&source[self.position])
                    && work < WORK
                {
                    self.position += 1;
                    work += 1;
                }
                if self.position < end && (1..=32).contains(&source[self.position]) {
                    self.operation = Operation::Trim {
                        end,
                        trailing: false,
                    };
                } else {
                    if super::slice_string(&source[self.position..end]).is_empty() {
                        return Ok(self.finish(heap));
                    }
                    if self.position == 0 && end == source.len() {
                        return Ok(NativeStep::Return(Value::Str(self.source)));
                    }
                    self.operation = Operation::Copy {
                        end,
                        stop_at_nul: false,
                    };
                    // Reuse this budget when the trim itself was short.
                    if work == 0 {
                        return self.advance(heap);
                    }
                }
            }
            Operation::Copy { end, stop_at_nul } => {
                if !stop_at_nul && self.position == 0 && end == source.len() {
                    return Ok(NativeStep::Return(Value::Str(self.source)));
                }
                let end_of_batch = self.position.saturating_add(WORK).min(end);
                let mut part = &source[self.position..end_of_batch];
                if stop_at_nul {
                    part = super::c_string(part);
                }
                reserve(&mut self.output, part.len())?;
                self.output.extend_from_slice(part);
                self.position += part.len();
                if self.position == end || self.position < end_of_batch {
                    return Ok(self.finish(heap));
                }
            }
            Operation::Case { upper } => {
                let end = self.position.saturating_add(WORK).min(source.len());
                let part = super::c_string(&source[self.position..end]);
                // Common short resource names already have the requested case.
                // Keep long conversions sliced instead of adding an unbounded scan.
                if self.position == 0
                    && part.len() == source.len()
                    && !part.iter().any(|&unit| {
                        if upper {
                            (97..=122).contains(&unit)
                        } else {
                            (65..=90).contains(&unit)
                        }
                    })
                {
                    return Ok(NativeStep::Return(Value::Str(self.source)));
                }
                reserve(&mut self.output, part.len())?;
                self.output.extend(part.iter().map(|&unit| match unit {
                    97..=122 if upper => unit - 32,
                    65..=90 if !upper => unit + 32,
                    _ => unit,
                }));
                self.position += part.len();
                if self.position == source.len() || self.position < end {
                    return Ok(self.finish(heap));
                }
            }
            Operation::Reverse { visible: None } => {
                let end = self.position.saturating_add(WORK).min(source.len());
                let prefix = super::c_string(&source[self.position..end]);
                self.position += prefix.len();
                if self.position < end || end == source.len() {
                    // The C-string copy establishes the result's logical length.
                    let visible = self.position;
                    self.position = 0;
                    self.operation = Operation::Reverse {
                        visible: Some(visible),
                    };
                    return self.advance(heap);
                }
            }
            Operation::Reverse {
                visible: Some(visible),
            } => {
                let end = self.position.saturating_add(WORK).min(visible);
                reserve(&mut self.output, end - self.position)?;
                self.output.extend(
                    source[source.len() - end..source.len() - self.position]
                        .iter()
                        .rev()
                        .copied(),
                );
                self.position = end;
                if end == visible {
                    return Ok(self.finish(heap));
                }
            }
            Operation::Repeat { total } => {
                let end = self.position.saturating_add(WORK).min(total);
                reserve(&mut self.output, end - self.position)?;
                if self.position < source.len() {
                    let seed_end = end.min(source.len());
                    self.output
                        .extend_from_slice(&source[self.position..seed_end]);
                    self.position = seed_end;
                }
                while self.position < end {
                    let offset = self.position % source.len();
                    // The emitted prefix already contains whole repetitions.
                    // Copy an aligned span, doubling it within this work slice.
                    let length = (self.position - offset).min(end - self.position);
                    self.output.extend_from_within(offset..offset + length);
                    self.position += length;
                }
                if self.position == total {
                    return Ok(self.finish(heap));
                }
            }
            Operation::Escape { mut hex } => {
                let end = self.position.saturating_add(WORK).min(source.len());
                let part = super::c_string(&source[self.position..end]);
                if self.position == 0
                    && part.len() == source.len()
                    && !part
                        .iter()
                        .any(|&unit| unit < 32 || matches!(unit, 34 | 39 | 92))
                {
                    return Ok(NativeStep::Return(Value::Str(self.source)));
                }
                reserve(&mut self.output, part.len() * 4)?;
                for &unit in part {
                    escape_unit(&mut self.output, &mut hex, unit);
                }
                self.position += part.len();
                if self.position == source.len() || self.position < end {
                    return Ok(self.finish(heap));
                }
                self.operation = Operation::Escape { hex };
            }
        }
        Ok(NativeStep::Continue(Box::new(self)))
    }
}

fn reserve<T>(output: &mut Vec<T>, additional: usize) -> NativeResult<()> {
    output
        .try_reserve(additional)
        .map_err(|_| NativeError::Message("string work allocation failed"))
}

// Delimiters in scenario and menu scripts are often a single UTF-16 unit.
// They need no prefix table, but long searches must still yield to the VM.
struct UnitFind {
    source: StrId,
    unit: u16,
    position: usize,
}
impl Trace for UnitFind {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.source.trace(visit);
    }
}
impl NativeContinuation for UnitFind {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx.heap_mut())
    }
}
impl UnitFind {
    fn advance(mut self, heap: &mut Heap) -> NativeResult<NativeStep> {
        let source = heap.string(self.source)?;
        let count = (source.len() - self.position).min(WORK);
        let end = self.position + count;
        if let Some(offset) = source[self.position..end]
            .iter()
            .position(|&unit| unit == self.unit || unit == 0)
        {
            let index = self.position + offset;
            return Ok(Find::result((source[index] == self.unit).then_some(index)));
        }
        if end == source.len() {
            return Ok(Find::result(None));
        }
        self.position = end;
        Ok(NativeStep::Continue(Box::new(self)))
    }
}

/// KMP preprocessing and scanning both consume work units, including fallback
/// comparisons. Repeated-prefix needles no longer make indexOf quadratic.
pub(super) struct Find {
    source: StrId,
    needle: StrId,
    prefix: SmallVec<[usize; 16]>,
    position: usize,
    matched: usize,
    building: bool,
}
impl Trace for Find {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.source.trace(visit);
        self.needle.trace(visit);
    }
}
impl NativeContinuation for Find {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx.heap_mut())
    }
}
impl Find {
    pub fn start(
        heap: &mut Heap,
        source: StrId,
        needle: StrId,
        position: usize,
    ) -> NativeResult<NativeStep> {
        let units = heap.string(needle)?;
        if units[0] != 0 && (units.len() == 1 || units[1] == 0) {
            return UnitFind {
                source,
                unit: units[0],
                position,
            }
            .advance(heap);
        }
        Self {
            source,
            needle,
            position,
            prefix: SmallVec::new(),
            matched: 0,
            building: true,
        }
        .advance(heap)
    }
    fn result(index: Option<usize>) -> NativeStep {
        NativeStep::Return(Value::Int(index.map_or(-1, |index| index as i64)))
    }
    fn advance(mut self, heap: &mut Heap) -> NativeResult<NativeStep> {
        let source = heap.string(self.source)?;
        let needle = heap.string(self.needle)?;
        for _ in 0..WORK {
            if self.building {
                let index = self.prefix.len();
                if index == needle.len() || needle[index] == 0 {
                    self.building = false;
                    self.matched = 0;
                    if index == 0 {
                        return Ok(Self::result(Some(self.position)));
                    }
                } else if index == 0 {
                    self.reserve_prefix()?;
                    self.prefix.push(0);
                } else if needle[index] == needle[self.matched] {
                    self.matched += 1;
                    self.reserve_prefix()?;
                    self.prefix.push(self.matched);
                } else if self.matched > 0 {
                    self.matched = self.prefix[self.matched - 1];
                } else {
                    self.reserve_prefix()?;
                    self.prefix.push(0);
                }
                continue;
            }
            if self.position == source.len() || source[self.position] == 0 {
                return Ok(Self::result(None));
            }
            if source[self.position] == needle[self.matched] {
                self.position += 1;
                self.matched += 1;
                if self.matched == self.prefix.len() {
                    return Ok(Self::result(Some(self.position - self.matched)));
                }
            } else if self.matched > 0 {
                self.matched = self.prefix[self.matched - 1];
            } else {
                self.position += 1;
            }
        }
        Ok(NativeStep::Continue(Box::new(self)))
    }
    fn reserve_prefix(&mut self) -> NativeResult<()> {
        self.prefix
            .try_reserve(1)
            .map_err(|_| NativeError::Message("string work allocation failed"))
    }
}

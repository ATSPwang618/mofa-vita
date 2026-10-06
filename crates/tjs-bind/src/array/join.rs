//! Join retains its live input and publishes a result only after every value
//! conversion. Long fields and large arrays both yield under the VM budget.
use crate::{
    NativeContinuation, NativeCx, NativeResult, NativeStep, ObjId, RestArgs, Trace, Value,
};
use tjs_core::{StrId, string, value};

struct Join {
    source: ObjId,
    delimiter: StrId,
    empty_delimiter: bool,
    index: usize,
    first: bool,
    purge: bool,
    item: Option<Value>,
    part: Option<StrId>,
    position: usize,
    output: Vec<u16>,
}
impl Trace for Join {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.source.trace(visit);
        self.delimiter.trace(visit);
        if let Some(value) = self.item {
            visit(value);
        }
        if let Some(part) = self.part {
            part.trace(visit);
        }
    }
}
pub(super) fn start(
    cx: &mut NativeCx<'_>,
    delimiter: Value,
    args: RestArgs<'_>,
) -> NativeResult<NativeStep> {
    let Value::Str(delimiter) = value::to_string(cx.heap_mut(), delimiter)? else {
        unreachable!()
    };
    let purge = args
        .get(1)
        .copied()
        .unwrap_or(Value::Void)
        .truthy(cx.heap())?;
    let empty_delimiter = cx
        .heap()
        .string(delimiter)?
        .first()
        .is_none_or(|&unit| unit == 0);
    Join {
        source: cx.this(),
        delimiter,
        empty_delimiter,
        index: 0,
        first: true,
        purge,
        item: None,
        part: None,
        position: 0,
        output: Vec::new(),
    }
    .advance(cx)
}
impl NativeContinuation for Join {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx)
    }
}
impl Join {
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let mut units = 0;
        for _ in 0..128 {
            if let Some(part) = self.part {
                let text = cx.heap().string(part)?;
                let end = self.position.saturating_add(1024 - units).min(text.len());
                let piece = string::c_string(&text[self.position..end]);
                self.output
                    .try_reserve(piece.len())
                    .map_err(|_| crate::NativeError::Message("join allocation failed"))?;
                self.output.extend_from_slice(piece);
                self.position += piece.len();
                units += piece.len();
                if self.position == text.len() || self.position < end {
                    self.part = None;
                    self.position = 0;
                }
                if units == 1024 {
                    break;
                }
            } else if let Some(item) = self.item.take() {
                // Every signed integer fits in 20 UTF-16 units. Keep the
                // normal slice boundary and avoid a temporary GC string.
                if matches!(item, Value::Int(_)) && 1024 - units >= 20 {
                    self.output
                        .try_reserve(20)
                        .map_err(|_| crate::NativeError::Message("join allocation failed"))?;
                    let start = self.output.len();
                    value::append_string_units(cx.heap(), item, &mut self.output)?;
                    units += self.output.len() - start;
                    if units == 1024 {
                        break;
                    }
                    continue;
                }
                let Value::Str(part) = value::to_string(cx.heap_mut(), item)? else {
                    unreachable!()
                };
                self.part = Some(part);
            } else {
                let values = cx.heap().array(self.source)?;
                if self.index >= values.len() {
                    return Ok(NativeStep::Return(if cx.result_needed() {
                        Value::Str(cx.heap_mut().alloc_string(self.output))
                    } else {
                        Value::Void
                    }));
                }
                let item = values[self.index];
                self.index += 1;
                if self.purge && matches!(item, Value::Void) {
                    continue;
                }
                self.item = Some(item);
                if !self.first && !self.empty_delimiter {
                    self.part = Some(self.delimiter);
                }
                self.first = false;
            }
        }
        Ok(NativeStep::Continue(Box::new(self)))
    }
}

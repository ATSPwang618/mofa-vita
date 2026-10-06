//! Bounded scanning without a duplicate element buffer.
use crate::{
    NativeContinuation, NativeCx, NativeResult, NativeStep, ObjId, RestArgs, Trace, Value,
};
use tjs_core::value;

pub(super) fn find(
    cx: &mut NativeCx<'_>,
    value: Value,
    args: RestArgs<'_>,
) -> NativeResult<NativeStep> {
    if !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    let start = value::to_integer(cx.heap(), args.first().copied().unwrap_or(Value::Void))? as i32;
    let length = cx.heap().array(cx.this())?.len();
    let index = if start < 0 {
        (length as i64 + i64::from(start)).max(0) as usize
    } else {
        start as usize
    };
    Search {
        target: cx.this(),
        value,
        index,
        remove: None,
    }
    .advance(cx)
}

pub(super) fn remove(
    cx: &mut NativeCx<'_>,
    value: Value,
    args: RestArgs<'_>,
) -> NativeResult<NativeStep> {
    let all = args
        .first()
        .copied()
        .unwrap_or(Value::Int(1))
        .truthy(cx.heap())?;
    Search {
        target: cx.this(),
        value,
        index: 0,
        remove: Some((all, Vec::new())),
    }
    .advance(cx)
}

struct Search {
    target: ObjId,
    value: Value,
    index: usize,
    remove: Option<(bool, Vec<usize>)>,
}
impl Trace for Search {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.target.trace(visit);
        self.value.trace(visit);
    }
}
impl NativeContinuation for Search {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.advance(cx)
    }
}
impl Search {
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let values = cx.heap().array(self.target)?;
        let end = self.index.saturating_add(128).min(values.len());
        let mut done = false;
        while self.index < end {
            let index = self.index;
            self.index += 1;
            if value::strict_equal(cx.heap(), self.value, values[index])? {
                let Some((all, indices)) = &mut self.remove else {
                    return Ok(NativeStep::Return(Value::Int(index as i64)));
                };
                indices.push(index);
                if !*all {
                    done = true;
                    break;
                }
            }
        }
        if !done && self.index < values.len() {
            return Ok(NativeStep::Continue(Box::new(self)));
        }
        let result = if let Some((_, indices)) = self.remove {
            // Comparison completes before the first mutation. A thrown error
            // or cancellation during scanning leaves the original array intact.
            cx.heap_mut().array_remove_indices(self.target, &indices)?;
            indices.len() as i64
        } else {
            -1
        };
        Ok(NativeStep::Return(Value::Int(result)))
    }
}

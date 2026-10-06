//! Standard Rust sorting for ordered primitive keys; resumable merge sorting
//! for script callbacks and TJS mixed/NaN comparisons without a total order.
use crate::{
    NativeContinuation, NativeCx, NativeResult, NativeStep, ObjId, RestArgs, Trace, Value,
};
use tjs_core::value;

pub(super) fn start(cx: &mut NativeCx<'_>, args: RestArgs<'_>) -> NativeResult<NativeStep> {
    let mode = args.first().copied().unwrap_or(Value::Void);
    let compare = if matches!(mode, Value::Obj(_)) {
        Some(mode)
    } else {
        None
    };
    let mode = if compare.is_some() || matches!(mode, Value::Void) {
        b'+'
    } else {
        tjs_core::string::units(cx.heap_mut(), mode)?
            .first()
            .copied()
            .unwrap_or(0) as u8
    };
    let stable = args
        .get(1)
        .copied()
        .unwrap_or(Value::Void)
        .truthy(cx.heap())?;
    let mut values = cx.heap().array(cx.this())?.to_vec();
    if compare.is_none() && standard_sort(cx, &mut values, mode, stable)? {
        let this = cx.this();
        cx.heap_mut().array_replace(this, values)?;
        return Ok(NativeStep::Return(Value::Void));
    }
    let length = values.len();
    Sort {
        target: cx.this(),
        values,
        buffer: vec![Value::Void; length],
        width: 1,
        start: 0,
        left: 0,
        right: 1.min(length),
        output: 0,
        compare,
        mode,
    }
    .advance(cx)
}

struct Sort {
    target: ObjId,
    values: Vec<Value>,
    buffer: Vec<Value>,
    width: usize,
    start: usize,
    left: usize,
    right: usize,
    output: usize,
    compare: Option<Value>,
    mode: u8,
}
impl Trace for Sort {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.target.trace(visit);
        self.values.trace(visit);
        self.buffer.trace(visit);
        self.compare.trace(visit);
    }
}
impl Sort {
    fn take(&mut self, right: bool) {
        let index = if right {
            &mut self.right
        } else {
            &mut self.left
        };
        self.buffer[self.output] = self.values[*index];
        *index += 1;
        self.output += 1;
    }
    fn advance(mut self, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        let length = self.values.len();
        while self.width < length {
            let middle = (self.start + self.width).min(length);
            let end = (middle + self.width).min(length);
            if self.output == end {
                self.start = end;
                if self.start == length {
                    std::mem::swap(&mut self.values, &mut self.buffer);
                    self.width *= 2;
                    self.start = 0;
                }
                self.left = self.start;
                self.right = (self.start + self.width).min(length);
                self.output = self.start;
                continue;
            }
            if self.left == middle {
                self.take(true);
                continue;
            }
            if self.right == end {
                self.take(false);
                continue;
            }
            // Right < left keeps equivalent elements in their original order.
            let (mut left, mut right) = (self.values[self.right], self.values[self.left]);
            if let Some(function) = self.compare {
                return Ok(NativeStep::Call {
                    function,
                    arguments: vec![left, right],
                    continuation: Box::new(self),
                });
            }
            if matches!(self.mode, b'0' | b'9')
                && matches!((left, right), (Value::Str(_), Value::Str(_)))
            {
                left = value::to_number(cx.heap(), left)?;
                right = value::to_number(cx.heap(), right)?;
            } else if matches!(self.mode, b'a' | b'z') {
                left = value::to_string(cx.heap_mut(), left)?;
                right = value::to_string(cx.heap_mut(), right)?;
            }
            let order = value::compare_in(cx.heap(), left, right)?;
            let less = if matches!(self.mode, b'-' | b'9' | b'z') {
                order.is_some_and(|o| o.is_gt())
            } else {
                order.is_some_and(|o| o.is_lt())
            };
            self.take(less);
        }
        cx.heap_mut().array_replace(self.target, self.values)?;
        Ok(NativeStep::Return(Value::Void))
    }
}
impl NativeContinuation for Sort {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        self.take(result.truthy(cx.heap())?);
        self.advance(cx)
    }
}
fn standard_sort(
    cx: &mut NativeCx<'_>,
    values: &mut [Value],
    mode: u8,
    stable: bool,
) -> NativeResult<bool> {
    if values.len() < 2 {
        return Ok(true);
    }
    let strings = values.iter().all(|v| matches!(v, Value::Str(_)));
    let converted = matches!(mode, b'a' | b'z') || matches!(mode, b'0' | b'9') && strings;
    if !converted {
        if !ordered(cx.heap(), values.iter().copied())? {
            return Ok(false);
        }
        let compare = |left: &Value, right: &Value| comparison(cx.heap(), *left, *right, mode);
        if stable {
            values.sort_by(compare);
        } else {
            values.sort_unstable_by(compare);
        }
        return Ok(true);
    }
    let mut keyed = Vec::with_capacity(values.len());
    for &original in values.iter() {
        let key = match mode {
            b'a' | b'z' => value::to_string(cx.heap_mut(), original)?,
            b'0' | b'9' if strings => value::to_number(cx.heap(), original)?,
            _ => original,
        };
        keyed.push((original, key));
    }
    if !ordered(cx.heap(), keyed.iter().map(|&(_, key)| key))? {
        return Ok(false);
    }
    let compare = |left: &(Value, Value), right: &(Value, Value)| {
        comparison(cx.heap(), left.1, right.1, mode)
    };
    if stable {
        keyed.sort_by(compare);
    } else {
        keyed.sort_unstable_by(compare);
    }
    for (slot, (original, _)) in values.iter_mut().zip(keyed) {
        *slot = original;
    }
    Ok(true)
}

fn ordered(heap: &crate::Heap, values: impl Iterator<Item = Value>) -> NativeResult<bool> {
    let mut first = None;
    for value in values {
        let first = *first.get_or_insert(value);
        if let Value::Str(id) = value {
            heap.string(id)?;
        }
        if !matches!(
            (first, value),
            (Value::Int(_), Value::Int(_)) | (Value::Str(_), Value::Str(_))
        ) && !matches!((first, value),
                (Value::Real(a), Value::Real(b)) if !a.is_nan() && !b.is_nan())
        {
            return Ok(false);
        }
    }
    Ok(true)
}
fn comparison(heap: &crate::Heap, left: Value, right: Value, mode: u8) -> std::cmp::Ordering {
    let order = value::compare_in(heap, left, right)
        .expect("validated keys")
        .expect("ordered keys");
    if matches!(mode, b'-' | b'9' | b'z') {
        order.reverse()
    } else {
        order
    }
}

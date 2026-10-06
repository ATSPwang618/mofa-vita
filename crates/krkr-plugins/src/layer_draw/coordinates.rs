//! Ordered, resumable PointF/RectF conversion. Getters execute on the caller's
//! VM and every retained object remains visible to the collector.
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, Trace, Value, value,
};

pub fn read<S: Trace + 'static>(
    cx: &mut NativeCx<'_>,
    input: Value,
    fields: &'static [&'static str],
    state: S,
    next: fn(S, &mut NativeCx<'_>, Vec<Vec<f64>>) -> NativeResult<NativeStep>,
) -> NativeResult<NativeStep> {
    if !matches!(input, Value::Obj(r) if r.object.is_some()) {
        return next(state, cx, Vec::new());
    }
    let key = Value::Str(
        cx.heap_mut()
            .alloc_string("count".encode_utf16().collect::<Vec<_>>()),
    );
    let missing = cx.heap_mut().alloc_dictionary();
    let read = Read {
        input,
        fields,
        state,
        next,
        output: Vec::new(),
        count: 0,
        index: 0,
        item: Value::Void,
        component: 0,
        current: Vec::new(),
        stage: Stage::Count,
        missing,
    };
    Ok(NativeStep::GetOr {
        object: input,
        key,
        raw: false,
        fallback: Value::Int(0),
        continuation: Box::new(read),
    })
}
enum Stage {
    Count,
    Item,
    Field,
}
pub fn one<S: Trace + 'static>(
    cx: &mut NativeCx<'_>,
    input: Value,
    fields: &'static [&'static str],
    state: S,
    next: fn(S, &mut NativeCx<'_>, Vec<Vec<f64>>) -> NativeResult<NativeStep>,
) -> NativeResult<NativeStep> {
    if let Some(values) = super::geometry::native(cx, input, fields) {
        return next(state, cx, vec![values]);
    }
    if !matches!(input, Value::Obj(r) if r.object.is_some()) {
        return next(state, cx, vec![vec![0.; fields.len()]]);
    }
    let missing = cx.heap_mut().alloc_dictionary();
    Box::new(Read {
        input: Value::Void,
        fields,
        state,
        next,
        output: Vec::new(),
        count: 1,
        index: 1,
        item: input,
        component: 0,
        current: Vec::new(),
        stage: Stage::Field,
        missing,
    })
    .field(cx)
}
struct Read<S> {
    input: Value,
    fields: &'static [&'static str],
    state: S,
    next: fn(S, &mut NativeCx<'_>, Vec<Vec<f64>>) -> NativeResult<NativeStep>,
    output: Vec<Vec<f64>>,
    count: i32,
    index: i32,
    item: Value,
    component: usize,
    current: Vec<f64>,
    stage: Stage,
    missing: tjs_core::ObjId,
}
impl<S: Trace> Trace for Read<S> {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.input.trace(visit);
        self.item.trace(visit);
        self.missing.trace(visit);
        self.state.trace(visit);
    }
}
impl<S: Trace + 'static> Read<S> {
    fn item(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.index >= self.count {
            return (self.next)(self.state, cx, self.output);
        }
        let key = Value::Int(i64::from(self.index));
        self.index += 1;
        self.stage = Stage::Item;
        Ok(NativeStep::GetOr {
            object: self.input,
            key,
            raw: false,
            fallback: Value::Obj(self.missing.into()),
            continuation: self,
        })
    }
    fn field(mut self: Box<Self>, cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
        if self.component == self.fields.len() {
            self.output.push(std::mem::take(&mut self.current));
            return self.item(cx);
        }
        let array = matches!(self.item, Value::Obj(r) if r.object.is_some_and(|id| cx.heap().array(id).is_ok()));
        let key = if array {
            Value::Int(self.component as i64)
        } else {
            Value::Str(
                cx.heap_mut().alloc_string(
                    self.fields[self.component]
                        .encode_utf16()
                        .collect::<Vec<_>>(),
                ),
            )
        };
        self.stage = Stage::Field;
        Ok(NativeStep::GetOr {
            object: self.item,
            key,
            raw: false,
            fallback: Value::Int(0),
            continuation: self,
        })
    }
}
impl<S: Trace + 'static> NativeContinuation for Read<S> {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        match self.stage {
            Stage::Count => {
                self.count = value::to_integer(cx.heap(), value)? as i32;
                if self.count > 1_000_000 {
                    return Err(NativeError::Message(
                        "vector coordinate count exceeds budget",
                    ));
                }
                self.item(cx)
            }
            Stage::Item => {
                if matches!(value, Value::Obj(r) if r.object == Some(self.missing)) {
                    return self.item(cx);
                }
                if !matches!(value, Value::Obj(r) if r.object.is_some()) {
                    self.output.push(vec![0.; self.fields.len()]);
                    return self.item(cx);
                }
                self.item = value;
                if let Some(values) = super::geometry::native(cx, value, self.fields) {
                    self.output.push(values);
                    return self.item(cx);
                }
                self.component = 0;
                self.current.clear();
                self.field(cx)
            }
            Stage::Field => {
                self.current.push(value::to_real(cx.heap(), value)?);
                self.component += 1;
                self.field(cx)
            }
        }
    }
}

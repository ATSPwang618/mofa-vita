//! stringUtil compatibility from the supplied Mahoyo scripts/old patch.
//! No original DLL is available: these are its three documented call sites,
//! not a claim that every export or malformed-input behavior was recovered.
use crate::exports::object;
use tjs_bind::flow;
use tjs_core::{
    Heap, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef, StrId, Value, value,
};

krkr_engine::native_plugin! {
    pub(crate) StringUtil {
        names: ["stringUtil.dll", "stringUtil.tpm"],
        link(cx, exports) {
            exports.function(cx, cx.global, "isNumber", is_number::CALL)?;
            exports.function(cx, cx.global, "parseKeyFrame", parse_key_frame::CALL)?;
            exports.function(cx, cx.global, "initSpline", init_spline::CALL)?;
            Ok(())
        }
    }
}

#[tjs_bind::function]
fn is_number(cx: &NativeCx<'_>, input: Value) -> NativeResult<bool> {
    let Value::Str(id) = input else {
        return Ok(true);
    };
    let units = cx.heap().string(id)?;
    let digit = |c: u16| (b'0' as u16..=b'9' as u16).contains(&c);
    // Keep the supplied predicate's grammar, including '-' and repeated dots.
    // Replacing it with Rust's float parser changes which values animate.
    Ok(units.first().is_some_and(|&c| c == b'-' as u16 || digit(c))
        && units[1..].iter().all(|&c| c == b'.' as u16 || digit(c)))
}

#[tjs_bind::function(resumable = true)]
fn parse_key_frame(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
    if let Value::Obj(reference) = input
        && let Some(id) = reference.object
        && cx.heap().array(id).is_ok()
    {
        // Menu timelines already contain arrays; preserve identity and values.
        return Ok(NativeStep::Return(input));
    }
    let Value::Str(source) = input else {
        return Err(NativeError::Type("a keyframe String or Array"));
    };
    let units = cx.heap().string(source)?;
    if units.first() != Some(&(b'(' as u16)) || units.last() != Some(&(b')' as u16)) {
        return Err(NativeError::Detail(format!(
            "invalid keyframe parentheses: {:?}",
            String::from_utf16_lossy(&units[..units.len().min(512)])
        )));
    }
    let length = units.len();
    let result = cx.heap_mut().alloc_array();
    Ok(flow::work(
        Parse {
            source,
            result,
            row: None,
            position: 0,
            start: 0,
            depth: 0,
            length,
        },
        Parse::poll,
    ))
}

#[derive(tjs_bind::Trace)]
struct Parse {
    source: StrId,
    result: ObjId,
    row: Option<ObjId>,
    position: usize,
    start: usize,
    depth: usize,
    length: usize,
}
impl Parse {
    fn field(&self, cx: &mut NativeCx<'_>, row: ObjId) -> NativeResult<()> {
        let text = cx.heap().string(self.source)?[self.start..self.position].to_vec();
        let text = Value::Str(cx.heap_mut().alloc_string(text));
        let field = if cx.heap().array(row)?.is_empty() {
            value::to_number(cx.heap(), text)?
        } else {
            text
        };
        cx.heap_mut().array_push(row, field)?;
        Ok(())
    }
    fn poll(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<Option<NativeStep>> {
        let end = self.position.saturating_add(1024).min(self.length);
        while self.position < end {
            let unit = cx.heap().string(self.source)?[self.position];
            match unit {
                0x28 => {
                    if self.depth == 0 {
                        self.row = Some(cx.heap_mut().alloc_array());
                        self.start = self.position + 1;
                    }
                    self.depth += 1;
                }
                0x29 if self.depth > 1 => self.depth -= 1,
                0x29 if self.depth == 1 => {
                    // The script implementation separates rows at ")(" and
                    // the final ")". A spare ')' inside a field is literal:
                    // shipped Mahoyo animations contain values such as "1)".
                    if self.position + 1 == self.length
                        || cx.heap().string(self.source)?[self.position + 1] == 0x28
                    {
                        self.depth = 0;
                        let row = self.row.take().expect("open keyframe");
                        self.field(cx, row)?;
                        cx.heap_mut()
                            .array_push(self.result, Value::Obj(ObjRef::bound(row)))?;
                    }
                }
                0x2c if self.depth == 1 => {
                    self.field(cx, self.row.expect("open keyframe"))?;
                    self.start = self.position + 1;
                }
                _ if self.depth == 0 => {
                    return Err(NativeError::Detail(format!(
                        "invalid keyframe parentheses at {}: {:?}",
                        self.position,
                        String::from_utf16_lossy(
                            &cx.heap().string(self.source)?[..self.length.min(512)]
                        )
                    )));
                }
                _ => {}
            }
            self.position += 1;
        }
        if self.position != self.length {
            return Ok(None);
        }
        if self.depth != 0 {
            return Err(NativeError::Message("unterminated keyframe"));
        }
        // Like a TJS array literal, native containers need their own context.
        // An unbound closure uses the script caller's this for numeric access.
        Ok(Some(NativeStep::Return(Value::Obj(ObjRef::bound(
            self.result,
        )))))
    }
}

#[tjs_bind::function(resumable = true)]
fn init_spline(cx: &mut NativeCx<'_>, input: Value) -> NativeResult<NativeStep> {
    let keys = object(input)?;
    let count = cx.heap().array(keys)?.len();
    let work = cx.heap_mut().alloc_array();
    if count < 3 {
        // The caller uses linear interpolation when there are no coefficients.
        return Ok(NativeStep::Return(Value::Obj(ObjRef::bound(work))));
    }
    let diagonal = cx.heap_mut().alloc_array();
    cx.heap_mut().array_push(work, Value::Real(0.0))?;
    cx.heap_mut().array_push(diagonal, Value::Real(0.0))?;
    Ok(flow::work(
        Spline {
            keys,
            work,
            diagonal,
            count,
            index: 1,
            backward: false,
            right: 0.0,
        },
        Spline::poll,
    ))
}

// The caller interpolates in normalized keyframe index, not absolute time.
// Its cubic polynomial needs c = second derivative / 6 with zero endpoints:
// c[i-1] + 4*c[i] + c[i+1] = y[i+1] - 2*y[i] + y[i-1].
// Solve this directly instead of copying the old patch's broken unrolled
// forward pass (which subtracts diagonal values where it needs the RHS).
#[derive(tjs_bind::Trace)]
struct Spline {
    keys: ObjId,
    work: ObjId,
    diagonal: ObjId,
    count: usize,
    index: usize,
    backward: bool,
    right: f64,
}
fn sample(heap: &Heap, keys: ObjId, index: usize) -> NativeResult<f64> {
    let row = object(heap.array(keys)?[index])?;
    Ok(value::to_real(
        heap,
        heap.array(row)?.get(1).copied().unwrap_or(Value::Void),
    )?)
}
fn number(heap: &Heap, array: ObjId, index: usize) -> NativeResult<f64> {
    Ok(value::to_real(heap, heap.array(array)?[index])?)
}
impl Spline {
    fn poll(&mut self, cx: &mut NativeCx<'_>) -> NativeResult<Option<NativeStep>> {
        // All variable-size tables are managed arrays, traced across slices.
        for _ in 0..256 {
            let i = self.index;
            if self.backward {
                if i == 0 {
                    // Like the supplied contract, omit the final zero; TJS
                    // reads that missing Array element as void/numeric zero.
                    return Ok(Some(NativeStep::Return(Value::Obj(ObjRef::bound(
                        self.work,
                    )))));
                }
                self.right = (number(cx.heap(), self.work, i)? - self.right)
                    / number(cx.heap(), self.diagonal, i)?;
                cx.heap_mut()
                    .array_set(self.work, i, Value::Real(self.right))?;
                self.index -= 1;
            } else {
                let heap = cx.heap();
                let mut rhs = sample(heap, self.keys, i + 1)? - 2.0 * sample(heap, self.keys, i)?
                    + sample(heap, self.keys, i - 1)?;
                let mut diagonal = 4.0;
                if i > 1 {
                    let inverse = 1.0 / number(heap, self.diagonal, i - 1)?;
                    diagonal -= inverse;
                    rhs -= number(heap, self.work, i - 1)? * inverse;
                }
                cx.heap_mut()
                    .array_push(self.diagonal, Value::Real(diagonal))?;
                cx.heap_mut().array_push(self.work, Value::Real(rhs))?;
                if i == self.count - 2 {
                    self.backward = true;
                } else {
                    self.index += 1;
                }
            }
        }
        Ok(None)
    }
}

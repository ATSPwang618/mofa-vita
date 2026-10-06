//! clipboardEx's saveStruct-derived expression writer, including raw members,
//! captured Array.count, and resumable inherited-array missing handlers.
use super::DATA_LIMIT;
use tjs_core::{
    Heap, HeapError, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId,
    ObjRef, ObjectKind, Trace, Value, member, value,
};
const LIMIT: usize = DATA_LIMIT / 2 - 1;
const DEPTH: usize = 128;
#[derive(Default)]
struct Output {
    text: Vec<u16>,
}
impl Output {
    fn append(&mut self, text: &[u16]) -> NativeResult<()> {
        if self.text.len().saturating_add(text.len()) > LIMIT {
            return Err(NativeError::Message("clipboard TJS exceeds size limit"));
        }
        self.text.extend_from_slice(text);
        Ok(())
    }
    fn buffered(&mut self, text: &[u16]) -> NativeResult<()> {
        self.append(text)
    }
    fn ascii(&mut self, text: &str) -> NativeResult<()> {
        self.append(&text.encode_utf16().collect::<Vec<_>>())
    }
    fn character(&mut self, unit: u16) -> NativeResult<()> {
        self.append(&[unit])
    }
    fn newline(&mut self) -> NativeResult<()> {
        self.ascii("\r\n")
    }
    fn open(&mut self, array: bool) -> NativeResult<()> {
        self.ascii(if array { "[" } else { "%[" })
    }
    fn close(&mut self, _: bool) -> NativeResult<()> {
        self.character(93)
    }
    fn quote(&mut self, text: &[u16]) -> NativeResult<()> {
        self.character(34)?;
        for &unit in tjs_core::string::c_string(text) {
            if matches!(unit, 34 | 92) {
                self.character(92)?;
            }
            self.character(unit)?;
        }
        self.character(34)
    }
}
enum Input {
    Array { index: i32, count: i32 },
    Members(std::vec::IntoIter<Vec<u16>>),
}
struct Frame {
    object: ObjId,
    input: Input,
    first: bool,
}
impl Trace for Frame {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.object.trace(visit);
    }
}
enum Pending {
    Count,
    Item,
}
struct Serialize {
    output: Output,
    after: Option<Box<dyn NativeContinuation>>,
    api: Value,
    stack: Vec<Frame>,
    value: Option<Value>,
    pending: Option<Pending>,
    missing: ObjId,
}
impl Trace for Serialize {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.api.trace(visit);
        if let Some(after) = &self.after {
            after.trace(visit);
        }
        self.stack.trace(visit);
        self.value.trace(visit);
        self.missing.trace(visit);
    }
}
impl Serialize {
    fn count(&mut self, heap: &Heap, result: Value) -> NativeResult<()> {
        let count = value::to_integer(heap, result)? as i32;
        let Input::Array { count: total, .. } = &mut self.stack.last_mut().unwrap().input else {
            unreachable!("count is requested only for arrays")
        };
        if count > 262144 {
            return Err(NativeError::Message("clipboard array exceeds item limit"));
        }
        *total = count;
        Ok(())
    }
    fn object(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        object: ObjId,
    ) -> NativeResult<NativeStep> {
        if self.stack.len() >= DEPTH || self.stack.iter().any(|f| f.object == object) {
            return Err(NativeError::Message(
                "clipboard TJS structure is cyclic or too deep",
            ));
        }
        // IsInstanceOf and EnumMembers return invalid-object statuses which the
        // original ignores: invalid objects serialize as an empty dictionary.
        let valid = cx.heap().is_valid(object)?;
        let name = Value::Str(cx.heap_mut().alloc_string([65, 114, 114, 97, 121]));
        let array = valid && value::instance_of(cx.heap_mut(), Value::Obj(object.into()), name)?;
        self.output.open(array)?;
        let input = if array {
            Input::Array { index: 0, count: 0 }
        } else {
            let entries = if valid {
                let mut names = Vec::new();
                let mut units = 0usize;
                for (key, _, _, _) in cx.heap().all_members_with_flags(object)? {
                    let name = cx.heap().symbol(key)?;
                    units = units.saturating_add(name.len());
                    if names.len() >= 262144 || units > LIMIT {
                        return Err(NativeError::Message(
                            "clipboard dictionary exceeds item limit",
                        ));
                    }
                    names.push(name.to_vec());
                }
                names
            } else {
                Vec::new()
            };
            Input::Members(entries.into_iter())
        };
        self.stack.push(Frame {
            object,
            input,
            first: true,
        });
        if array {
            let Value::Obj(mut count) = self.api else {
                unreachable!("checked at link")
            };
            // AsObject discards the captured closure's context.
            count.this = Some(object);
            match member::get(cx.heap_mut(), Value::Obj(count), Value::Void) {
                Ok(value) => self.count(cx.heap(), value)?,
                Err(member::MemberError::Invoke {
                    function,
                    argument: None,
                }) => {
                    self.pending = Some(Pending::Count);
                    return Ok(NativeStep::Call {
                        function,
                        arguments: vec![],
                        continuation: self,
                    });
                }
                Err(
                    member::MemberError::NotProperty
                    | member::MemberError::AccessDenied
                    | member::MemberError::Heap(HeapError::InvalidObject)
                    | member::MemberError::Native(NativeError::Heap(
                        HeapError::NotArray | HeapError::InvalidObject,
                    ))
                    | member::MemberError::Native(NativeError::This),
                ) => {}
                Err(error) => return Err(NativeError::Detail(error.to_string())),
            }
        }
        Ok(NativeStep::Continue(self))
    }
}
impl NativeContinuation for Serialize {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.pending.take() {
            Some(Pending::Count) => self.count(cx.heap(), result)?,
            Some(Pending::Item) if !matches!(result, Value::Obj(ObjRef { object: Some(id), .. }) if id == self.missing) => {
                self.value = Some(result)
            }
            _ => {}
        }
        // Bound native work. Every pending object/value remains rooted while
        // count getters, inherited-array missing handlers, and GC run.
        for _ in 0..64 {
            if let Some(value) = self.value.take() {
                match value {
                    Value::Obj(ObjRef {
                        object: Some(object),
                        ..
                    }) => return self.object(cx, object),
                    Value::Str(id) => self.output.quote(cx.heap().string(id)?)?,
                    Value::Int(_) => {
                        let text = value::to_string_units(cx.heap(), value)?;
                        self.output.ascii("int ")?;
                        self.output.buffered(&text)?;
                    }
                    Value::Real(real) => {
                        let text = value::to_string_units(cx.heap(), value)?;
                        self.output.ascii("real ")?;
                        if real == 0.0 || !real.is_finite() {
                            self.output.append(&text)?;
                        } else {
                            let bits = real.to_bits();
                            let sign = if real.is_sign_negative() { "-" } else { "" };
                            let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
                            self.output.ascii(&format!(
                                "{sign}0x1.{:013X}p{exponent}",
                                bits & 0x000f_ffff_ffff_ffff
                            ))?;
                        }
                        self.output.ascii(" /* ")?;
                        self.output.append(&text)?;
                        self.output.ascii(" */")?;
                    }
                    Value::Void => self.output.ascii("void")?,
                    Value::Octet(id) => {
                        let data = cx.heap().octet(id)?;
                        if data.len() > LIMIT / 3 {
                            return Err(NativeError::Message("clipboard octet exceeds size limit"));
                        }
                        self.output.ascii("<% ")?;
                        for byte in data {
                            self.output.ascii(&format!("{byte:02x} "))?;
                        }
                        self.output.ascii("%>")?;
                    }
                    _ => self.output.ascii("null")?,
                }
            }
            let Some(frame) = self.stack.last_mut() else {
                let text = Value::Str(
                    cx.heap_mut()
                        .alloc_string(std::mem::take(&mut self.output.text)),
                );
                return self
                    .after
                    .take()
                    .expect("serializer completion")
                    .resume(cx, text);
            };
            let array = matches!(frame.input, Input::Array { .. });
            let (key, value, index) = match &mut frame.input {
                Input::Array { index, count } if *index < *count => {
                    let current = *index;
                    *index += 1;
                    (None, None, Some(current))
                }
                Input::Members(entries) => match entries.next() {
                    Some(key) => {
                        // Freeze enumeration names, not values: an earlier
                        // nested count getter can replace a later raw slot.
                        // Like upstream, insertion/deletion during enumeration
                        // has no specified order; removed names are skipped.
                        if !cx.heap().is_valid(frame.object)? {
                            continue;
                        }
                        let name = cx.heap_mut().intern(&key);
                        match cx.heap().member_with_flags(frame.object, name)? {
                            Some((value, false, _)) => (Some(key), Some(value), None),
                            _ => continue,
                        }
                    }
                    None => {
                        self.stack.pop();
                        self.output.close(false)?;
                        continue;
                    }
                },
                _ => {
                    self.stack.pop();
                    self.output.close(array)?;
                    continue;
                }
            };
            if !frame.first {
                self.output.character(44)?;
                if !array {
                    self.output.newline()?;
                }
            }
            frame.first = false;
            if let Some(key) = key {
                self.output.quote(&key)?;
                self.output.ascii("=>")?;
            }
            self.value = value;
            if let Some(index) = index {
                let object = frame.object;
                if !cx.heap().is_valid(object)? {
                    continue;
                }
                if cx.heap().object(object)?.kind() == ObjectKind::Array {
                    self.value = Some(
                        cx.heap()
                            .array(object)?
                            .get(index as usize)
                            .copied()
                            .unwrap_or(Value::Void),
                    );
                } else {
                    self.pending = Some(Pending::Item);
                    return Ok(NativeStep::GetOr {
                        object: Value::Obj(ObjRef::bound(object)),
                        key: Value::Int(i64::from(index)),
                        raw: true,
                        fallback: Value::Obj(self.missing.into()),
                        continuation: self,
                    });
                }
            }
        }
        Ok(NativeStep::Continue(self))
    }
}

pub(super) fn start(
    cx: &mut NativeCx<'_>,
    value: Value,
    api: Value,
    after: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let missing = cx.heap_mut().alloc_dictionary();
    Ok(NativeStep::Continue(Box::new(Serialize {
        output: Output::default(),
        after: Some(after),
        api,
        stack: Vec::new(),
        value: Some(value),
        pending: None,
        missing,
    })))
}

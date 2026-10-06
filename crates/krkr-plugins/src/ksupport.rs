//! Collection operations from kwidgets utils.cpp (e7bed32).
use crate::exports::arg;
use std::{cmp::Ordering, collections::HashSet};
use tjs_core::{
    Heap, NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep,
    ObjId, ObjRef, ObjectKind, Value, value,
};

krkr_engine::native_plugin! {
    pub(crate) Support {
        names: ["ksupport.dll", "ksupport.tpm"],
        link(cx, exports) {
        use NativeCallable::Resumable;
        for (name, function) in [
            ("equalStruct", Resumable(equal)),
            ("equalStructNumericLoose", Resumable(equal_loose)),
            ("dictionaryKeys", keys::CALL),
            ("dictionaryValues", values::CALL),
            ("arrayHash", Resumable(array_hash)),
            ("unionDictionary", Resumable(union_dictionary)),
            ("unionSet", Resumable(union)),
            ("intersectionSet", Resumable(intersection)),
            ("differenceSet", Resumable(difference)),
            ("sliceArray", Resumable(slice)),
            ("eachArray", Resumable(each_array)),
            ("eachDictionary", Resumable(each_dictionary)),
        ] {
            exports.function(cx, cx.global, name, function)?;
        }
        super::geometry::install(cx, exports)?;
        super::drawing::install(cx, exports)?;
        super::table::install(cx, exports)?;
        super::timeline::install(cx, exports)?;
        Ok(())
        }
    }
}
const LIMIT: usize = 262_144;
fn closure(value: Value) -> NativeResult<ObjRef> {
    if let Value::Obj(r) = value {
        Ok(r)
    } else {
        Err(NativeError::Type("an object"))
    }
}
fn live(heap: &Heap, value: Value) -> NativeResult<Option<ObjId>> {
    let Some(id) = closure(value)?.object else {
        return Ok(None);
    };
    Ok(heap.is_valid(id)?.then_some(id))
}
fn result_array(heap: &mut Heap, values: &[Value]) -> NativeResult<Value> {
    Ok(Value::Obj(ObjRef::bound(heap.alloc_array_from(values)?)))
}
fn text(cx: &mut NativeCx<'_>, name: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
    )
}
fn entries(heap: &Heap, source: Value) -> NativeResult<Vec<(Vec<u16>, Value)>> {
    let Some(id) = live(heap, source)? else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::new();
    for (key, value, hidden, static_) in heap.all_members_with_flags(id)? {
        if hidden && !static_ {
            continue;
        }
        if entries.len() >= LIMIT {
            return Err(NativeError::Message(
                "plugin collection exceeds work buffer limit",
            ));
        }
        entries
            .try_reserve(1)
            .map_err(|_| NativeError::Message("plugin collection allocation failed"))?;
        entries.push((heap.symbol(key)?.to_vec(), value));
    }
    Ok(entries)
}
fn member_count(heap: &Heap, source: Value) -> NativeResult<usize> {
    let Some(id) = live(heap, source)? else {
        return Ok(0);
    };
    Ok(heap.all_members_with_flags(id)?.count())
}
#[tjs_bind::function]
fn keys(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<tjs_bind::Array<Vec<tjs_bind::Utf16>>> {
    let entries = entries(cx.heap(), arg(args, 0)?)?;
    let values: Vec<_> = entries
        .into_iter()
        .map(|(key, _)| tjs_bind::Utf16(key))
        .collect();
    Ok(tjs_bind::Array(values))
}
#[tjs_bind::function]
fn values(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<tjs_bind::Array<Vec<Value>>> {
    let values: Vec<_> = entries(cx.heap(), arg(args, 0)?)?
        .into_iter()
        .map(|(_, v)| v)
        .collect();
    Ok(tjs_bind::Array(values))
}
// Failed dispatch statuses leave C++'s reused temporary untouched. Null and
// invalid objects are checked before scheduling, while script exceptions escape.
fn get(
    cx: &mut NativeCx<'_>,
    source: Value,
    key: Value,
    raw: bool,
    previous: Value,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    if live(cx.heap(), source)?.is_none() {
        return Ok(tjs_bind::flow::deliver(previous, next));
    }
    Ok(NativeStep::GetOr {
        object: source,
        key,
        raw,
        fallback: previous,
        continuation: next,
    })
}
fn count(
    cx: &mut NativeCx<'_>,
    source: Value,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let key = text(cx, "count");
    get(cx, source, key, false, Value::Void, next)
}
fn length(cx: &NativeCx<'_>, value: Value) -> NativeResult<i32> {
    let count = value::to_integer(cx.heap(), value)? as i32;
    if count > LIMIT as i32 {
        return Err(NativeError::Message(
            "plugin collection exceeds work buffer limit",
        ));
    }
    Ok(count.max(0))
}
fn sorted(heap: &Heap, mut items: Vec<Value>) -> NativeResult<Vec<Value>> {
    // A mixed string/numeric/NaN comparator need not form a total order. The
    // original std::sort leaves that input unspecified; use bounded merge sort
    // so it cannot trigger Rust sort's inconsistent-comparator panic.
    let mut buffer = items.clone();
    let mut width = 1;
    while width < items.len() {
        for start in (0..items.len()).step_by(width * 2) {
            let middle = (start + width).min(items.len());
            let end = (middle + width).min(items.len());
            let (mut a, mut b) = (start, middle);
            for slot in &mut buffer[start..end] {
                if a < middle
                    && (b == end
                        || value::compare_in(heap, items[b], items[a])? != Some(Ordering::Less))
                {
                    *slot = items[a];
                    a += 1;
                } else {
                    *slot = items[b];
                    b += 1;
                }
            }
        }
        std::mem::swap(&mut items, &mut buffer);
        width *= 2;
    }
    Ok(items)
}
fn set_result(
    cx: &mut NativeCx<'_>,
    a: Vec<Value>,
    b: Vec<Value>,
    mode: u8,
) -> NativeResult<Value> {
    let a = sorted(cx.heap(), a)?;
    let b = sorted(cx.heap(), b)?;
    let (mut i, mut j) = (0, 0);
    let mut output = Vec::new();
    while i < a.len() && j < b.len() {
        match value::compare_in(cx.heap(), a[i], b[j])?.unwrap_or(Ordering::Equal) {
            Ordering::Less => {
                if mode != 1 {
                    output.push(a[i]);
                }
                i += 1;
            }
            Ordering::Greater => {
                if mode == 0 {
                    output.push(b[j]);
                }
                j += 1;
            }
            Ordering::Equal => {
                if mode != 2 {
                    output.push(a[i]);
                }
                i += 1;
                j += 1;
            }
        }
    }
    if mode != 1 {
        output.extend_from_slice(&a[i..]);
    }
    if mode == 0 {
        output.extend_from_slice(&b[j..]);
    }
    result_array(cx.heap_mut(), &output)
}
#[derive(tjs_bind::Trace)]
enum ArrayOp {
    Hash,
    Slice,
    Set(u8),
    Each { function: Value, extra: Vec<Value> },
}
#[derive(tjs_bind::Trace)]
struct ArrayWalk {
    source: Value,
    other: Option<Value>,
    op: ArrayOp,
    values: Vec<Value>,
    left: Option<Vec<Value>>,
    output: Option<Value>,
    index: i32,
    end: i32,
    previous: Value,
    waiting_count: bool,
    waiting_value: bool,
}
impl NativeContinuation for ArrayWalk {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        value: Value,
    ) -> NativeResult<NativeStep> {
        if self.waiting_count {
            self.waiting_count = false;
            if matches!(self.op, ArrayOp::Slice) {
                // utils.cpp converts this count but never uses it for slicing.
                let _ = value::to_integer(cx.heap(), value)? as i32;
            } else {
                if matches!(self.op, ArrayOp::Set(_))
                    && (value::to_integer(cx.heap(), value)? as i32) < 0
                {
                    return Err(NativeError::Message("negative set source count"));
                }
                self.end = length(cx, value)?;
            }
        } else if self.waiting_value {
            self.waiting_value = false;
            self.previous = value;
            let key = Value::Int(i64::from(self.index));
            self.index += 1;
            if let ArrayOp::Each { function, extra } = &self.op {
                let function = *function;
                let mut arguments = vec![key, value];
                arguments.extend_from_slice(extra);
                // AsObjectClosure converts eagerly, but a null/default-less
                // callable returns an ignored failure status for each item.
                if callable(cx.heap(), function)? {
                    return Ok(NativeStep::CallDiscard {
                        function,
                        arguments,
                        continuation: self,
                    });
                }
            } else if matches!(self.op, ArrayOp::Hash) {
                let key = value::to_string_units(cx.heap(), value)?;
                let key = cx.heap_mut().intern(tjs_core::string::c_string(&key));
                let out = closure(self.output.expect("hash destination"))?
                    .object
                    .expect("dictionary");
                cx.heap_mut().set_member(out, key, Value::Int(1))?;
            } else {
                self.values.push(value);
            }
        }
        if self.index < self.end {
            self.waiting_value = true;
            return get(
                cx,
                self.source,
                Value::Int(i64::from(self.index)),
                true,
                self.previous,
                self,
            );
        }
        if let Some(other) = self.other.take() {
            self.left = Some(std::mem::take(&mut self.values));
            self.source = other;
            self.index = 0;
            self.previous = Value::Void;
            self.waiting_count = true;
            return count(cx, other, self);
        }
        let result = match self.op {
            ArrayOp::Each { .. } => Value::Void,
            ArrayOp::Slice => result_array(cx.heap_mut(), &self.values)?,
            ArrayOp::Set(mode) => set_result(
                cx,
                self.left.take().unwrap_or_default(),
                std::mem::take(&mut self.values),
                mode,
            )?,
            ArrayOp::Hash => self.output.expect("hash destination"),
        };
        Ok(NativeStep::Return(result))
    }
}
fn callable(heap: &Heap, function: Value) -> NativeResult<bool> {
    Ok(live(heap, function)?.is_some_and(|id| {
        heap.object(id).is_ok_and(|r| {
            matches!(
                r.kind(),
                ObjectKind::Function
                    | ObjectKind::NativeFunction
                    | ObjectKind::Class
                    | ObjectKind::NativeClass
            )
        })
    }))
}
fn start_array(cx: &mut NativeCx<'_>, args: &[Value], op: ArrayOp) -> NativeResult<NativeStep> {
    if matches!(op, ArrayOp::Set(_)) && args.len() < 2 {
        return Err(NativeError::Missing(args.len()));
    }
    let source = arg(args, 0)?;
    closure(source)?;
    let output = matches!(op, ArrayOp::Hash)
        .then(|| Value::Obj(ObjRef::bound(cx.heap_mut().alloc_dictionary())));
    let mut walk = ArrayWalk {
        source,
        other: None,
        op,
        values: Vec::new(),
        left: None,
        index: 0,
        end: 0,
        output,
        previous: Value::Void,
        waiting_count: true,
        waiting_value: false,
    };
    match &walk.op {
        ArrayOp::Set(_) => {
            walk.other = Some(arg(args, 1)?);
        }
        ArrayOp::Slice => {
            walk.index = value::to_integer(cx.heap(), arg(args, 1)?)? as i32;
            let size = value::to_integer(cx.heap(), arg(args, 2)?)? as i32;
            if size > LIMIT as i32 {
                return Err(NativeError::Message(
                    "plugin slice exceeds work buffer limit",
                ));
            }
            // Signed overflow in the source's from+size is undefined. Reject it.
            walk.end = walk
                .index
                .checked_add(size.max(0))
                .ok_or(NativeError::Message("slice index overflow"))?;
        }
        _ => {}
    }
    count(cx, source, Box::new(walk))
}
fn array_hash(cx: &mut NativeCx<'_>, a: &[Value]) -> NativeResult<NativeStep> {
    start_array(cx, a, ArrayOp::Hash)
}
fn union(cx: &mut NativeCx<'_>, a: &[Value]) -> NativeResult<NativeStep> {
    start_array(cx, a, ArrayOp::Set(0))
}
fn intersection(cx: &mut NativeCx<'_>, a: &[Value]) -> NativeResult<NativeStep> {
    start_array(cx, a, ArrayOp::Set(1))
}
fn difference(cx: &mut NativeCx<'_>, a: &[Value]) -> NativeResult<NativeStep> {
    start_array(cx, a, ArrayOp::Set(2))
}
fn slice(cx: &mut NativeCx<'_>, a: &[Value]) -> NativeResult<NativeStep> {
    if a.len() < 3 {
        return Err(NativeError::Missing(a.len()));
    }
    start_array(cx, a, ArrayOp::Slice)
}
fn each_array(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let function = arg(args, 1)?;
    closure(function)?;
    start_array(
        cx,
        args,
        ArrayOp::Each {
            function,
            extra: args[2..].to_vec(),
        },
    )
}
// Only names are frozen: an existing slot's value is read immediately before
// its callback. Source C++ explicitly leaves insert/delete during enumeration
// unspecified; freezing names keeps those cases memory safe.
#[derive(tjs_bind::Trace)]
struct DictionaryWalk {
    source: Value,
    other: Option<Value>,
    names: Vec<Vec<u16>>,
    index: usize,
    function: Option<Value>,
    extra: Vec<Value>,
    output: Option<Value>,
}
impl DictionaryWalk {
    fn new(heap: &Heap, source: Value) -> NativeResult<Self> {
        Ok(Self {
            source,
            other: None,
            names: entries(heap, source)?
                .into_iter()
                .map(|(key, _)| key)
                .collect(),
            index: 0,
            function: None,
            extra: Vec::new(),
            output: None,
        })
    }
}
impl NativeContinuation for DictionaryWalk {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        loop {
            while self.index < self.names.len() {
                let units = &self.names[self.index];
                self.index += 1;
                let Some(id) = live(cx.heap(), self.source)? else {
                    self.index = self.names.len();
                    break;
                };
                let symbol = cx.heap_mut().intern(units);
                let Some((value, hidden, static_)) = cx.heap().member_with_flags(id, symbol)?
                else {
                    continue;
                };
                if hidden && !static_ {
                    continue;
                }
                let key = Value::Str(cx.heap_mut().alloc_string(units.clone()));
                if let Some(function) = self.function {
                    if !callable(cx.heap(), function)? {
                        continue;
                    }
                    let mut arguments = vec![key, value];
                    arguments.extend_from_slice(&self.extra);
                    return Ok(NativeStep::CallDiscard {
                        function,
                        arguments,
                        continuation: self,
                    });
                }
                return Ok(NativeStep::Set {
                    object: self.output.expect("union destination"),
                    key,
                    value,
                    continuation: self,
                });
            }
            if let Some(other) = self.other.take() {
                self.source = other;
                self.names = entries(cx.heap(), other)?
                    .into_iter()
                    .map(|(key, _)| key)
                    .collect();
                self.index = 0;
                continue;
            }
            return Ok(NativeStep::Return(self.output.unwrap_or(Value::Void)));
        }
    }
}
fn union_dictionary(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let a = arg(args, 0)?;
    let b = arg(args, 1)?;
    closure(a)?;
    closure(b)?;
    let mut walk = DictionaryWalk::new(cx.heap(), a)?;
    walk.other = Some(b);
    walk.output = Some(Value::Obj(ObjRef::bound(cx.heap_mut().alloc_dictionary())));
    Ok(NativeStep::Continue(Box::new(walk)))
}
fn each_dictionary(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let source = arg(args, 0)?;
    let function = arg(args, 1)?;
    closure(source)?;
    closure(function)?;
    let mut walk = DictionaryWalk::new(cx.heap(), source)?;
    walk.function = Some(function);
    walk.extra = args[2..].to_vec();
    Ok(NativeStep::Continue(Box::new(walk)))
}

#[derive(tjs_bind::Trace)]
enum Frame {
    Pair(Value, Value),
    DictionaryStart(Value, Value),
    LeavePair(ObjId, ObjId),
    Array {
        a: Value,
        b: Value,
        index: i32,
        count: i32,
        previous_a: Value,
        previous_b: Value,
    },
    Dictionary {
        a: Value,
        b: Value,
        names: Vec<Vec<u16>>,
        index: usize,
    },
}
#[derive(tjs_bind::Trace)]
enum Await {
    None,
    KeysA(Value, Value),
    KeysB(Value, Value, Value),
    CountA(Value, Value),
    CountB(Value, Value, Value),
    ArrayA {
        a: Value,
        b: Value,
        index: i32,
        count: i32,
        previous_b: Value,
    },
    ArrayB {
        a: Value,
        b: Value,
        index: i32,
        count: i32,
        left: Value,
    },
    Dictionary(Value),
}
#[derive(tjs_bind::Trace)]
struct Compare {
    stack: Vec<Frame>,
    waiting: Await,
    seen: HashSet<(ObjId, ObjId)>,
    missing: Value,
    loose: bool,
    scripts_ex: bool,
}
fn is_instance(cx: &mut NativeCx<'_>, source: Value, name: &str) -> NativeResult<bool> {
    if live(cx.heap(), source)?.is_none() {
        return Ok(false);
    }
    let name = text(cx, name);
    Ok(value::instance_of(cx.heap_mut(), source, name)?)
}
impl NativeContinuation for Compare {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match std::mem::replace(&mut self.waiting, Await::None) {
            Await::None => {}
            Await::KeysA(a, b) => {
                self.waiting = Await::KeysB(a, b, result);
                return crate::scripts_ex::keys_for(cx, b, self);
            }
            Await::KeysB(a, b, left) => {
                self.stack.push(Frame::DictionaryStart(a, b));
                self.stack.push(Frame::Pair(left, result));
            }
            Await::CountA(a, b) => {
                self.waiting = Await::CountB(a, b, result);
                return count(cx, b, self);
            }
            Await::CountB(a, b, left) => {
                if !value::strict_equal(cx.heap(), left, result)? {
                    return Ok(NativeStep::Return(Value::Int(0)));
                }
                let count = length(cx, left)?;
                self.stack.push(Frame::Array {
                    a,
                    b,
                    index: 0,
                    count,
                    previous_a: Value::Void,
                    previous_b: Value::Void,
                });
            }
            Await::ArrayA {
                a,
                b,
                index,
                count,
                previous_b,
            } => {
                self.waiting = Await::ArrayB {
                    a,
                    b,
                    index,
                    count,
                    left: result,
                };
                return get(cx, b, Value::Int(i64::from(index)), true, previous_b, self);
            }
            Await::ArrayB {
                a,
                b,
                index,
                count,
                left,
            } => {
                self.stack.push(Frame::Array {
                    a,
                    b,
                    index: index + 1,
                    count,
                    previous_a: left,
                    previous_b: result,
                });
                self.stack.push(Frame::Pair(left, result));
            }
            Await::Dictionary(left) => {
                // A derived Dictionary can retain plain-object dispatch:
                // MEMBERNOTFOUND is skipped, whereas Dictionary's successful
                // void (or a missing handler explicitly returning void) is
                // compared. Keep the marker alive across that handler's GC.
                if value::strict_equal(cx.heap(), result, self.missing)? {
                    if self.scripts_ex && !self.loose {
                        return Ok(NativeStep::Return(Value::Int(0)));
                    }
                } else {
                    self.stack.push(Frame::Pair(left, result));
                }
            }
        }
        for _ in 0..64 {
            match self.stack.pop() {
                None => return Ok(NativeStep::Return(Value::Int(1))),
                Some(Frame::DictionaryStart(a, b)) => {
                    let names = entries(cx.heap(), a)?
                        .into_iter()
                        .map(|(key, _)| key)
                        .collect();
                    self.stack.push(Frame::Dictionary {
                        a,
                        b,
                        names,
                        index: 0,
                    });
                }
                Some(Frame::LeavePair(a, b)) => {
                    self.seen.remove(&(a, b));
                }
                Some(Frame::Array {
                    a,
                    b,
                    index,
                    count,
                    previous_a,
                    previous_b,
                }) => {
                    if index >= count {
                        continue;
                    }
                    self.waiting = Await::ArrayA {
                        a,
                        b,
                        index,
                        count,
                        previous_b,
                    };
                    return get(cx, a, Value::Int(i64::from(index)), true, previous_a, self);
                }
                Some(Frame::Dictionary {
                    a,
                    b,
                    names,
                    mut index,
                }) => {
                    if index >= names.len() {
                        continue;
                    }
                    let name = names[index].clone();
                    index += 1;
                    self.stack.push(Frame::Dictionary { a, b, names, index });
                    let Some(id) = live(cx.heap(), a)? else {
                        continue;
                    };
                    let symbol = cx.heap_mut().intern(&name);
                    let Some((left, hidden, static_)) = cx.heap().member_with_flags(id, symbol)?
                    else {
                        continue;
                    };
                    if hidden && !static_ {
                        continue;
                    }
                    if live(cx.heap(), b)?.is_none() {
                        if self.scripts_ex && !self.loose {
                            return Ok(NativeStep::Return(Value::Int(0)));
                        }
                        continue;
                    }
                    self.waiting = Await::Dictionary(left);
                    let key = Value::Str(cx.heap_mut().alloc_string(name));
                    if self.scripts_ex && !self.loose {
                        return Ok(NativeStep::GetRequiredOr {
                            object: b,
                            key,
                            fallback: self.missing,
                            continuation: self,
                        });
                    }
                    return get(cx, b, key, false, self.missing, self);
                }
                Some(Frame::Pair(a, b)) => {
                    if let (Value::Obj(ar), Value::Obj(br)) = (a, b) {
                        // Object identity alone wins, even with different this.
                        if ar.object == br.object {
                            continue;
                        }
                        if let (Some(ai), Some(bi)) = (ar.object, br.object) {
                            if is_instance(cx, a, "Function")? && is_instance(cx, b, "Function")? {
                                if !value::strict_equal(cx.heap(), a, b)? {
                                    return Ok(NativeStep::Return(Value::Int(0)));
                                }
                                continue;
                            }
                            let array =
                                is_instance(cx, a, "Array")? && is_instance(cx, b, "Array")?;
                            let dictionary = !array
                                && is_instance(cx, a, "Dictionary")?
                                && is_instance(cx, b, "Dictionary")?;
                            if array || dictionary {
                                // Reference recursively overflows on distinct cyclic
                                // graphs. Pair memoization bounds this safely.
                                if !self.seen.insert((ai, bi)) {
                                    continue;
                                }
                                if self.seen.len() > LIMIT {
                                    return Err(NativeError::Message(
                                        "structure comparison exceeds node limit",
                                    ));
                                }
                                self.stack.push(Frame::LeavePair(ai, bi));
                                if array {
                                    self.waiting = Await::CountA(a, b);
                                    return count(cx, a, self);
                                }
                                if self.scripts_ex && !self.loose {
                                    self.waiting = Await::KeysA(a, b);
                                    return crate::scripts_ex::keys_for(cx, a, self);
                                }
                                if member_count(cx.heap(), a)? != member_count(cx.heap(), b)? {
                                    return Ok(NativeStep::Return(Value::Int(0)));
                                }
                                let names = entries(cx.heap(), a)?
                                    .into_iter()
                                    .map(|(key, _)| key)
                                    .collect();
                                self.stack.push(Frame::Dictionary {
                                    a,
                                    b,
                                    names,
                                    index: 0,
                                });
                                continue;
                            }
                        }
                    }
                    let equal = if self.loose
                        && matches!(a, Value::Int(_) | Value::Real(_))
                        && matches!(b, Value::Int(_) | Value::Real(_))
                    {
                        if let (false, Value::Real(a), Value::Real(b)) = (self.scripts_ex, a, b) {
                            ((a as f32) - (b as f32)).abs() < f32::EPSILON
                        } else {
                            value::equal(cx.heap(), a, b)?
                        }
                    } else {
                        value::strict_equal(cx.heap(), a, b)?
                    };
                    if !equal {
                        return Ok(NativeStep::Return(Value::Int(0)));
                    }
                }
            }
        }
        Ok(NativeStep::Continue(self))
    }
}
fn compare(cx: &mut NativeCx<'_>, args: &[Value], loose: bool) -> NativeResult<NativeStep> {
    compare_rules(cx, args, loose, false)
}
pub(crate) fn scripts_compare(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    loose: bool,
) -> NativeResult<NativeStep> {
    compare_rules(cx, args, loose, true)
}
fn compare_rules(
    cx: &mut NativeCx<'_>,
    args: &[Value],
    loose: bool,
    scripts_ex: bool,
) -> NativeResult<NativeStep> {
    let pair = Frame::Pair(arg(args, 0)?, arg(args, 1)?);
    Ok(NativeStep::Continue(Box::new(Compare {
        stack: vec![pair],
        waiting: Await::None,
        seen: HashSet::new(),
        missing: Value::Obj(cx.heap_mut().alloc_object().into()),
        loose,
        scripts_ex,
    })))
}
fn equal(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    compare(cx, args, false)
}
fn equal_loose(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    compare(cx, args, true)
}

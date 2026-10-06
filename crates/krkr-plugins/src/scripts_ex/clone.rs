use super::*;

enum Cursor {
    Array { index: i32, count: i32 },
    Members(std::vec::IntoIter<Vec<u16>>),
}
struct Frame {
    source: Value,
    target: ObjId,
    cursor: Cursor,
    previous: Value,
    key: Value,
    flags: MemberFlags,
}
impl Trace for Frame {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.source.trace(v);
        self.target.trace(v);
        self.previous.trace(v);
        self.key.trace(v);
    }
}
enum Await {
    Count,
    Item,
    Stored,
    Custom(Value),
}
struct Clone {
    stack: Vec<Frame>,
    value: Option<Value>,
    waiting: Option<Await>,
    done: Option<Value>,
    remaining: usize,
}
impl Trace for Clone {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.stack.trace(v);
        self.value.trace(v);
        self.done.trace(v);
        if let Some(Await::Custom(value)) = self.waiting {
            value.trace(v);
        }
    }
}
pub(super) fn start(_: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    Ok(NativeStep::Continue(Box::new(Clone {
        stack: vec![],
        value: Some(arg(args, 0)?),
        waiting: None,
        done: None,
        remaining: ITEMS,
    })))
}
impl NativeContinuation for Clone {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.waiting.take() {
            Some(Await::Count) => {
                let Cursor::Array { count, .. } = &mut self.stack.last_mut().unwrap().cursor else {
                    unreachable!()
                };
                *count = length(cx, result)?;
            }
            Some(Await::Item) => self.value = Some(result),
            // krkr2 tests FuncCall == S_TRUE (1), but successful script calls
            // return S_OK (0). Keep its callback side effects and original value.
            Some(Await::Custom(source)) => self.done = Some(source),
            _ => {}
        }
        for _ in 0..64 {
            if let Some(value) = self.value.take() {
                if self.remaining == 0 {
                    return Err(NativeError::Message("ScriptsEx clone exceeds node limit"));
                }
                self.remaining -= 1;
                let array = matches!(value, Value::Obj(_)) && instance(cx, value, "Array")?;
                let dictionary =
                    !array && matches!(value, Value::Obj(_)) && instance(cx, value, "Dictionary")?;
                if array || dictionary {
                    let source = closure(value)?.object;
                    if self.stack.len() >= 128
                        || self
                            .stack
                            .iter()
                            .any(|f| closure(f.source).is_ok_and(|r| r.object == source))
                    {
                        return Err(NativeError::Message(
                            "ScriptsEx clone is cyclic or too deep",
                        ));
                    }
                    let target = if array {
                        cx.heap_mut().alloc_array()
                    } else {
                        cx.heap_mut().alloc_dictionary()
                    };
                    let cursor = if array {
                        Cursor::Array { index: 0, count: 0 }
                    } else {
                        Cursor::Members(names(cx.heap(), value)?.into_iter())
                    };
                    self.stack.push(Frame {
                        source: value,
                        target,
                        cursor,
                        previous: Value::Void,
                        key: Value::Void,
                        flags: MemberFlags::default(),
                    });
                    if array {
                        self.waiting = Some(Await::Count);
                        let key = text(cx, "count");
                        return read(cx, value, key, false, Value::Void, self);
                    }
                } else if matches!(value, Value::Obj(_)) && live(cx.heap(), value)?.is_some() {
                    self.waiting = Some(Await::Custom(value));
                    let key = text(cx, "clone");
                    return Ok(NativeStep::CallMemberOr {
                        object: value,
                        key,
                        arguments: vec![],
                        result_needed: true,
                        continuation: self,
                    });
                } else {
                    self.done = Some(value);
                }
            }
            if let Some(value) = self.done.take() {
                let Some(frame) = self.stack.last_mut() else {
                    return Ok(NativeStep::Return(value));
                };
                frame.previous = value;
                self.waiting = Some(Await::Stored);
                return Ok(match frame.cursor {
                    Cursor::Array { .. } => NativeStep::CallMemberOr {
                        object: Value::Obj(ObjRef::bound(frame.target)),
                        key: text(cx, "add"),
                        arguments: vec![value],
                        result_needed: false,
                        continuation: self,
                    },
                    Cursor::Members(_) => NativeStep::SetProperty {
                        object: Value::Obj(ObjRef::bound(frame.target)),
                        key: frame.key,
                        value,
                        flags: frame.flags,
                        continuation: self,
                    },
                });
            }
            let frame = self.stack.last_mut().expect("pending container");
            match &mut frame.cursor {
                Cursor::Array { index, count } if *index < *count => {
                    let key = Value::Int((*index).into());
                    *index += 1;
                    let source = frame.source;
                    let previous = frame.previous;
                    self.waiting = Some(Await::Item);
                    return read(cx, source, key, true, previous, self);
                }
                Cursor::Members(names) => {
                    if let Some(name) = names.next() {
                        let Some((value, hidden, class_only)) = member(cx, frame.source, &name)?
                        else {
                            continue;
                        };
                        frame.key = Value::Str(cx.heap_mut().alloc_string(name));
                        frame.flags = MemberFlags {
                            ensure: true,
                            hidden,
                            class_only,
                            ..Default::default()
                        };
                        self.value = Some(value);
                        continue;
                    }
                    self.done = Some(Value::Obj(ObjRef::bound(frame.target)));
                    self.stack.pop();
                }
                _ => {
                    self.done = Some(Value::Obj(ObjRef::bound(frame.target)));
                    self.stack.pop();
                }
            }
        }
        Ok(NativeStep::Continue(self))
    }
}

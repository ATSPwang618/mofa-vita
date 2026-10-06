use super::*;

enum Cursor {
    Array { index: i32, count: i32 },
    Members(std::vec::IntoIter<Vec<u16>>),
}
enum Await {
    Count,
    Item,
    Callback,
}
struct Each {
    source: Value,
    callback: Value,
    extra: Vec<Value>,
    cursor: Cursor,
    waiting: Option<Await>,
    previous: Value,
    key: Value,
}
impl Trace for Each {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.source.trace(v);
        self.callback.trace(v);
        self.extra.trace(v);
        self.previous.trace(v);
        self.key.trace(v);
    }
}
pub(super) fn start(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let callback = arg(args, 1)?;
    let source = Value::Obj(closure(args[0])?);
    let mut callback = closure(callback)?;
    callback.this = callback.this.or(Some(cx.this()));
    if args.len() > ITEMS {
        return Err(NativeError::Message("ScriptsEx callback argument limit"));
    }
    let array = instance(cx, source, "Array")?;
    let cursor = if array {
        Cursor::Array { index: 0, count: 0 }
    } else {
        Cursor::Members(names(cx.heap(), source)?.into_iter())
    };
    let task = Box::new(Each {
        source,
        callback: Value::Obj(callback),
        extra: args[2..].to_vec(),
        cursor,
        waiting: array.then_some(Await::Count),
        previous: Value::Void,
        key: Value::Void,
    });
    if array {
        let key = text(cx, "count");
        read(cx, source, key, false, Value::Void, task)
    } else {
        Ok(NativeStep::Continue(task))
    }
}
impl Each {
    fn call(mut self: Box<Self>, cx: &NativeCx<'_>) -> NativeResult<NativeStep> {
        let mut arguments = Vec::with_capacity(self.extra.len() + 2);
        arguments.extend([self.key, self.previous]);
        arguments.extend_from_slice(&self.extra);
        self.waiting = Some(Await::Callback);
        // Default FuncCall: failed dispatch leaves the cleared break result
        // void; real callback errors remain in the caller's VM exception chain.
        let callable = closure(self.callback)?.object.is_some_and(|id| {
            cx.heap().is_valid(id).unwrap_or(false)
                && cx.heap().object(id).is_ok_and(|o| {
                    matches!(
                        o.kind(),
                        ObjectKind::Function
                            | ObjectKind::NativeFunction
                            | ObjectKind::Class
                            | ObjectKind::NativeClass
                    )
                })
        });
        if callable {
            Ok(NativeStep::Call {
                function: self.callback,
                arguments,
                continuation: self,
            })
        } else {
            Ok(NativeStep::Continue(self))
        }
    }
}
impl NativeContinuation for Each {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        match self.waiting.take() {
            Some(Await::Count) => {
                let Cursor::Array { count, .. } = &mut self.cursor else {
                    unreachable!()
                };
                *count = length(cx, result)?;
            }
            Some(Await::Item) => {
                self.previous = result;
                return self.call(cx);
            }
            Some(Await::Callback) if !matches!(result, Value::Void) => {
                return Ok(NativeStep::Return(result));
            }
            _ => {}
        }
        for _ in 0..64 {
            match &mut self.cursor {
                Cursor::Array { index, count } => {
                    if *index >= *count {
                        return Ok(NativeStep::Return(Value::Void));
                    }
                    self.key = Value::Int((*index).into());
                    *index += 1;
                    self.waiting = Some(Await::Item);
                    return read(cx, self.source, self.key, true, self.previous, self);
                }
                Cursor::Members(names) => {
                    let Some(name) = names.next() else {
                        return Ok(NativeStep::Return(Value::Void));
                    };
                    let Some((value, hidden, static_)) = member(cx, self.source, &name)? else {
                        continue;
                    };
                    // Reference compares the entire flag word to HIDDEN, so
                    // hidden+static members are visited unlike getObjectKeys.
                    if hidden && !static_ {
                        continue;
                    }
                    self.key = Value::Str(cx.heap_mut().alloc_string(name));
                    self.previous = value;
                    return self.call(cx);
                }
            }
        }
        Ok(NativeStep::Continue(self))
    }
}

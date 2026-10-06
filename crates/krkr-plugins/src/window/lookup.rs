//! Mutable, lazily published compatibility dictionaries, as in windowEx.
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjRef, Trace, Value,
    value,
};
pub(super) fn lookup(
    cx: &mut NativeCx<'_>,
    metric: bool,
    key: Value,
    reverse: bool,
) -> NativeResult<NativeStep> {
    let class = cx
        .heap()
        .registered_class(if metric { "System" } else { "Window" })
        .ok_or(NativeError::This)?;
    let owner = Value::Obj(ObjRef::bound(class));
    let member = Value::Str(
        cx.heap_mut().alloc_string(
            if metric { "metrics" } else { "_Notifications" }
                .encode_utf16()
                .collect::<Vec<_>>(),
        ),
    );
    let missing = Value::Obj(ObjRef::bound(cx.heap_mut().alloc_dictionary()));
    Ok(NativeStep::GetRequiredOr {
        object: owner,
        key: member,
        fallback: missing,
        continuation: Box::new(Table {
            owner,
            member,
            missing,
            key,
            metric,
            reverse,
        }),
    })
}
struct Table {
    owner: Value,
    member: Value,
    missing: Value,
    key: Value,
    metric: bool,
    reverse: bool,
}
impl Trace for Table {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for value in [self.owner, self.member, self.missing, self.key] {
            value.trace(visit);
        }
    }
}
impl NativeContinuation for Table {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        mut table: Value,
    ) -> NativeResult<NativeStep> {
        if value::strict_equal(cx.heap(), table, self.missing)? {
            let id = cx.heap_mut().alloc_dictionary();
            let values = if self.metric {
                super::names::METRICS
            } else {
                super::names::NOTIFICATIONS
            };
            for &(name, number) in values {
                let key = cx
                    .heap_mut()
                    .intern(&name.encode_utf16().collect::<Vec<_>>());
                cx.heap_mut().set_member(id, key, Value::Int(number))?;
                if !self.metric {
                    let key = cx
                        .heap_mut()
                        .intern(&number.to_string().encode_utf16().collect::<Vec<_>>());
                    let name = Value::Str(
                        cx.heap_mut()
                            .alloc_string(name.encode_utf16().collect::<Vec<_>>()),
                    );
                    cx.heap_mut().set_member(id, key, name)?;
                }
            }
            table = Value::Obj(ObjRef::bound(id));
            return Ok(NativeStep::Set {
                object: self.owner,
                key: self.member,
                value: table,
                continuation: Box::new(Read {
                    table,
                    key: self.key,
                    metric: self.metric,
                    reverse: self.reverse,
                }),
            });
        }
        Box::new(Read {
            table,
            key: self.key,
            metric: self.metric,
            reverse: self.reverse,
        })
        .resume(cx, Value::Void)
    }
}
struct Read {
    table: Value,
    key: Value,
    metric: bool,
    reverse: bool,
}
impl Trace for Read {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.table.trace(visit);
        self.key.trace(visit);
    }
}
impl NativeContinuation for Read {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let fallback = if self.reverse {
            Value::Str(cx.heap_mut().alloc_string(Vec::new()))
        } else {
            Value::Int(-1)
        };
        Ok(NativeStep::GetRequiredOr {
            object: self.table,
            key: self.key,
            fallback,
            continuation: Box::new(ResultValue {
                metric: self.metric,
                reverse: self.reverse,
            }),
        })
    }
}
struct ResultValue {
    metric: bool,
    reverse: bool,
}
impl Trace for ResultValue {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for ResultValue {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        if self.reverse {
            return Ok(NativeStep::Return(value::to_string(cx.heap_mut(), value)?));
        }
        let number = value::to_integer(cx.heap(), value)? as i32;
        if self.metric {
            if number < 0 {
                return Err(NativeError::Message("unknown system metric"));
            }
            krkr_engine::extensions::desktop_request(
                cx,
                krkr_engine::protocol::window::desktop::Command::Metric(number as u32),
            )
        } else {
            Ok(NativeStep::Return(Value::Int(number.into())))
        }
    }
}

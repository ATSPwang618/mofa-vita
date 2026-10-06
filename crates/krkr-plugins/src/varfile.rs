//! Live global-object storage, following SDL varfile's read and directory rules.
use krkr_engine::{
    plugins::{Context, Plugin},
    storages::managed::{self, Medium, Operation},
};
use std::rc::Rc;
use tjs_core::{
    NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value,
};
#[derive(Default, tjs_bind::Trace)]
pub(crate) struct VarFile {
    #[trace(skip = "Provider is traced by the Storages managed registry")]
    registration: Option<managed::Registration>,
}
krkr_engine::native_plugin! { impl VarFile { names: ["varfile.dll", "varfile.tpm"] } }
impl Plugin for VarFile {
    fn link(&mut self, cx: &mut Context<'_>) -> NativeResult<()> {
        self.registration = Some(managed::register(
            cx.heap,
            "var",
            Rc::new(Variables(cx.global)),
        )?);
        let key = cx
            .heap
            .intern(&krkr_engine::assets::name::units("varfileLoaded"));
        cx.heap.set_member(cx.global, key, Value::Int(1))?;
        Ok(())
    }
    fn unlink(&mut self, _: &mut Context<'_>) -> NativeResult<bool> {
        self.registration = None;
        // The source deliberately leaves varfileLoaded set after unlink.
        Ok(true)
    }
}
#[derive(tjs_bind::Trace)]
struct Variables(ObjId);
impl Medium for Variables {
    fn resolve(
        &self,
        cx: &mut NativeCx<'_>,
        path: Vec<u16>,
        operation: Operation,
        next: Box<dyn NativeContinuation>,
    ) -> NativeResult<NativeStep> {
        let prefix = krkr_engine::assets::name::units("var://./");
        let path = path
            .strip_prefix(prefix.as_slice())
            .ok_or(NativeError::Message("varfile requires the '.' domain"))?;
        let parts = path.split(|&v| v == 47).map(<[u16]>::to_vec).collect();
        Walker {
            parts,
            at: 0,
            operation,
            next,
        }
        .advance(cx, Value::Obj(tjs_core::ObjRef::bound(self.0)))
    }
}
struct Walker {
    parts: Vec<Vec<u16>>,
    at: usize,
    operation: Operation,
    next: Box<dyn NativeContinuation>,
}
impl Trace for Walker {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.next.trace(visit);
    }
}
impl NativeContinuation for Walker {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        self.advance(cx, value)
    }
}
impl Walker {
    fn advance(mut self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        if self.at >= self.parts.len() {
            return self.finish(cx, value);
        }
        let Value::Obj(reference) = value else {
            if matches!(self.operation, Operation::Update) {
                return Err(NativeError::Message("cannot open varfile destination"));
            }
            return self.finish(cx, Value::Void);
        };
        let Some(object) = reference.object else {
            if matches!(self.operation, Operation::Update) {
                return Err(NativeError::Message("cannot open varfile destination"));
            }
            return self.finish(cx, Value::Void);
        };
        let last = self.at + 1 == self.parts.len();
        let part = &self.parts[self.at];
        if last && part.is_empty() {
            if matches!(self.operation, Operation::Update) {
                return Err(NativeError::Message("cannot open varfile destination"));
            }
            return self.finish(cx, value);
        }
        if part.is_empty() {
            return self.finish(cx, Value::Void);
        }
        if last && matches!(self.operation, Operation::Write) {
            // VariantStream's writable memory buffer is discarded on close;
            // opening it checks the parent, without assigning a global member.
            return self.next.resume(cx, Value::Void);
        }
        let text = Value::Str(cx.heap_mut().alloc_string(part.clone()));
        let array = !last && cx.heap().array(object).is_ok();
        self.at += 1;
        if array {
            let key = Value::Int(tjs_core::value::to_integer(cx.heap(), text)? as i32 as i64);
            let missing = cx.heap_mut().alloc_dictionary();
            return Ok(NativeStep::GetOr {
                object: value,
                key,
                raw: false,
                fallback: Value::Obj(missing.into()),
                continuation: Box::new(ArrayDirectory {
                    walker: self,
                    object: value,
                    key: text,
                    missing,
                }),
            });
        }
        Ok(NativeStep::GetOr {
            object: value,
            key: text,
            raw: false,
            fallback: Value::Void,
            continuation: Box::new(self),
        })
    }
    fn finish(self, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        let result = match self.operation {
            Operation::Read => {
                if matches!(value, Value::Octet(_)) {
                    value
                } else {
                    Value::Void
                }
            }
            Operation::Exists => Value::Int(i64::from(matches!(value, Value::Octet(_)))),
            Operation::Update => Value::Void,
            Operation::Write => {
                return Err(NativeError::Message("cannot open varfile destination"));
            }
            Operation::List => {
                let mut names = Vec::new();
                if let Value::Obj(reference) = value
                    && let Some(object) = reference.object
                {
                    // EnumMembers(TJS_IGNOREPROP): don't execute directory getters.
                    for (key, value) in cx.heap().members(object)? {
                        if matches!(value, Value::Octet(_)) {
                            names.push(cx.heap().symbol(key)?.to_vec());
                        }
                    }
                    if let Ok(array) = cx.heap().array(object) {
                        for (i, value) in array.iter().enumerate() {
                            if matches!(value, Value::Octet(_)) {
                                names.push(i.to_string().encode_utf16().collect());
                            }
                        }
                    }
                }
                let values = names
                    .into_iter()
                    .map(|name| Value::Str(cx.heap_mut().alloc_string(name)))
                    .collect::<Vec<_>>();
                Value::Obj(cx.heap_mut().alloc_array_from(&values)?.into())
            }
        };
        self.next.resume(cx, result)
    }
}
#[derive(tjs_bind::Trace)]
struct ArrayDirectory {
    walker: Walker,
    object: Value,
    key: Value,
    missing: ObjId,
}
impl NativeContinuation for ArrayDirectory {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
        if matches!(value, Value::Obj(reference) if reference.object == Some(self.missing)) {
            Ok(NativeStep::GetOr {
                object: self.object,
                key: self.key,
                raw: false,
                fallback: Value::Void,
                continuation: Box::new(self.walker),
            })
        } else {
            self.walker.advance(cx, value)
        }
    }
}

//! ScriptsEx.dll, krkr2@dca72645/cpp/plugins/scriptsEx.cpp.
mod clone;
mod storage;
mod visit;

use crate::exports::{arg, class};
use tjs_core::{
    Heap, MemberFlags, NativeCallable, NativeContinuation, NativeCx, NativeError, NativeResult,
    NativeStep, ObjId, ObjRef, ObjectKind, Trace, Value, value,
};

const ITEMS: usize = 262144;
const BYTES: usize = 16 * 1024 * 1024;
const ENSURE: i64 = 0x200;
const REQUIRED: i64 = 0x400;
const RAW: i64 = 0x800;
const HIDDEN: i64 = 0x1000;
const STATIC: i64 = 0x10000;

krkr_engine::native_plugin! {
    pub(crate) ScriptsEx {
        names: ["ScriptsEx.dll", "ScriptsEx.tpm"],
        link(cx, exports) {
        use NativeCallable::{Leaf, Resumable};
        let scripts = class(cx, "Scripts")?;
        for (name, call) in [
            ("getObjectKeys", Resumable(keys)),
            ("getObjectCount", Leaf(count)),
            ("getObjectContext", Leaf(context)),
            ("isNullContext", Leaf(null_context)),
            ("equalStruct", Resumable(equal)),
            ("equalStructNumericLoose", Resumable(equal_loose)),
            ("foreach", Resumable(visit::start)),
            ("getMD5HashString", Resumable(md5)),
            ("clone", Resumable(clone::start)),
            ("propGet", Resumable(prop_get)),
            ("propSet", Resumable(prop_set)),
            ("safeEvalStorage", Resumable(storage::start)),
            ("rehash", rehash::CALL),
        ] {
            let function = cx.heap.alloc_native_function(call);
            exports
                .value_with_flags(cx, scripts, name, Value::Obj(function.into()), false, true)?;
        }
        for (name, value) in [
            ("pfMemberEnsure", ENSURE),
            ("pfMemberMustExist", REQUIRED),
            ("pfIgnoreProp", RAW),
            ("pfHiddenMember", HIDDEN),
            ("pfStaticMember", STATIC),
        ] {
            exports
                .value_with_flags(cx, scripts, name, Value::Int(value), false, true)?;
        }
        Ok(())
        }
    }
}
fn closure(value: Value) -> NativeResult<ObjRef> {
    match value {
        Value::Obj(r) => Ok(r),
        _ => Err(NativeError::Type("an object closure")),
    }
}
fn live(heap: &Heap, source: Value) -> NativeResult<Option<ObjId>> {
    let Some(object) = closure(source)?.object else {
        return Ok(None);
    };
    Ok(heap.is_valid(object)?.then_some(object))
}
fn instance(cx: &mut NativeCx<'_>, source: Value, name: &str) -> NativeResult<bool> {
    if live(cx.heap(), source)?.is_none() {
        return Ok(false);
    }
    let name = text(cx, name);
    Ok(value::instance_of(cx.heap_mut(), source, name)?)
}
fn text(cx: &mut NativeCx<'_>, value: &str) -> Value {
    Value::Str(
        cx.heap_mut()
            .alloc_string(value.encode_utf16().collect::<Vec<_>>()),
    )
}
fn error(e: impl std::fmt::Display) -> NativeError {
    NativeError::Detail(e.to_string())
}
fn context(_: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let this = closure(arg(args, 0)?)?.this;
    Ok(Value::Obj(ObjRef { object: this, this }))
}
fn null_context(_: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    Ok(Value::Int(i64::from(
        closure(arg(args, 0)?)?.this.is_none(),
    )))
}
fn count(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let source = arg(args, 0)?;
    if !cx.result_needed() {
        return Ok(Value::Void);
    }
    let count = match live(cx.heap(), source)? {
        Some(object) => cx.heap().all_members_with_flags(object)?.count(),
        None => 0, // Do not return the reference's uninitialized stack integer.
    };
    Ok(Value::Int(i64::from(count as i32)))
}
fn flags(heap: &Heap, value: Option<Value>, default: i64) -> NativeResult<MemberFlags> {
    let bits = value
        .map(|v| value::to_integer(heap, v))
        .transpose()?
        .unwrap_or(default) as i32 as i64;
    Ok(MemberFlags {
        ensure: bits & ENSURE != 0,
        must_exist: bits & REQUIRED != 0,
        ignore_property: bits & RAW != 0,
        hidden: bits & HIDDEN != 0,
        class_only: bits & STATIC != 0,
    })
}
fn key(value: Value) -> NativeResult<Value> {
    match value {
        Value::Int(i) => Ok(Value::Int(i64::from(i as i32))),
        Value::Str(_) => Ok(value),
        _ => Err(NativeError::Type("an Integer or String property name")),
    }
}
fn prop_get(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    arg(args, 1)?;
    let object = Value::Obj(closure(args[0])?);
    let flags = flags(cx.heap(), args.get(2).copied(), REQUIRED)?;
    Ok(NativeStep::GetProperty {
        object,
        key: key(args[1])?,
        flags,
        continuation: tjs_bind::flow::identity(),
    })
}
fn prop_set(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let value = arg(args, 2)?;
    let object = Value::Obj(closure(args[0])?);
    let flags = flags(cx.heap(), args.get(3).copied(), ENSURE)?;
    Ok(NativeStep::SetProperty {
        object,
        key: key(args[1])?,
        value,
        flags,
        continuation: tjs_bind::flow::identity(),
    })
}
fn equal(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    crate::ksupport::scripts_compare(cx, args, false)
}
fn equal_loose(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    crate::ksupport::scripts_compare(cx, args, true)
}
#[tjs_bind::function]
fn rehash() -> NativeResult<Value> {
    // TJSDoRehash only requests a rebuild of the old custom hash tables.
    // Rust's managed maps maintain their buckets on mutation; no game state,
    // getter, finalizer, or object identity changes for this deprecated hint.
    Ok(Value::Void)
}

// Snapshots own only bounded names. Consumers read each current raw slot when
// visited, allowing earlier callbacks to mutate later entries.
fn names(heap: &Heap, source: Value) -> NativeResult<Vec<Vec<u16>>> {
    let Some(object) = live(heap, source)? else {
        return Ok(vec![]);
    };
    if heap.object(object)?.kind() == ObjectKind::Array {
        return Ok(vec![]);
    } // Array::EnumMembers = NOTIMPL.
    let mut result = Vec::new();
    let mut units = 0usize;
    for (key, _, _, _) in heap.all_members_with_flags(object)? {
        let name = heap.symbol(key)?;
        units = units.saturating_add(name.len());
        if result.len() >= ITEMS || units > BYTES / 2 {
            return Err(NativeError::Message("ScriptsEx enumeration exceeds limit"));
        }
        result.push(name.to_vec());
    }
    Ok(result)
}
fn member(
    cx: &mut NativeCx<'_>,
    source: Value,
    name: &[u16],
) -> NativeResult<Option<(Value, bool, bool)>> {
    let Some(object) = live(cx.heap(), source)? else {
        return Ok(None);
    };
    let key = cx.heap_mut().intern(name);
    Ok(cx.heap().member_with_flags(object, key)?)
}
fn length(cx: &NativeCx<'_>, value: Value) -> NativeResult<i32> {
    let n = value::to_integer(cx.heap(), value)? as i32;
    if n > ITEMS as i32 {
        return Err(NativeError::Message("ScriptsEx array exceeds item limit"));
    }
    Ok(n.max(0))
}
fn read(
    cx: &mut NativeCx<'_>,
    source: Value,
    key: Value,
    raw: bool,
    fallback: Value,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    if live(cx.heap(), source)?.is_none() {
        return Ok(tjs_bind::flow::deliver(fallback, next));
    }
    Ok(NativeStep::GetOr {
        object: source,
        key,
        raw,
        fallback,
        continuation: next,
    })
}
fn keys(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let source = arg(args, 0)?;
    if !cx.result_needed() {
        return Ok(NativeStep::Return(Value::Void));
    }
    keys_for(cx, source, tjs_bind::flow::identity())
}
pub(crate) fn keys_for(
    cx: &mut NativeCx<'_>,
    source: Value,
    next: Box<dyn NativeContinuation>,
) -> NativeResult<NativeStep> {
    let names = names(cx.heap(), source)?.into_iter();
    let array = cx.heap_mut().alloc_array();
    Ok(NativeStep::Continue(Box::new(Keys {
        source,
        array,
        names,
        next,
        sorted: false,
    })))
}
struct Keys {
    source: Value,
    array: ObjId,
    names: std::vec::IntoIter<Vec<u16>>,
    next: Box<dyn NativeContinuation>,
    sorted: bool,
}
impl Trace for Keys {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.source.trace(v);
        self.array.trace(v);
        self.next.trace(v);
    }
}
impl NativeContinuation for Keys {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if self.sorted {
            return self.next.resume(cx, Value::Obj(ObjRef::bound(self.array)));
        }
        for _ in 0..64 {
            let Some(name) = self.names.next() else {
                self.sorted = true;
                let key = text(cx, "sort");
                return Ok(NativeStep::CallMemberOr {
                    object: Value::Obj(ObjRef::bound(self.array)),
                    key,
                    arguments: vec![],
                    result_needed: false,
                    continuation: self,
                });
            };
            if member(cx, self.source, &name)?.is_some_and(|(_, hidden, _)| !hidden) {
                let name = Value::Str(cx.heap_mut().alloc_string(name));
                let key = text(cx, "add");
                return Ok(NativeStep::CallMemberOr {
                    object: Value::Obj(ObjRef::bound(self.array)),
                    key,
                    arguments: vec![name],
                    result_needed: false,
                    continuation: self,
                });
            }
        }
        Ok(NativeStep::Continue(self))
    }
}
struct Digest {
    input: Value,
    position: usize,
    digest: md5::Context,
}
impl Trace for Digest {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        self.input.trace(v);
    }
}
fn md5(_: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let input = arg(args, 0)?;
    if !matches!(input, Value::Octet(_)) {
        return Err(NativeError::Type("an Octet"));
    }
    Ok(NativeStep::Continue(Box::new(Digest {
        input,
        position: 0,
        digest: md5::Context::new(),
    })))
}
impl NativeContinuation for Digest {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let Value::Octet(id) = self.input else {
            unreachable!()
        };
        let input = cx.heap().octet(id)?;
        let end = input.len().min(self.position.saturating_add(64 * 1024));
        self.digest.consume(&input[self.position..end]);
        self.position = end;
        if end < input.len() {
            return Ok(NativeStep::Continue(self));
        }
        let digest = format!("{:x}", self.digest.finalize());
        Ok(NativeStep::Return(text(cx, &digest)))
    }
}

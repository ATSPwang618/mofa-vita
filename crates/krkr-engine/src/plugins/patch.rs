//! Patch declarations and scoped installation methods. No script is run at registration.
use super::{Context, Identity, key, service};
use std::{cell::RefCell, rc::Rc};
use tjs_core::{
    Heap, NativeCallable, NativeCx, NativeError, NativeResult, NativeStep, ObjId, ObjRef,
    ObjectKind, SymbolId, Trace, Value,
};

pub(super) struct Spec {
    pub replace: bool,
    pub globals: Vec<(SymbolId, Value)>,
    pub on_link: Option<Value>,
    pub on_unlink: Option<Value>,
}
impl Trace for Spec {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        for &(_, value) in &self.globals {
            visit(value);
        }
        self.on_link.trace(visit);
        self.on_unlink.trace(visit);
    }
}

fn object(heap: &Heap, value: Value) -> NativeResult<ObjId> {
    let Value::Obj(ObjRef {
        object: Some(id), ..
    }) = value
    else {
        return Err(NativeError::Type("an object"));
    };
    if !heap.is_valid(id)? {
        return Err(NativeError::Message("patch target is invalid"));
    }
    Ok(id)
}
fn dictionary(heap: &Heap, value: Value) -> NativeResult<ObjId> {
    let id = object(heap, value)?;
    if heap.object(id)?.kind() != ObjectKind::Dictionary {
        return Err(NativeError::Type("a Dictionary"));
    }
    Ok(id)
}
fn callable(heap: &Heap, value: Value) -> NativeResult<Value> {
    let id = object(heap, value)?;
    if !matches!(
        heap.object(id)?.kind(),
        ObjectKind::Function | ObjectKind::NativeFunction
    ) {
        return Err(NativeError::Type("a function"));
    }
    Ok(value)
}
fn units(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let Value::Str(id) = tjs_core::value::to_string(cx.heap_mut(), value)? else {
        unreachable!()
    };
    Ok(cx.heap().string(id)?.to_vec())
}
fn plugin_name(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<Vec<u16>> {
    let text = units(cx, value)?;
    if text.is_empty() || text.contains(&0) {
        return Err(NativeError::Message("plugin name is empty or contains NUL"));
    }
    Ok(key(&text))
}
fn parse(heap: &Heap, value: Value, replace: bool) -> NativeResult<(i64, Spec)> {
    let mut priority = 100;
    let mut spec = Spec {
        replace,
        globals: Vec::new(),
        on_link: None,
        on_unlink: None,
    };
    if matches!(value, Value::Void) {
        return Ok((priority, spec));
    }
    let id = dictionary(heap, value)?;
    for (key, value) in heap.members(id)? {
        let field = heap.symbol(key)?;
        if field == "priority".encode_utf16().collect::<Vec<_>>() {
            let Value::Int(number) = value else {
                return Err(NativeError::Type("a positive integer priority"));
            };
            if number <= 0 {
                return Err(NativeError::Message("patch priority must be positive"));
            }
            priority = number;
        } else if field == "globals".encode_utf16().collect::<Vec<_>>() {
            spec.globals = heap.members(dictionary(heap, value)?)?.collect();
        } else if field == "onLink".encode_utf16().collect::<Vec<_>>() {
            if !matches!(value, Value::Void) {
                spec.on_link = Some(callable(heap, value)?);
            }
        } else if field == "onUnlink".encode_utf16().collect::<Vec<_>>() {
            if !matches!(value, Value::Void) {
                spec.on_unlink = Some(callable(heap, value)?);
            }
        } else {
            return Err(NativeError::Detail(format!(
                "unknown plugin patch field: {}",
                String::from_utf16_lossy(field)
            )));
        }
    }
    Ok((priority, spec))
}

pub(super) fn register(
    cx: &mut NativeCx<'_>,
    name: Value,
    value: Value,
    replace: bool,
) -> NativeResult<()> {
    let name = plugin_name(cx, name)?;
    let (priority, spec) = parse(cx.heap(), value, replace)?;
    let registry = service(cx)?;
    super::lifecycle::recover(cx.heap_mut(), &registry)?;
    let mut r = registry.borrow_mut();
    if r.busy {
        return Err(NativeError::Message("cannot register during a plugin hook"));
    }
    let id = if let Some(&id) = r.names.get(&name) {
        id
    } else {
        let id = r.providers.len();
        r.providers.push(Identity::default());
        r.names.insert(name, id);
        id
    };
    let provider = &mut r.providers[id];
    if provider.active.is_some() {
        return Err(NativeError::Message("cannot change a loaded plugin"));
    }
    if provider.patches.contains_key(&priority) {
        return Err(NativeError::Message("duplicate plugin patch priority"));
    }
    provider.patches.insert(priority, Rc::new(spec));
    Ok(())
}

pub(super) fn alias(cx: &mut NativeCx<'_>, name: Value, target: Value) -> NativeResult<()> {
    let name = plugin_name(cx, name)?;
    let target = plugin_name(cx, target)?;
    let registry = service(cx)?;
    super::lifecycle::recover(cx.heap_mut(), &registry)?;
    let mut r = registry.borrow_mut();
    if r.busy {
        return Err(NativeError::Message("cannot register during a plugin hook"));
    }
    if r.names.contains_key(&name) {
        return Err(NativeError::Message("plugin alias already exists"));
    }
    let id = *r
        .names
        .get(&target)
        .ok_or(NativeError::Message("plugin alias target does not exist"))?;
    if r.providers[id].active.is_some() {
        return Err(NativeError::Message("cannot change a loaded plugin"));
    }
    r.names.insert(name, id);
    Ok(())
}

pub(super) struct Session {
    global: ObjId,
    pub pending: super::context::Pending,
    open: bool,
}
impl Trace for Session {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        visit(Value::Obj(self.global.into()));
        for &(owner, _, value) in &self.pending {
            visit(Value::Obj(owner.into()));
            if let Some(value) = value {
                visit(value.0);
            }
        }
    }
}
#[derive(Clone)]
struct Handle(Rc<RefCell<Session>>);
impl Trace for Handle {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.0.borrow().trace(visit);
    }
}
pub(super) struct Scope {
    handle: Handle,
    pub value: Value,
}
impl Trace for Scope {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.handle.trace(visit);
        visit(self.value);
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        let mut session = self.handle.0.borrow_mut();
        session.open = false;
        session.pending.clear();
    }
}
impl Scope {
    pub(super) fn new(heap: &mut Heap, global: ObjId, spec: &Spec) -> NativeResult<Self> {
        let handle = Handle(Rc::new(RefCell::new(Session {
            global,
            pending: Vec::new(),
            open: true,
        })));
        let owner = heap.alloc_object();
        for (name, call) in [
            ("export", export as tjs_core::NativeThunk),
            ("extend", extend),
            ("hook", hook),
        ] {
            let function = heap.alloc_native_function(NativeCallable::Leaf(call));
            heap.initialize_native_state(function, handle.clone())?;
            let key = heap.intern_str(name);
            heap.set_member(owner, key, Value::Obj(function.into()))?;
        }
        let mut cx = Context::new(heap, global);
        for &(key, value) in &spec.globals {
            stage(&mut cx, global, key, value)?;
        }
        handle.0.borrow_mut().pending = cx.exports;
        Ok(Self {
            handle,
            value: Value::Obj(ObjRef::bound(owner)),
        })
    }
    pub(super) fn commit(&self, heap: &mut Heap) -> NativeResult<super::journal::Journal> {
        let mut session = self.handle.0.borrow_mut();
        let mut cx = Context::new(heap, session.global);
        cx.exports = std::mem::take(&mut session.pending);
        let journal = super::journal::Journal::capture(&cx)?;
        cx.commit()?;
        session.open = false;
        Ok(journal)
    }
}

fn arg(args: &[Value], index: usize) -> NativeResult<Value> {
    args.get(index).copied().ok_or(NativeError::Missing(index))
}
fn edit(
    cx: &mut NativeCx<'_>,
    f: impl FnOnce(&mut Context<'_>) -> NativeResult<()>,
) -> NativeResult<Value> {
    let function = cx.function().ok_or(NativeError::This)?;
    let handle = cx
        .heap_mut()
        .with_native_state::<Handle, _>(function, |h| h.clone())?;
    let mut session = handle.0.borrow_mut();
    if !session.open {
        return Err(NativeError::Message(
            "plugin installation context is closed",
        ));
    }
    let mut context = Context::new(cx.heap_mut(), session.global);
    context.exports = std::mem::take(&mut session.pending);
    let result = f(&mut context);
    session.pending = context.exports;
    result?;
    Ok(Value::Void)
}
fn stage(cx: &mut Context<'_>, owner: ObjId, key: SymbolId, value: Value) -> NativeResult<()> {
    let (_, hidden, class_only) = cx
        .member(owner, key)?
        .unwrap_or((Value::Void, false, false));
    cx.stage_key(owner, key, Some((value, hidden, class_only)))
}
fn export(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let name = units(cx, arg(args, 0)?)?;
    let key = cx.heap_mut().intern(&name);
    let value = arg(args, 1)?;
    edit(cx, |cx| stage(cx, cx.global, key, value))
}
fn extend(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let owner = object(cx.heap(), arg(args, 0)?)?;
    let members = dictionary(cx.heap(), arg(args, 1)?)?;
    let entries = cx.heap().members(members)?.collect::<Vec<_>>();
    edit(cx, |cx| {
        for (key, value) in entries {
            stage(cx, owner, key, value)?;
        }
        Ok(())
    })
}
fn hook(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<Value> {
    let owner = object(cx.heap(), arg(args, 0)?)?;
    let name = units(cx, arg(args, 1)?)?;
    let key = cx.heap_mut().intern(&name);
    let handler = callable(cx.heap(), arg(args, 2)?)?;
    edit(cx, |cx| {
        let previous = cx
            .member(owner, key)?
            .ok_or(NativeError::Message("hook member does not exist"))?
            .0;
        callable(cx.heap, previous)?;
        let function = cx
            .heap
            .alloc_native_function(NativeCallable::Resumable(invoke_hook));
        cx.heap
            .initialize_native_state(function, Hook { previous, handler })?;
        stage(cx, owner, key, Value::Obj(function.into()))
    })
}
#[derive(Clone, tjs_bind::Trace)]
struct Hook {
    previous: Value,
    handler: Value,
}
fn receiver(mut value: Value, this: ObjId) -> Value {
    if let Value::Obj(reference) = &mut value {
        reference.this = reference.this.or(Some(this));
    }
    value
}
fn invoke_hook(cx: &mut NativeCx<'_>, args: &[Value]) -> NativeResult<NativeStep> {
    let function = cx.function().ok_or(NativeError::This)?;
    let hook = cx
        .heap_mut()
        .with_native_state::<Hook, _>(function, |h| h.clone())?;
    let mut arguments = Vec::with_capacity(args.len() + 1);
    arguments.push(receiver(hook.previous, cx.this()));
    arguments.extend_from_slice(args);
    let function = receiver(hook.handler, cx.this());
    let continuation = tjs_bind::flow::identity();
    Ok(if cx.result_needed() {
        NativeStep::Call {
            function,
            arguments,
            continuation,
        }
    } else {
        NativeStep::CallDiscard {
            function,
            arguments,
            continuation,
        }
    })
}

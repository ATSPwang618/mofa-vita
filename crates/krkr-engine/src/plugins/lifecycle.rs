//! Same-VM plugin loading. Only explicit script hooks create script frames.
use super::*;
use std::collections::VecDeque;
use tjs_core::{NativeContinuation, NativeTryContinuation};

enum Action {
    Begin { id: usize, dependencies: Vec<usize> },
    Native(usize),
    Patch(usize, Rc<patch::Spec>),
    Finish(usize, Vec<u16>),
}
fn resolve(r: &Registry, name: &[u16]) -> NativeResult<usize> {
    let exact = r.names.get(&key(name)).copied();
    // Games may name their bundled DLL through a relative or absolute path.
    // Only portable native providers participate in basename fallback; explicit
    // path aliases/patches keep precedence, and an unknown DLL still fails.
    let native_basename = || {
        let at = name
            .iter()
            .rposition(|&c| c == b'/' as u16 || c == b'\\' as u16)?;
        let id = *r.names.get(&key(&name[at + 1..]))?;
        r.providers[id].native.is_some().then_some(id)
    };
    exact.or_else(native_basename).ok_or_else(|| {
        NativeError::Detail(format!(
            "cannot load plugin: {}",
            String::from_utf16_lossy(name)
        ))
    })
}
fn plan(
    r: &Registry,
    name: Vec<u16>,
    visiting: &mut BTreeSet<usize>,
    ready: &mut BTreeSet<usize>,
    actions: &mut VecDeque<Action>,
) -> NativeResult<()> {
    if r.loaded.iter().any(|loaded| loaded.name == name) {
        return Ok(());
    }
    let id = resolve(r, &name)?;
    let provider = &r.providers[id];
    if ready.contains(&id) || provider.active.as_ref().is_some_and(|a| a.complete) {
        actions.push_back(Action::Finish(id, name));
        return Ok(());
    }
    if !visiting.insert(id) {
        return Err(NativeError::Message("cyclic plugin dependency"));
    }
    let mut patches = Vec::new();
    let mut native = true;
    for patch in provider.patches.values().rev() {
        patches.push(patch.clone());
        if patch.replace {
            native = false;
            break;
        }
    }
    let mut dependencies = Vec::new();
    if native {
        let base = provider.native.as_ref().ok_or_else(|| {
            NativeError::Detail(format!(
                "plugin patch requires a lower provider: {}",
                String::from_utf16_lossy(&name)
            ))
        })?;
        for dependency in base.borrow().dependencies() {
            let name = dependency.encode_utf16().collect::<Vec<_>>();
            dependencies.push(resolve(r, &name)?);
            plan(r, name, visiting, ready, actions)?;
        }
    }
    actions.push_back(Action::Begin { id, dependencies });
    if native {
        actions.push_back(Action::Native(id));
    }
    for patch in patches.into_iter().rev() {
        actions.push_back(Action::Patch(id, patch));
    }
    actions.push_back(Action::Finish(id, name));
    visiting.remove(&id);
    ready.insert(id);
    Ok(())
}

fn enter(cx: &mut NativeCx<'_>, link: bool) -> NativeResult<(Shared, ObjId, Busy)> {
    let registry = service(cx)?;
    recover(cx.heap_mut(), &registry)?;
    let global = {
        let mut r = registry.borrow_mut();
        if r.busy {
            return Err(NativeError::Message(
                "plugin registration is already in progress",
            ));
        }
        let global = r
            .global
            .ok_or(NativeError::Message("Plugins requires an engine global"))?;
        if !cx.heap().is_valid(global)? {
            return Err(NativeError::Message("plugin global is invalid"));
        }
        r.busy = true;
        global
    };
    let guard = Busy {
        registry: registry.clone(),
        link,
    };
    Ok((registry, global, guard))
}

pub(super) fn link(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
    let name = name(cx, value)?;
    let (registry, global, guard) = enter(cx, true)?;
    let mut actions = VecDeque::new();
    plan(
        &registry.borrow(),
        name,
        &mut BTreeSet::new(),
        &mut BTreeSet::new(),
        &mut actions,
    )?;
    Ok(NativeStep::Try {
        task: Box::new(Link {
            registry,
            global,
            actions,
            pending: None,
        }),
        continuation: Box::new(Linked { guard, global }),
    })
}
struct Link {
    registry: Shared,
    global: ObjId,
    actions: VecDeque<Action>,
    pending: Option<(usize, Rc<patch::Spec>, patch::Scope)>,
}
impl Trace for Link {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.registry.borrow().trace(visit);
        for action in &self.actions {
            if let Action::Patch(_, spec) = action {
                spec.trace(visit);
            }
        }
        if let Some((_, spec, scope)) = &self.pending {
            spec.trace(visit);
            scope.trace(visit);
        }
    }
}
impl Link {
    fn commit_patch(&mut self, heap: &mut Heap) -> NativeResult<()> {
        if let Some((id, spec, scope)) = self.pending.take() {
            let exports = scope.commit(heap)?;
            self.registry.borrow_mut().providers[id]
                .active
                .as_mut()
                .expect("loading identity")
                .patches
                .push(Installed { spec, exports });
        }
        Ok(())
    }
}
impl NativeContinuation for Link {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.commit_patch(cx.heap_mut())?;
        while let Some(action) = self.actions.pop_front() {
            match action {
                Action::Begin { id, dependencies } => {
                    let mut r = self.registry.borrow_mut();
                    r.linking = Some(id);
                    if r.providers[id].active.is_none() {
                        r.serial += 1;
                        r.providers[id].active = Some(Active {
                            serial: r.serial,
                            ..Active::default()
                        });
                    }
                    r.providers[id]
                        .active
                        .as_mut()
                        .expect("loading identity")
                        .dependencies = dependencies;
                }
                Action::Native(id) => {
                    let provider = {
                        let r = self.registry.borrow();
                        let active = r.providers[id].active.as_ref().expect("loading identity");
                        if active.native.is_some() {
                            if !active.native_complete {
                                return Err(NativeError::Message(
                                    "failed native plugin must be unlinked before retry",
                                ));
                            }
                            continue;
                        }
                        r.providers[id]
                            .native
                            .clone()
                            .expect("planned native provider")
                    };
                    let mut context = Context::new(cx.heap_mut(), self.global);
                    // A native link can retain resources before returning an error.
                    // Mark it for cleanup before invoking the provider.
                    self.registry.borrow_mut().providers[id]
                        .active
                        .as_mut()
                        .expect("loading identity")
                        .native = Some(journal::Journal::default());
                    provider.borrow_mut().link(&mut context)?;
                    let journal = journal::Journal::capture(&context)?;
                    context.commit()?;
                    let mut registry = self.registry.borrow_mut();
                    let active = registry.providers[id]
                        .active
                        .as_mut()
                        .expect("loading identity");
                    active.native = Some(journal);
                    active.native_complete = true;
                }
                Action::Patch(id, spec) => {
                    let scope = patch::Scope::new(cx.heap_mut(), self.global, &spec)?;
                    let function = spec.on_link;
                    let argument = scope.value;
                    self.pending = Some((id, spec, scope));
                    if let Some(function) = function {
                        return Ok(NativeStep::Call {
                            function,
                            arguments: vec![argument],
                            continuation: self,
                        });
                    }
                    self.commit_patch(cx.heap_mut())?;
                }
                Action::Finish(id, name) => {
                    let mut r = self.registry.borrow_mut();
                    r.providers[id]
                        .active
                        .as_mut()
                        .expect("loaded identity")
                        .complete = true;
                    if !r.loaded.iter().any(|p| p.name == name) {
                        r.loaded.push(Loaded { name, provider: id });
                    }
                    r.linking = None;
                }
            }
        }
        Ok(NativeStep::Return(Value::Void))
    }
}
struct Linked {
    guard: Busy,
    global: ObjId,
}
impl Trace for Linked {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.guard.trace(visit);
    }
}
impl NativeTryContinuation for Linked {
    fn resume(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Result<Value, Value>,
    ) -> NativeResult<NativeStep> {
        if let Err(error) = result {
            let id = self.guard.registry.borrow_mut().linking.take();
            if let Some(id) = id {
                // Preserve the original script exception even if external native
                // cleanup fails. Its still-live provider remains tracked for unlink.
                if let Err(cleanup) = abort(cx.heap_mut(), &self.guard.registry, self.global, id) {
                    krkr_protocol::log!(Warn, "plugin load cleanup: {cleanup}");
                }
            }
            return Ok(NativeStep::Throw(error));
        }
        Ok(NativeStep::Return(Value::Void))
    }
}

fn blocked(r: &Registry, id: usize) -> bool {
    let Some(active) = &r.providers[id].active else {
        return false;
    };
    r.providers.iter().enumerate().any(|(other_id, provider)| {
        if other_id == id {
            return false;
        }
        let Some(other) = &provider.active else {
            return false;
        };
        other.dependencies.contains(&id)
            || (other.serial > active.serial
                && active
                    .journals()
                    .any(|a| other.journals().any(|b| a.overlaps(b))))
    })
}
fn native_ready(
    heap: &mut Heap,
    registry: &Shared,
    global: ObjId,
    id: usize,
) -> NativeResult<bool> {
    let native = {
        let r = registry.borrow();
        if blocked(&r, id) {
            return Ok(false);
        }
        r.providers[id]
            .active
            .as_ref()
            .filter(|a| a.native.is_some())
            .and(r.providers[id].native.clone())
    };
    if let Some(native) = native {
        return native.borrow().can_unlink(&Context::new(heap, global));
    }
    Ok(true)
}
pub(super) fn unlink(cx: &mut NativeCx<'_>, value: Value) -> NativeResult<NativeStep> {
    let name = name(cx, value)?;
    let (registry, global, guard) = enter(cx, false)?;
    let id = {
        let mut r = registry.borrow_mut();
        if let Some(index) = r.loaded.iter().position(|p| p.name == name) {
            let id = r.loaded[index].provider;
            if r.loaded.iter().filter(|p| p.provider == id).count() > 1 {
                r.loaded.remove(index);
                return Ok(NativeStep::Return(Value::Int(1)));
            }
            id
        } else {
            let id = resolve(&r, &name)?;
            // A failed patch can have an in-use native base. It is deliberately
            // absent from getList, but can still be explicitly released.
            if !r.providers[id].active.as_ref().is_some_and(|a| !a.complete) {
                return Err(NativeError::Message("plugin is not loaded"));
            }
            id
        }
    };
    if !native_ready(cx.heap_mut(), &registry, global, id)? {
        return Ok(NativeStep::Return(Value::Int(0)));
    }
    let callbacks = registry.borrow().providers[id]
        .active
        .as_ref()
        .expect("loaded identity")
        .patches
        .iter()
        .rev()
        .filter_map(|p| p.spec.on_unlink)
        .collect();
    Box::new(Unlink {
        guard,
        global,
        id,
        callbacks,
        waiting: false,
    })
    .resume(cx, Value::Void)
}
struct Unlink {
    guard: Busy,
    global: ObjId,
    id: usize,
    callbacks: VecDeque<Value>,
    waiting: bool,
}
impl Trace for Unlink {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.guard.trace(visit);
        for &function in &self.callbacks {
            visit(function);
        }
    }
}
impl NativeContinuation for Unlink {
    fn resume(
        mut self: Box<Self>,
        cx: &mut NativeCx<'_>,
        result: Value,
    ) -> NativeResult<NativeStep> {
        if self.waiting && matches!(result, Value::Int(0)) {
            return Ok(NativeStep::Return(Value::Int(0)));
        }
        if let Some(function) = self.callbacks.pop_front() {
            self.waiting = true;
            return Ok(NativeStep::Call {
                function,
                arguments: Vec::new(),
                continuation: self,
            });
        }
        let registry = self.guard.registry.clone();
        if !native_ready(cx.heap_mut(), &registry, self.global, self.id)? {
            return Ok(NativeStep::Return(Value::Int(0)));
        }
        let mut context = Context::new(cx.heap_mut(), self.global);
        let native = {
            let r = registry.borrow();
            let active = r.providers[self.id]
                .active
                .as_ref()
                .expect("loaded identity");
            for patch in active.patches.iter().rev() {
                patch.exports.restore(&mut context)?;
            }
            // The native provider retains its existing unlink policy (some
            // deliberately leave constants on retained class objects). Exports
            // reads the pending patch restoration before deciding slot ownership.
            active
                .native
                .as_ref()
                .and(r.providers[self.id].native.clone())
        };
        context.validate()?;
        if let Some(native) = native
            && !native.borrow_mut().unlink(&mut context)?
        {
            return Ok(NativeStep::Return(Value::Int(0)));
        }
        context.commit()?;
        let mut r = registry.borrow_mut();
        r.providers[self.id].active = None;
        r.loaded.retain(|p| p.provider != self.id);
        Ok(NativeStep::Return(Value::Int(1)))
    }
}

fn abort(heap: &mut Heap, registry: &Shared, global: ObjId, id: usize) -> NativeResult<()> {
    let mut context = Context::new(heap, global);
    {
        let r = registry.borrow();
        let Some(active) = &r.providers[id].active else {
            return Ok(());
        };
        for patch in active.patches.iter().rev() {
            patch.exports.restore(&mut context)?;
        }
    }
    context.commit()?;
    let native = {
        let mut r = registry.borrow_mut();
        let active = r.providers[id].active.as_mut().expect("aborted identity");
        active.patches.clear();
        active.complete = false;
        let linked = active.native.is_some();
        linked.then(|| r.providers[id].native.clone().expect("native base"))
    };
    let mut context = Context::new(heap, global);
    if let Some(native) = native {
        if !native.borrow().can_unlink(&context)? {
            return Ok(());
        }
        if let Some(exports) = &registry.borrow().providers[id]
            .active
            .as_ref()
            .expect("aborted identity")
            .native
        {
            exports.restore(&mut context)?;
        }
        context.validate()?;
        if !native.borrow_mut().unlink(&mut context)? {
            return Ok(());
        }
    }
    context.commit()?;
    registry.borrow_mut().providers[id].active = None;
    Ok(())
}

pub(super) fn recover(heap: &mut Heap, registry: &Shared) -> NativeResult<()> {
    // Cancelled native continuations cannot borrow Heap from Drop. The engine
    // drains their owned journals immediately after cancelling the scheduler.
    loop {
        let next = {
            let mut r = registry.borrow_mut();
            if r.busy {
                return Ok(());
            }
            r.abandoned
                .pop()
                .map(|id| (id, r.global.expect("loading global")))
        };
        let Some((id, global)) = next else {
            return Ok(());
        };
        abort(heap, registry, global, id)?;
    }
}

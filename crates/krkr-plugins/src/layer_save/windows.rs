//! Background saves keep script values on the VM thread. Worker notifications
//! are delivered through owned continuous hooks, not Win32 window messages.
use super::*;
use krkr_image::export::Job;
use std::cell::RefCell;
#[derive(Default, Clone)]
struct Jobs(Rc<RefCell<Queue>>);
impl Trace for Jobs {
    fn trace(&self, v: &mut dyn FnMut(Value)) {
        let q = self.0.borrow();
        for s in q.slots.iter().flatten() {
            s.layer.trace(v);
            s.filename.trace(v);
        }
        q.hook.trace(v);
    }
}
#[derive(Default)]
struct Queue {
    slots: Vec<Option<Slot>>,
    hook: Option<extensions::Continuous>,
}
struct Slot {
    token: Rc<()>,
    layer: Value,
    filename: Value,
    job: Option<Job>,
    lease: Option<Lease>,
    path: Option<std::path::PathBuf>,
    canceled: bool,
    percent: u8,
}
impl Queue {
    fn remove(&mut self, id: usize, token: &Rc<()>) {
        if self
            .slots
            .get(id)
            .and_then(Option::as_ref)
            .is_some_and(|s| Rc::ptr_eq(&s.token, token))
        {
            self.slots[id] = None;
        }
        if self.slots.iter().all(Option::is_none) {
            self.hook = None;
        }
    }
    fn clear(&mut self) {
        self.slots.clear();
        self.hook = None;
    }
}
#[derive(Default, tjs_bind::Trace)]
pub(super) struct State {
    jobs: Jobs,
}
fn jobs(cx: &mut NativeCx<'_>) -> NativeResult<Jobs> {
    let owner = cx.this();
    extensions::window_id(cx, Value::Obj(owner.into()))?;
    cx.heap_mut().initialize_native_default::<State>(owner)?;
    cx.heap_mut()
        .with_native_state::<State, _>(owner, |s| s.jobs.clone())
}
#[derive(tjs_bind::Trace)]
pub(super) struct Pending {
    owner: tjs_core::ObjId,
    shared: Jobs,
    id: usize,
    #[trace(skip = "Generation identity contains no script values")]
    token: Rc<()>,
    committed: bool,
}
impl Pending {
    fn live(&self) -> bool {
        self.shared
            .0
            .borrow()
            .slots
            .get(self.id)
            .and_then(Option::as_ref)
            .is_some_and(|s| Rc::ptr_eq(&s.token, &self.token))
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        if !self.committed {
            self.shared.0.borrow_mut().remove(self.id, &self.token);
        }
    }
}
#[derive(tjs_bind::Trace)]
struct Start {
    pending: Option<Pending>,
    lease: Option<Lease>,
    source: Value,
    name: Vec<u16>,
    tags: Value,
    layer: Value,
    class: Value,
    phase: u8,
}
#[tjs_bind::function(resumable = true)]
pub(super) fn start(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<NativeStep> {
    if args.len() < 3 {
        return Err(NativeError::Missing(2));
    }
    let shared = jobs(cx)?;
    let name = text(cx, args[1])?;
    let filename = Value::Str(cx.heap_mut().alloc_string(name.clone()));
    let token = Rc::new(());
    let id = {
        let mut q = shared.0.borrow_mut();
        if q.hook.as_ref().is_some_and(|h| !h.active()) {
            q.clear();
        }
        let id = q
            .slots
            .iter()
            .position(Option::is_none)
            .unwrap_or(q.slots.len());
        if id == q.slots.len() {
            q.slots.push(None);
        }
        q.slots[id] = Some(Slot {
            token: token.clone(),
            layer: Value::Void,
            filename,
            job: None,
            lease: None,
            path: None,
            canceled: false,
            percent: 0,
        });
        id
    };
    let owner = cx.this();
    let class = Value::Obj(
        cx.heap()
            .registered_class("Layer")
            .ok_or(NativeError::This)?
            .into(),
    );
    let pending = Pending {
        owner,
        shared,
        id,
        token,
        committed: false,
    };
    let lease = capture(cx)?.lease();
    Box::new(Start {
        pending: Some(pending),
        lease: Some(lease),
        source: args[0],
        name,
        tags: args[2],
        layer: Value::Void,
        class,
        phase: 0,
    })
    .resume(cx, Value::Void)
}
impl NativeContinuation for Start {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, v: Value) -> NativeResult<NativeStep> {
        let p = self.pending.as_ref().unwrap();
        if !p.live() || !cx.heap().is_valid(p.owner)? {
            return Err(NativeError::Message("save window became invalid"));
        }
        let window = Value::Obj(ObjRef::bound(p.owner));
        match self.phase {
            0 => {
                self.phase = 1;
                let key = key(cx, "primaryLayer");
                Ok(NativeStep::Get {
                    object: window,
                    key,
                    continuation: self,
                })
            }
            1 => {
                self.phase = 2;
                Ok(NativeStep::Construct {
                    class: self.class,
                    arguments: vec![window, v],
                    continuation: self,
                })
            }
            2 => {
                self.layer = v;
                self.pending.as_ref().unwrap().shared.0.borrow_mut().slots[p.id]
                    .as_mut()
                    .unwrap()
                    .layer = v;
                self.phase = 3;
                let name: Vec<u16> = "saveLayer:"
                    .encode_utf16()
                    .chain(self.name.iter().copied())
                    .collect();
                let value = Value::Str(cx.heap_mut().alloc_string(name));
                let key = key(cx, "name");
                Ok(NativeStep::Set {
                    object: v,
                    key,
                    value,
                    continuation: self,
                })
            }
            3 => {
                self.phase = 4;
                let key = key(cx, "assignImages");
                Ok(NativeStep::Get {
                    object: self.class,
                    key,
                    continuation: self,
                })
            }
            4 => {
                self.phase = 5;
                let Value::Obj(mut function) = v else {
                    return Err(NativeError::Type("Layer.assignImages function"));
                };
                function.this = Some(crate::exports::object(self.layer)?);
                Ok(NativeStep::CallDiscard {
                    function: Value::Obj(function),
                    arguments: vec![self.source],
                    continuation: self,
                })
            }
            _ => {
                let name = String::from_utf16_lossy(&self.name);
                let mode = if name
                    .rsplit('.')
                    .next()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
                {
                    Mode::BackgroundPng
                } else {
                    Mode::BackgroundTlg
                };
                let layer = self.layer;
                let encode = Encoding {
                    owner: layer,
                    name: Some(self.name.clone()),
                    tags: if matches!(self.tags, Value::Obj(_)) {
                        Some(self.tags)
                    } else {
                        None
                    },
                    mode,
                    lease: self.lease.take().unwrap(),
                    background: self.pending.take(),
                };
                pixels::read(cx, layer, Box::new(encode))
            }
        }
    }
}
pub(super) fn started(
    cx: &mut NativeCx<'_>,
    mut p: Pending,
    lease: Lease,
    job: Job,
    path: Option<std::path::PathBuf>,
) -> NativeResult<NativeStep> {
    validate(cx, &p)?;
    let mut q = p.shared.0.borrow_mut();
    if q.hook.is_none() {
        let function = cx.heap_mut().alloc_native_function(tick::CALL);
        let function = Value::Obj(ObjRef {
            object: Some(function),
            this: Some(p.owner),
        });
        q.hook = Some(extensions::continuous(cx, function)?);
    }
    let slot = q.slots[p.id].as_mut().unwrap();
    if slot.canceled {
        job.cancel();
    }
    slot.job = Some(job);
    slot.lease = Some(lease);
    slot.path = path;
    p.committed = true;
    Ok(NativeStep::Return(Value::Int(p.id as i64)))
}
pub(super) fn validate(cx: &NativeCx<'_>, p: &Pending) -> NativeResult<()> {
    if p.live() && cx.heap().is_valid(p.owner)? {
        Ok(())
    } else {
        Err(NativeError::Message("save window became invalid"))
    }
}
fn cancel_job(cx: &mut NativeCx<'_>, args: &[Value], stop: bool) -> NativeResult<NativeStep> {
    let n = value::to_integer(cx.heap(), crate::exports::arg(args, 0)?)? as i32;
    let shared = jobs(cx)?;
    let mut q = shared.0.borrow_mut();
    if n >= 0
        && let Some(s) = q.slots.get_mut(n as usize).and_then(Option::as_mut)
    {
        s.canceled = true;
        if let Some(job) = &s.job {
            job.cancel();
        }
        if stop {
            let token = s.token.clone();
            q.remove(n as usize, &token);
        }
    }
    Ok(NativeStep::Return(Value::Void))
}
#[tjs_bind::function(resumable = true)]
pub(super) fn cancel(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<NativeStep> {
    cancel_job(cx, args, false)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn stop(
    cx: &mut NativeCx<'_>,
    args: tjs_bind::RestArgs<'_>,
) -> NativeResult<NativeStep> {
    cancel_job(cx, args, true)
}
#[tjs_bind::function(resumable = true)]
pub(super) fn cleanup(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    jobs(cx)?.0.borrow_mut().clear();
    Ok(NativeStep::Return(Value::Void))
}

#[derive(tjs_bind::Trace)]
struct Event {
    owner: tjs_core::ObjId,
    shared: Jobs,
    #[trace(
        skip = "Completed worker job contains no script handles; arguments keep snapshot Layer alive"
    )]
    slot: Option<Slot>,
    arguments: Vec<Value>,
    error: Option<String>,
    complete: bool,
}
impl Drop for Event {
    fn drop(&mut self) {
        if !self.complete {
            self.shared.0.borrow_mut().clear();
        }
    }
}
impl NativeContinuation for Event {
    fn resume(mut self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.complete = true;
        if let Some(error) = self.error.take() {
            Err(NativeError::Detail(error))
        } else {
            Ok(NativeStep::Return(Value::Void))
        }
    }
}
#[tjs_bind::function(resumable = true)]
fn tick(cx: &mut NativeCx<'_>) -> NativeResult<NativeStep> {
    let owner = cx.this();
    let shared = jobs(cx)?;
    let mut q = shared.0.borrow_mut();
    let mut event = None;
    for (id, slot) in q.slots.iter_mut().enumerate() {
        let Some(s) = slot else {
            continue;
        };
        let Some(job) = &s.job else {
            continue;
        };
        let percent = job.progress();
        let args = |v: Value| vec![Value::Int(id as i64), v, s.layer, s.filename];
        if percent != s.percent {
            let arguments = args(Value::Int(i64::from(percent)));
            s.percent = percent;
            event = Some(("onSaveLayerImageProgress", arguments, None, None));
            break;
        }
        if let Some(result) = job.take_result() {
            let canceled = s.canceled || result.is_err();
            let arguments = args(Value::Int(i64::from(canceled)));
            let error = if s.canceled { None } else { result.err() };
            event = Some(("onSaveLayerImageDone", arguments, slot.take(), error));
            break;
        }
    }
    if q.slots.iter().all(Option::is_none) {
        q.hook = None;
    }
    drop(q);
    let Some((name, arguments, slot, error)) = event else {
        return Ok(NativeStep::Return(Value::Void));
    };
    if let Some(path) = slot.as_ref().and_then(|slot| slot.path.as_ref()) {
        krkr_engine::storages::service(cx)?
            .borrow_mut()
            .invalidate_file(path);
    }
    let key = key(cx, name);
    let next = Box::new(Event {
        owner,
        shared,
        slot,
        arguments: arguments.clone(),
        error,
        complete: false,
    });
    Ok(NativeStep::CallMemberOr {
        object: Value::Obj(ObjRef::bound(owner)),
        key,
        arguments,
        result_needed: false,
        continuation: next,
    })
}

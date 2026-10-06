use super::*;
use crate::{
    io,
    operations::{Operations, Request},
};
use tjs_core::WaitMode;

pub(super) enum Phase {
    OpenStop(Vec<u16>),
    OpenClear(Vec<u16>),
    OpenRead(Vec<u16>),
    OpenPlans(crate::storages::ReadPlan, Option<crate::storages::ReadPlan>),
    OpenReady(io::Delivery),
    OpenCommit(krkr_audio::Handle),
    Play,
    Playing,
    Stop,
    Seek(u64),
    FadeStop(i32, i32, i32),
    FadeStart(i32, i32, i32),
    StopFade,
    Done,
}
struct Task {
    shared: Shared,
    id: SoundId,
    owner: ObjId,
    phase: Phase,
}
impl Trace for Task {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
    }
}
pub(super) fn start(
    lease: &Lease,
    cx: &mut NativeCx<'_>,
    phase: Phase,
) -> NativeResult<NativeStep> {
    let owner = lease.shared.borrow().record(lease.id)?.owner;
    Box::new(Task {
        shared: lease.shared.clone(),
        id: lease.id,
        owner,
        phase,
    })
    .resume(cx, Value::Void)
}
impl Task {
    fn event(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        name: &str,
        args: Vec<Value>,
    ) -> NativeResult<NativeStep> {
        Ok(NativeStep::CallMember {
            object: Value::Obj(self.owner.into()),
            key: text(cx.heap_mut(), name),
            arguments: args,
            continuation: self,
        })
    }
    fn status(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        status: &'static str,
    ) -> NativeResult<NativeStep> {
        let mut world = self.shared.borrow_mut();
        let record = world.record_mut(self.id)?;
        if record.status == status {
            drop(world);
            return self.resume(cx, Value::Void);
        }
        record.status = status;
        record.events.clear();
        let source = record.source;
        world.events.borrow_mut().cancel(source);
        drop(world);
        let arg = text(cx.heap_mut(), status);
        self.event(cx, "onStatusChanged", vec![arg])
    }
}
impl NativeContinuation for Task {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        self.shared.borrow().record(self.id)?;
        match std::mem::replace(&mut self.phase, Phase::Done) {
            Phase::OpenStop(path) => {
                let w = self.shared.borrow();
                let r = w.record(self.id)?;
                if let Some(h) = &r.handle {
                    h.stop();
                }
                let loaded = r.status != "unload";
                drop(w);
                self.phase = Phase::OpenClear(path);
                if loaded {
                    self.status(cx, "stop")
                } else {
                    self.resume(cx, Value::Void)
                }
            }
            Phase::OpenClear(path) => {
                let mut w = self.shared.borrow_mut();
                let r = w.record_mut(self.id)?;
                r.handle = None;
                r.active_filters.clear();
                r.paused = false;
                drop(w);
                self.phase = Phase::OpenRead(path);
                self.status(cx, "unload")
            }
            Phase::OpenRead(path) => {
                let mut sli = path.clone();
                sli.extend(".sli".encode_utf16());
                crate::storages::managed::plans(
                    cx,
                    vec![(sli, false), (path, true)],
                    self,
                    |mut task, cx, mut plans| {
                        let plan = plans.pop().flatten().expect("required audio plan");
                        let sli = plans.pop().flatten();
                        task.phase = Phase::OpenPlans(plan, sli);
                        task.resume(cx, Value::Void)
                    },
                )
            }
            Phase::OpenPlans(plan, sli) => {
                let filters = filter::snapshot(cx.heap_mut(), &self.shared, self.id)?;
                let delivery = io::Delivery::default();
                let backend = self.shared.borrow().backend.clone();
                let operations = self.shared.borrow().operations.clone();
                self.phase = Phase::OpenReady(delivery.clone());
                Operations::wait(
                    &operations,
                    Request::Read(
                        Box::new(io::Work::AudioOpen(plan, sli, backend, filters)),
                        delivery,
                    ),
                    WaitMode::Internal,
                    self,
                )
            }
            Phase::OpenReady(delivery) => {
                let Some(io::Data::Audio(handle)) = delivery.borrow_mut().take() else {
                    return Err(NativeError::Message("missing audio completion"));
                };
                let old = self.shared.borrow_mut().record_mut(self.id)?.labels.take();
                self.phase = Phase::OpenCommit(handle);
                if let Some(old) = old {
                    Ok(NativeStep::Invalidate {
                        object: Value::Obj(old.into()),
                        continuation: self,
                    })
                } else {
                    self.resume(cx, Value::Void)
                }
            }
            Phase::OpenCommit(handle) => {
                let mut w = self.shared.borrow_mut();
                let r = w.record_mut(self.id)?;
                r.frequency = handle.format().rate as i32;
                handle.looping(r.looping);
                handle.pause(r.paused);
                if r.use_vis_buffer {
                    handle.visualization(true).map_err(NativeError::Detail)?;
                }
                r.handle = Some(handle);
                w.update_gain(self.id);
                drop(w);
                self.status(cx, "stop")
            }
            Phase::Play => {
                let w = self.shared.borrow();
                let r = w.record(self.id)?;
                let Some(h) = r.handle.clone() else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                let operations = w.operations.clone();
                drop(w);
                self.phase = Phase::Playing;
                Operations::wait(
                    &operations,
                    Request::Read(Box::new(io::Work::AudioPlay(h)), io::Delivery::default()),
                    WaitMode::Internal,
                    self,
                )
            }
            Phase::Playing => self.status(cx, "play"),
            Phase::Stop => {
                let w = self.shared.borrow();
                let r = w.record(self.id)?;
                if let Some(h) = &r.handle {
                    h.stop();
                }
                let loaded = r.status != "unload";
                drop(w);
                self.phase = Phase::Seek(0);
                if loaded {
                    self.status(cx, "stop")
                } else {
                    self.resume(cx, Value::Void)
                }
            }
            Phase::Seek(position) => {
                let w = self.shared.borrow();
                let h = w.record(self.id)?.handle.clone();
                let operations = w.operations.clone();
                drop(w);
                let Some(h) = h else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                Operations::wait(
                    &operations,
                    Request::Read(
                        Box::new(io::Work::AudioSeek(h, position)),
                        io::Delivery::default(),
                    ),
                    WaitMode::Internal,
                    self,
                )
            }
            Phase::FadeStop(to, time, delay) => {
                let had_fade = self
                    .shared
                    .borrow_mut()
                    .record_mut(self.id)?
                    .fade
                    .take()
                    .is_some();
                self.phase = Phase::FadeStart(to, time, delay);
                if had_fade {
                    self.event(cx, "onFadeCompleted", Vec::new())
                } else {
                    self.resume(cx, Value::Void)
                }
            }
            Phase::FadeStart(to, time, delay) => {
                let mut w = self.shared.borrow_mut();
                let r = w.record_mut(self.id)?;
                r.fade = Some(Fade {
                    target: to,
                    delta: to.wrapping_sub(r.volume).wrapping_mul(60) / time,
                    count: time / 60,
                    blank: delay,
                });
                drop(w);
                if time < 60 && delay == 0 {
                    self.phase = Phase::StopFade;
                    self.resume(cx, Value::Void)
                } else {
                    Ok(NativeStep::Return(Value::Void))
                }
            }
            Phase::StopFade => {
                let mut w = self.shared.borrow_mut();
                let r = w.record_mut(self.id)?;
                let Some(fade) = r.fade.take() else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                r.volume = fade.target.clamp(0, 100000);
                w.update_gain(self.id);
                drop(w);
                self.event(cx, "onFadeCompleted", Vec::new())
            }
            Phase::Done => Ok(NativeStep::Return(Value::Void)),
        }
    }
}

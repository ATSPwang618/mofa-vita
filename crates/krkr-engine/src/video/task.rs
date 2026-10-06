use super::*;
use crate::{
    io,
    operations::{Operations, Request},
};
use tjs_core::WaitMode;
pub(super) enum Phase {
    Open(Vec<u16>),
    Read(Vec<u16>),
    ReadPlan(crate::storages::ReadPlan),
    Opened(io::Delivery),
    Graphics,
    Play,
    Pause,
    Stop,
    Close,
    Seek(f64, Box<Phase>),
    Audio(i64),
    AudioReady(i64),
    Video(i64),
    VideoReady,
    Prepare,
    Prepared,
    PreparedPause,
    Loop(f64, i64),
    Event(&'static str, Vec<Value>),
    Error(String),
    Done,
}
impl Trace for Phase {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        match self {
            Self::Event(_, args) => args.trace(visit),
            Self::Seek(_, next) => next.trace(visit),
            _ => {}
        }
    }
}
pub(super) struct Event {
    pub id: VideoId,
    pub owner: ObjId,
    pub phase: Phase,
    pub generation: u64,
}
impl Trace for Event {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.phase.trace(visit);
    }
}
impl NativeContinuation for Event {
    fn resume(self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        let shared = bindings::service(cx)?;
        Box::new(Task {
            shared,
            id: self.id,
            owner: self.owner,
            generation: self.generation,
            phase: self.phase,
        })
        .resume(cx, Value::Void)
    }
}
struct Task {
    shared: Shared,
    id: VideoId,
    owner: ObjId,
    generation: u64,
    phase: Phase,
}
impl Trace for Task {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.phase.trace(visit);
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        if let Ok(mut world) = self.shared.try_borrow_mut()
            && let Ok(r) = world.record_mut(self.id)
            && r.generation == self.generation
        {
            r.busy = false;
        }
    }
}
pub(super) fn start(
    lease: &Lease,
    cx: &mut NativeCx<'_>,
    phase: Phase,
) -> NativeResult<NativeStep> {
    let mut world = lease.shared.borrow_mut();
    let r = world.record_mut(lease.id)?;
    r.generation += 1;
    r.events.clear();
    r.busy = true;
    r.rendered_frame = None;
    let generation = r.generation;
    let owner = r.owner;
    let source = r.source;
    world.events.borrow_mut().cancel(source);
    drop(world);
    Box::new(Task {
        shared: lease.shared.clone(),
        id: lease.id,
        owner,
        generation,
        phase,
    })
    .resume(cx, Value::Void)
}
impl Task {
    fn playback(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        playing: bool,
        status: &'static str,
    ) -> NativeResult<NativeStep> {
        let handle = self.shared.borrow().record(self.id)?.handle.clone();
        if let Some(handle) = handle {
            self.shared.borrow().backend.trace(match status {
                "play" => "engine: play requested",
                "pause" => "engine: pause requested",
                _ => "engine: stop requested",
            });
            handle.play(playing);
            self.status(cx, status)
        } else {
            Ok(NativeStep::Return(Value::Void))
        }
    }
    fn event(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        name: &'static str,
        arguments: Vec<Value>,
    ) -> NativeResult<NativeStep> {
        Ok(NativeStep::CallMember {
            object: Value::Obj(self.owner.into()),
            key: text(cx.heap_mut(), name),
            arguments,
            continuation: self,
        })
    }
    fn status(
        self: Box<Self>,
        cx: &mut NativeCx<'_>,
        status: &'static str,
    ) -> NativeResult<NativeStep> {
        let mut world = self.shared.borrow_mut();
        let r = world.record_mut(self.id)?;
        if r.status == status {
            drop(world);
            return self.resume(cx, Value::Void);
        }
        r.status = status;
        r.events.clear();
        let source = r.source;
        world.events.borrow_mut().cancel(source);
        drop(world);
        let argument = text(cx.heap_mut(), status);
        self.event(cx, "onStatusChanged", vec![argument])
    }
    fn read(self: Box<Self>, work: io::Work, delivery: io::Delivery) -> NativeResult<NativeStep> {
        let operations = self.shared.borrow().operations.clone();
        Operations::wait(
            &operations,
            Request::Read(Box::new(work), delivery),
            WaitMode::Internal,
            self,
        )
    }
}
impl NativeContinuation for Task {
    fn resume(mut self: Box<Self>, cx: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        {
            let w = self.shared.borrow();
            let Ok(r) = w.record(self.id) else {
                return Ok(NativeStep::Return(Value::Void));
            };
            if r.generation != self.generation || !w.windows.borrow().is_live(r.window) {
                return Ok(NativeStep::Return(Value::Void));
            }
        }
        match std::mem::replace(&mut self.phase, Phase::Done) {
            Phase::Open(path) => {
                let mut world = self.shared.borrow_mut();
                let old_status = world.record(self.id)?.status;
                world.clear(self.id);
                let r = world.record_mut(self.id)?;
                self.generation = r.generation;
                r.busy = true;
                r.status = old_status;
                drop(world);
                self.phase = Phase::Read(path);
                self.status(cx, "unload")
            }
            Phase::Read(path) => crate::storages::managed::plans(
                cx,
                vec![(path, true)],
                self,
                |mut task, cx, mut plans| {
                    task.phase =
                        Phase::ReadPlan(plans.pop().flatten().expect("required video plan"));
                    task.resume(cx, Value::Void)
                },
            ),
            Phase::ReadPlan(plan) => {
                let path = [
                    plan.name.clone(),
                    krkr_assets::name::units(krkr_image::scale::SUFFIX),
                ]
                .concat();
                self.phase = Phase::ReadPlan(plan);
                crate::storages::managed::plans(
                    cx,
                    vec![(path, false)],
                    self,
                    |mut task, _cx, mut plans| {
                        let Phase::ReadPlan(plan) = std::mem::replace(&mut task.phase, Phase::Done)
                        else {
                            unreachable!()
                        };
                        let backend = task.shared.borrow().backend.clone();
                        let delivery = io::Delivery::default();
                        task.phase = Phase::Opened(delivery.clone());
                        task.read(
                            io::Work::VideoOpen {
                                plan,
                                scale: plans.pop().flatten(),
                                service: backend,
                                silent: false,
                            },
                            delivery,
                        )
                    },
                )
            }
            Phase::Opened(delivery) => {
                let Some(io::Data::Video(handle)) = delivery.borrow_mut().take() else {
                    return Err(NativeError::Message("missing video completion"));
                };
                let mut world = self.shared.borrow_mut();
                world.backend.trace("engine: open completion delivered");
                let window = world.record(self.id)?.window;
                let (images, tickets) = world.layers.borrow_mut().video_open(
                    window,
                    handle.info().size,
                    handle.stored_size(),
                )?;
                let r = world.record_mut(self.id)?;
                handle.gain(r.volume, r.balance);
                handle.rate(r.rate).map_err(NativeError::Detail)?;
                r.audio_stream = if handle.info().audio_streams == 0 {
                    -1
                } else {
                    0
                };
                r.handle = Some(handle);
                r.images = Some(images);
                r.tickets = tickets;
                r.geometry_dirty = true;
                world.backend.trace("engine: awaiting initial graphics");
                drop(world);
                self.phase = Phase::Graphics;
                self.resume(cx, Value::Void)
            }
            Phase::Graphics => {
                let ticket = self
                    .shared
                    .borrow_mut()
                    .record_mut(self.id)?
                    .tickets
                    .pop_front();
                if let Some(ticket) = ticket {
                    let operations = self.shared.borrow().operations.clone();
                    self.phase = Phase::Graphics;
                    Operations::wait(
                        &operations,
                        Request::Window(ticket, crate::window::Delivery::default()),
                        WaitMode::Internal,
                        self,
                    )
                } else {
                    self.shared
                        .borrow()
                        .backend
                        .trace("engine: initial graphics completed");
                    self.status(cx, "stop")
                }
            }
            Phase::Play => self.playback(cx, true, "play"),
            Phase::Pause => self.playback(cx, false, "pause"),
            Phase::Stop => self.playback(cx, false, "stop"),
            Phase::Close => {
                let mut world = self.shared.borrow_mut();
                let old = world.record(self.id)?.status;
                world.clear(self.id);
                let r = world.record_mut(self.id)?;
                self.generation = r.generation;
                r.status = old;
                drop(world);
                self.status(cx, "unload")
            }
            Phase::Seek(time, next) => {
                let mut world = self.shared.borrow_mut();
                let r = world.record_mut(self.id)?;
                let Some(handle) = r.handle.clone() else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                if r.period >= 0 && time * handle.info().fps <= r.period as f64 {
                    r.period_past = false;
                }
                r.rendered_frame = None;
                r.tickets.clear();
                drop(world);
                self.phase = *next;
                self.read(io::Work::VideoSeek(handle, time), io::Delivery::default())
            }
            Phase::Audio(index) => {
                let world = self.shared.borrow();
                let r = world.record(self.id)?;
                let Some(handle) = r.handle.clone() else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                if index < -1 || index >= handle.info().audio_streams as i64 {
                    return Err(NativeError::Message("invalid movie audio stream"));
                }
                drop(world);
                self.phase = Phase::AudioReady(index);
                self.read(
                    io::Work::VideoAudio(handle, (index >= 0).then_some(index as usize)),
                    io::Delivery::default(),
                )
            }
            Phase::AudioReady(index) => {
                self.shared.borrow_mut().record_mut(self.id)?.audio_stream = index;
                self.resume(cx, Value::Void)
            }
            Phase::Video(index) => {
                let mut world = self.shared.borrow_mut();
                let r = world.record_mut(self.id)?;
                let Some(handle) = r.handle.clone() else {
                    return Ok(NativeStep::Return(Value::Void));
                };
                if index < 0 || index >= handle.info().video_streams as i64 {
                    return Err(NativeError::Message("invalid movie video stream"));
                }
                r.tickets.clear();
                r.rendered_frame = None;
                drop(world);
                self.phase = Phase::VideoReady;
                self.read(
                    io::Work::VideoStream(handle, index as usize),
                    io::Delivery::default(),
                )
            }
            Phase::VideoReady => {
                let mut world = self.shared.borrow_mut();
                let r = world.record_mut(self.id)?;
                let window = r.window;
                let size = r.handle.as_ref().ok_or(NativeError::This)?.info().size;
                let old = r.images.take();
                if let Some(old) = old {
                    world.layers.borrow_mut().video_close(window, old);
                }
                let stored = world
                    .record(self.id)?
                    .handle
                    .as_ref()
                    .unwrap()
                    .stored_size();
                let (images, tickets) =
                    world.layers.borrow_mut().video_open(window, size, stored)?;
                let r = world.record_mut(self.id)?;
                r.images = Some(images);
                r.tickets = tickets;
                r.geometry_dirty = true;
                drop(world);
                self.resume(cx, Value::Void)
            }
            Phase::Loop(time, reason) => {
                self.phase = Phase::Seek(
                    time,
                    Box::new(Phase::Event("onPeriod", vec![Value::Int(reason)])),
                );
                self.resume(cx, Value::Void)
            }
            Phase::Prepare => {
                self.shared.borrow_mut().record_mut(self.id)?.preparing = true;
                self.phase = Phase::Seek(0.0, Box::new(Phase::Play));
                self.resume(cx, Value::Void)
            }
            Phase::Prepared => {
                self.phase = Phase::PreparedPause;
                self.event(cx, "onPeriod", vec![Value::Int(2)])
            }
            Phase::PreparedPause => {
                if let Some(handle) = &self.shared.borrow().record(self.id)?.handle {
                    handle.play(false);
                }
                self.phase = Phase::Seek(0.0, Box::new(Phase::Done));
                self.status(cx, "pause")
            }
            Phase::Event(name, arguments) => self.event(cx, name, arguments),
            Phase::Error(error) => {
                self.shared.borrow_mut().clear(self.id);
                Err(NativeError::Detail(error))
            }
            Phase::Done => Ok(NativeStep::Return(Value::Void)),
        }
    }
}

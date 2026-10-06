//! VideoOverlay ownership and events. Media workers never run script code.
mod bindings;
pub(crate) mod extension;
mod task;
use crate::{
    events::{self, Kind, SourceId},
    layer::video::Images,
    operations,
};
use krkr_protocol::{
    graphics::{LayerId, Rect},
    window::{Response, Ticket, WindowId},
};
use slotmap::{SlotMap, new_key_type};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::Duration};
use tjs_core::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value,
};
new_key_type! { struct VideoId; }
pub(crate) type Shared = Rc<RefCell<Videos>>;
struct Lease {
    shared: Shared,
    id: VideoId,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.shared.borrow_mut().remove(self.id);
    }
}
enum Posted {
    Status(&'static str),
    Frame(i64),
    Period(i64),
    Loop(f64, i64),
    Prepare,
    Error(String),
}
struct Record {
    owner: ObjId,
    window: WindowId,
    source: SourceId,
    handle: Option<krkr_video::Handle>,
    images: Option<Images>,
    tickets: VecDeque<Ticket>,
    rendered_frame: Option<i64>,
    status: &'static str,
    mode: i64,
    bounds: Rect,
    visible: bool,
    targets: [Option<LayerId>; 2],
    target_values: [Value; 2],
    looping: bool,
    segment: (i64, i64),
    period: i64,
    period_past: bool,
    preparing: bool,
    busy: bool,
    geometry_dirty: bool,
    volume: i32,
    balance: i32,
    rate: f64,
    audio_stream: i64,
    events: VecDeque<Posted>,
    generation: u64,
}
pub(crate) struct Videos {
    pub backend: krkr_video::Service,
    operations: operations::Shared,
    events: events::Shared,
    windows: crate::window::Shared,
    layers: crate::layer::Shared,
    records: SlotMap<VideoId, Record>,
}
impl Videos {
    fn record(&self, id: VideoId) -> NativeResult<&Record> {
        self.records.get(id).ok_or(NativeError::This)
    }
    fn record_mut(&mut self, id: VideoId) -> NativeResult<&mut Record> {
        self.records.get_mut(id).ok_or(NativeError::This)
    }
    fn clear(&mut self, id: VideoId) {
        if let Some(r) = self.records.get_mut(id) {
            if let Some(h) = r.handle.take() {
                h.play(false);
            }
            r.tickets.clear();
            r.rendered_frame = None;
            r.events.clear();
            r.busy = false;
            r.preparing = false;
            r.generation += 1;
            r.status = "unload";
            self.events.borrow_mut().cancel(r.source);
            if let Some(images) = r.images.take() {
                self.layers.borrow_mut().video_close(r.window, images);
            }
        }
    }
    fn remove(&mut self, id: VideoId) {
        self.clear(id);
        if let Some(r) = self.records.remove(id) {
            self.events.borrow_mut().remove(r.source);
        }
    }
    pub fn reset(&mut self) {
        let ids: Vec<_> = self.records.keys().collect();
        for id in ids {
            self.clear(id);
        }
    }
    pub fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        self.records
            .values()
            .filter(|r| r.status == "play" || r.busy)
            .map(|r| Value::Obj(r.owner.into()))
    }
    pub fn sleep_duration(&self) -> Option<Duration> {
        self.records
            .values()
            .any(|r| r.handle.is_some())
            .then_some(Duration::from_millis(5))
    }
    pub fn advance(&mut self) {
        let mut dead = Vec::new();
        for (id, r) in &mut self.records {
            if !self.windows.borrow().is_live(r.window) {
                if r.handle.is_some() || !r.events.is_empty() {
                    dead.push(id);
                }
                continue;
            }
            let Some(handle) = r.handle.clone() else {
                continue;
            };
            if r.busy {
                continue;
            }
            let result = (|| -> NativeResult<()> {
                if let Some(error) = handle.error() {
                    return Err(NativeError::Detail(error));
                }
                while let Some(ticket) = r.tickets.front() {
                    let Some(result) = ticket.take() else {
                        return Ok(());
                    };
                    if !matches!(result.map_err(NativeError::Detail)?, Response::Done) {
                        return Err(NativeError::Message("unexpected movie upload response"));
                    }
                    r.tickets.pop_front();
                }
                if let Some(frame) = r.rendered_frame.take() {
                    if r.mode != 0 {
                        post(&self.events, r, Posted::Frame(frame));
                    }
                    if r.preparing {
                        if post(&self.events, r, Posted::Prepare) {
                            r.busy = true;
                            r.preparing = false;
                        }
                        return Ok(());
                    }
                    if r.period >= 0
                        && !r.period_past
                        && frame >= r.period
                        && post(&self.events, r, Posted::Period(1))
                    {
                        r.period = -1;
                    }
                }
                let frame_number = (handle.position() * handle.info().fps).floor() as i64;
                if r.status == "play" && r.segment.1 > 0 && frame_number >= r.segment.1 {
                    if post(
                        &self.events,
                        r,
                        Posted::Loop(r.segment.0.max(0) as f64 / handle.info().fps, 3),
                    ) {
                        r.busy = true;
                    }
                    return Ok(());
                }
                let frame = handle.frame();
                if frame.is_some() || r.geometry_dirty {
                    let number = frame
                        .as_ref()
                        .map(|f| (f.time * handle.info().fps).round() as i64);
                    let pixels = frame.map(|f| f.pixels);
                    let overlay = (r.mode != 1).then_some((r.bounds, r.visible));
                    if let Some(images) = &mut r.images {
                        r.tickets = self
                            .layers
                            .borrow_mut()
                            .video_frame(r.window, images, pixels, r.targets, overlay)?;
                        r.rendered_frame = number;
                        r.geometry_dirty = false;
                    }
                    return Ok(());
                }
                if r.status == "play" && handle.finished() {
                    if r.looping {
                        if post(&self.events, r, Posted::Loop(0.0, 0)) {
                            r.busy = true;
                        }
                    } else if post(&self.events, r, Posted::Status("stop")) {
                        handle.play(false);
                        r.status = "stop";
                    }
                }
                Ok(())
            })();
            if let Err(error) = result
                && post(&self.events, r, Posted::Error(error.to_string()))
            {
                handle.play(false);
                r.status = "stop";
                r.busy = true;
            }
        }
        for id in dead {
            self.clear(id);
        }
    }
    pub fn callback(
        &mut self,
        source: SourceId,
        heap: &mut Heap,
    ) -> Option<Box<dyn NativeContinuation>> {
        let (id, r) = self.records.iter_mut().find(|(_, r)| r.source == source)?;
        let event = r.events.pop_front()?;
        let phase = match event {
            Posted::Loop(time, reason) => task::Phase::Loop(time, reason),
            Posted::Prepare => task::Phase::Prepared,
            Posted::Status(status) => {
                task::Phase::Event("onStatusChanged", vec![text(heap, status)])
            }
            Posted::Frame(frame) => task::Phase::Event("onFrameUpdate", vec![Value::Int(frame)]),
            Posted::Period(reason) => task::Phase::Event("onPeriod", vec![Value::Int(reason)]),
            Posted::Error(error) => task::Phase::Error(error),
        };
        Some(Box::new(task::Event {
            id,
            owner: r.owner,
            phase,
            generation: r.generation,
        }))
    }
}
fn post(events: &events::Shared, r: &mut Record, event: Posted) -> bool {
    if r.events.len() >= 8 || events.borrow_mut().post(r.source, 1, 0).is_err() {
        return false;
    }
    r.events.push_back(event);
    true
}
fn text(heap: &mut Heap, text: &str) -> Value {
    Value::Str(heap.alloc_string(text.encode_utf16().collect::<Vec<_>>()))
}
pub(crate) fn install(
    heap: &mut Heap,
    operations: operations::Shared,
    events: events::Shared,
    windows: crate::window::Shared,
    layers: crate::layer::Shared,
    audio: krkr_audio::Service,
) -> NativeResult<Shared> {
    let shared = Rc::new(RefCell::new(Videos {
        backend: krkr_video::Service::new(audio),
        operations,
        events,
        windows,
        layers,
        records: SlotMap::with_key(),
    }));
    bindings::install(heap, shared.clone())?;
    Ok(shared)
}

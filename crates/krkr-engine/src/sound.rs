//! Script sound state and events. Decoder/device ownership stays in audio services.
mod bindings;
mod filter;
pub(crate) mod visualization;
pub(crate) fn register_decoder(
    heap: &mut Heap,
    kind: krkr_audio::Codec,
) -> NativeResult<krkr_audio::Registration> {
    bindings::register_decoder(heap, kind)
}
mod flags;
mod task;
use crate::{
    events::{self, Kind, SourceId},
    operations,
};
use slotmap::{SlotMap, new_key_type};
use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::Duration};
use tjs_core::{
    Heap, NativeContinuation, NativeCx, NativeError, NativeResult, NativeStep, ObjId, Trace, Value,
};
use tjs_runtime::clock::Clock;
new_key_type! { struct SoundId; }
pub(crate) type Shared = Rc<RefCell<Sounds>>;
struct Lease {
    shared: Shared,
    id: SoundId,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.shared.borrow_mut().remove(self.id);
    }
}
struct Fade {
    target: i32,
    delta: i32,
    count: i32,
    blank: i32,
}
enum Posted {
    Status(&'static str),
    Fade,
    Error(String),
    Label(String),
}
struct Record {
    filters: ObjId,
    active_filters: Vec<ObjId>,
    flags: Option<ObjId>,
    labels: Option<ObjId>,
    owner: ObjId,
    source: SourceId,
    handle: Option<krkr_audio::Handle>,
    status: &'static str,
    volume: i32,
    volume2: i32,
    pan: i32,
    frequency: i32,
    looping: bool,
    paused: bool,
    use_vis_buffer: bool,
    fade: Option<Fade>,
    events: VecDeque<Posted>,
}
pub(crate) struct Sounds {
    sample_buffers: std::collections::BTreeMap<i64, std::rc::Weak<RefCell<visualization::Storage>>>,
    pub backend: krkr_audio::Service,
    clock: Rc<dyn Clock>,
    operations: operations::Shared,
    events: events::Shared,
    records: SlotMap<SoundId, Record>,
    next_beat: Duration,
    global_volume: i32,
}
impl Sounds {
    fn record(&self, id: SoundId) -> NativeResult<&Record> {
        self.records.get(id).ok_or(NativeError::This)
    }
    fn record_mut(&mut self, id: SoundId) -> NativeResult<&mut Record> {
        self.records.get_mut(id).ok_or(NativeError::This)
    }
    fn remove(&mut self, id: SoundId) {
        if let Some(record) = self.records.remove(id) {
            self.events.borrow_mut().remove(record.source);
        }
    }
    pub fn reset(&mut self) {
        for r in self.records.values_mut() {
            // Existing script objects keep their identity; their media lease
            // is replaced and the flags view now reads the unloaded state.
            r.handle = None;
            r.active_filters.clear();
            r.labels = None;
            r.status = "unload";
            r.fade = None;
            r.paused = false;
            r.events.clear();
            self.events.borrow_mut().cancel(r.source);
        }
    }
    fn update_gain(&self, id: SoundId) {
        if let Some(r) = self.records.get(id)
            && let Some(h) = &r.handle
        {
            let mut gain = (r.volume / 10) * (r.volume2 / 10) / 1000;
            gain = (gain / 10) * (self.global_volume / 10) / 1000;
            h.gain(gain, r.pan);
        }
    }
    pub fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        self.records
            .values()
            .filter(|r| r.status == "play" || r.fade.is_some())
            .map(|r| Value::Obj(r.owner.into()))
    }
    pub fn sleep_duration(&self) -> Option<Duration> {
        let beat = self
            .records
            .values()
            .any(|r| r.status == "play" || r.fade.is_some())
            .then(|| self.next_beat.saturating_sub(self.clock.now()));
        let label = self
            .records
            .values()
            .filter(|r| r.events.len() < 4)
            .filter_map(|r| r.handle.as_ref()?.label_delay())
            .min();
        beat.into_iter().chain(label).min()
    }
    pub fn advance(&mut self) {
        for r in self.records.values_mut() {
            if let Some(h) = &r.handle {
                for _ in 0..32usize.saturating_sub(r.events.len()) {
                    let Some(name) =
                        h.take_label_if(|| self.events.borrow_mut().post(r.source, 1, 0).is_ok())
                    else {
                        break;
                    };
                    r.events.push_back(Posted::Label(name));
                }
            }
        }
        let now = self.clock.now();
        if now < self.next_beat {
            return;
        }
        // Fade and end checks use the reference 60 ms beat. Labels use their
        // own submitted-device deadlines rather than waiting for this beat.
        let elapsed = (now - self.next_beat).as_millis() / 60 + 1;
        let beats = elapsed.min(64) as usize;
        self.next_beat += Duration::from_millis(elapsed as u64 * 60);
        for r in self.records.values_mut() {
            if r.status == "play"
                && let Some(h) = &r.handle
            {
                if let Some(error) = h.error()
                    && self.events.borrow_mut().post(r.source, 1, 0).is_ok()
                {
                    h.stop();
                    r.status = "stop";
                    r.events.push_back(Posted::Error(error));
                } else if h.finished() && self.events.borrow_mut().post(r.source, 1, 0).is_ok() {
                    r.status = "stop";
                    r.events.push_back(Posted::Status("stop"));
                }
            }
            for _ in 0..beats {
                let Some(f) = &mut r.fade else {
                    break;
                };
                if f.blank != 0 {
                    f.blank = f.blank.saturating_sub(60).max(0);
                } else if f.count == 1 && self.events.borrow_mut().post(r.source, 1, 0).is_ok() {
                    r.volume = f.target.clamp(0, 100000);
                    r.fade = None;
                    r.events.push_back(Posted::Fade);
                } else if f.count > 1 {
                    f.count -= 1;
                    r.volume = r.volume.wrapping_add(f.delta).clamp(0, 100000);
                }
            }
        }
        for id in self.records.keys() {
            self.update_gain(id);
        }
    }
    pub fn callback(
        &mut self,
        source: SourceId,
        heap: &mut Heap,
    ) -> Option<Box<dyn NativeContinuation>> {
        let record = self.records.values_mut().find(|r| r.source == source)?;
        let event = record.events.pop_front()?;
        let (name, argument, error) = match event {
            Posted::Status(status) => ("onStatusChanged", Some(text(heap, status)), None),
            Posted::Fade => ("onFadeCompleted", None, None),
            Posted::Error(error) => ("", None, Some(error)),
            Posted::Label(name) => ("onLabel", Some(text(heap, &name)), None),
        };
        Some(Box::new(Callback {
            owner: record.owner,
            name: text(heap, name),
            argument,
            error,
        }))
    }
}
fn text(heap: &mut Heap, text: &str) -> Value {
    Value::Str(heap.alloc_string(text.encode_utf16().collect::<Vec<_>>()))
}
struct Callback {
    owner: ObjId,
    name: Value,
    argument: Option<Value>,
    error: Option<String>,
}
impl Trace for Callback {
    fn trace(&self, visit: &mut dyn FnMut(Value)) {
        self.owner.trace(visit);
        self.name.trace(visit);
        self.argument.trace(visit);
    }
}
impl NativeContinuation for Callback {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        if let Some(error) = self.error {
            return Err(NativeError::Detail(error));
        }
        Ok(NativeStep::CallMember {
            object: Value::Obj(self.owner.into()),
            key: self.name,
            arguments: self.argument.into_iter().collect(),
            continuation: Box::new(Done),
        })
    }
}
struct Done;
impl Trace for Done {
    fn trace(&self, _: &mut dyn FnMut(Value)) {}
}
impl NativeContinuation for Done {
    fn resume(self: Box<Self>, _: &mut NativeCx<'_>, _: Value) -> NativeResult<NativeStep> {
        Ok(NativeStep::Return(Value::Void))
    }
}
pub(crate) fn install(
    heap: &mut Heap,
    clock: Rc<dyn Clock>,
    operations: operations::Shared,
    events: events::Shared,
) -> NativeResult<Shared> {
    let next_beat = clock.now() + Duration::from_millis(60);
    let shared = Rc::new(RefCell::new(Sounds {
        sample_buffers: Default::default(),
        backend: Default::default(),
        clock,
        operations,
        events,
        records: SlotMap::with_key(),
        next_beat,
        global_volume: 100000,
    }));
    bindings::install(heap, shared.clone())?;
    flags::install(heap)?;
    Ok(shared)
}

use serde::{Deserialize, Serialize};
use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::Instant,
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static RECORDER: RwLock<Option<Arc<Recorder>>> = RwLock::new(None);
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
static NEXT_THREAD: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static THREAD: u64 = NEXT_THREAD.fetch_add(1, Ordering::Relaxed);
    static THREAD_SESSION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Thread {
        thread: u64,
        name: String,
    },
    Span {
        thread: u64,
        start_ns: u64,
        duration_ns: u64,
        name: String,
        detail: String,
    },
    Counter {
        thread: u64,
        at_ns: u64,
        name: String,
        value: u64,
    },
    Marker {
        thread: u64,
        at_ns: u64,
        name: String,
        detail: String,
    },
}
struct Recorder {
    id: u64,
    origin: Instant,
    sender: SyncSender<Event>,
    dropped: AtomicU64,
}
impl Recorder {
    fn thread(&self) -> u64 {
        let id = THREAD.with(|t| *t);
        THREAD_SESSION.with(|s| {
            if s.get() != self.id {
                self.send(Event::Thread {
                    thread: id,
                    name: std::thread::current().name().unwrap_or("unnamed").into(),
                });
                s.set(self.id);
            }
        });
        id
    }
    fn now(&self) -> u64 {
        self.origin.elapsed().as_nanos() as u64
    }
    fn send(&self, event: Event) {
        if self.sender.try_send(event).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// One process-wide capture at a time. Stop workers before finishing the session.
pub struct Session {
    recorder: Arc<Recorder>,
}
impl Session {
    pub fn start(capacity: usize) -> Result<(Self, Receiver<Event>), &'static str> {
        let mut slot = RECORDER.write().unwrap();
        if slot.is_some() {
            return Err("a performance recording is already active");
        }
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let recorder = Arc::new(Recorder {
            id: NEXT_SESSION.fetch_add(1, Ordering::Relaxed),
            origin: Instant::now(),
            sender,
            dropped: AtomicU64::new(0),
        });
        *slot = Some(recorder.clone());
        ACTIVE.store(true, Ordering::Release);
        Ok((Self { recorder }, receiver))
    }
    pub fn finish(self) -> u64 {
        self.stop();
        self.recorder.dropped.load(Ordering::Relaxed)
    }
    fn stop(&self) {
        let mut slot = RECORDER.write().unwrap();
        if slot.as_ref().is_some_and(|r| r.id == self.recorder.id) {
            ACTIVE.store(false, Ordering::Release);
            slot.take();
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}
#[inline]
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

#[derive(Clone, Copy)]
pub struct Stamp {
    session: u64,
    thread: u64,
    start_ns: u64,
}
pub fn stamp() -> Option<Stamp> {
    if !active() {
        return None;
    }
    let slot = RECORDER.read().unwrap();
    let recorder = slot.as_ref()?;
    Some(Stamp {
        session: recorder.id,
        thread: recorder.thread(),
        start_ns: recorder.now(),
    })
}
pub fn complete(stamp: Stamp, name: &str, detail: impl FnOnce() -> String) {
    if !active() {
        return;
    }
    let slot = RECORDER.read().unwrap();
    if let Some(recorder) = slot.as_ref().filter(|r| r.id == stamp.session) {
        let duration_ns = recorder.now().saturating_sub(stamp.start_ns);
        recorder.send(Event::Span {
            thread: stamp.thread,
            start_ns: stamp.start_ns,
            duration_ns,
            name: name.into(),
            detail: detail(),
        });
    }
}
pub struct Span {
    stamp: Option<Stamp>,
    name: &'static str,
    detail: Option<String>,
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(stamp) = self.stamp {
            complete(stamp, self.name, || self.detail.take().unwrap_or_default());
        }
    }
}
pub fn span(name: &'static str) -> Span {
    Span {
        stamp: stamp(),
        name,
        detail: None,
    }
}
pub fn span_detail(name: &'static str, detail: impl FnOnce() -> String) -> Span {
    let stamp = stamp();
    Span {
        stamp,
        name,
        detail: stamp.map(|_| detail()),
    }
}
pub fn counter(name: &'static str, value: u64) {
    if !active() {
        return;
    }
    let slot = RECORDER.read().unwrap();
    if let Some(recorder) = slot.as_ref() {
        recorder.send(Event::Counter {
            thread: recorder.thread(),
            at_ns: recorder.now(),
            name: name.into(),
            value,
        });
    }
}
pub fn marker(name: &'static str, detail: impl FnOnce() -> String) {
    if !active() {
        return;
    }
    let slot = RECORDER.read().unwrap();
    if let Some(recorder) = slot.as_ref() {
        recorder.send(Event::Marker {
            thread: recorder.thread(),
            at_ns: recorder.now(),
            name: name.into(),
            detail: detail(),
        });
    }
}

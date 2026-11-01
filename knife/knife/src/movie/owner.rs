//! One persistent decoder owner. Only integer surface descriptors cross threads;
//! the native decoder itself is created, used and destroyed on this service.
use std::{sync::{Arc, atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{Receiver, SyncSender, TryRecvError, TrySendError}}, thread, time::{Duration, Instant}};
use knife_perf::{Metric, Scope};

#[derive(Clone, Copy, Debug)]
pub(super) struct Frame {
    pub token: u64, pub seek_serial: u64, pub pointer: usize, pub width: u32, pub height: u32,
    pub timestamp_ms: u64, pub uv: [f32; 4],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Settings { pub looping: bool, pub loop_ms: u64, pub seek_serial: u64, pub seek_ms: u64 }
pub(super) enum Event { Started, Frame(Frame), Eof, Failed(String) }
pub(super) struct Update<S> { pub settings: Settings, pub audio: Option<S> }

/// 0 = no GPU reader, token<<2|1 = published, |2 = acquired by GXM owner.
/// Only that owner can ACK acquired storage; cancellation may revoke unread
/// descriptors but can never release an acquired surface on its behalf.
#[derive(Default)]
struct Lease(AtomicU64);
impl Lease {
    fn publish(&self, token: u64) { self.0.store(token << 2 | 1, Ordering::Release); }
    fn acquire(&self, token: u64) -> bool {
        self.0.compare_exchange(token << 2 | 1, token << 2 | 2, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
    fn ack(&self, token: u64) -> bool {
        self.0.compare_exchange(token << 2 | 2, 0, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }
    fn revoke_unread(&self) {
        let value = self.0.load(Ordering::Acquire);
        if value & 3 == 1 { let _ = self.0.compare_exchange(value, 0, Ordering::AcqRel, Ordering::Acquire); }
    }
    fn active(&self) -> bool { self.0.load(Ordering::Acquire) != 0 }
}
#[derive(Default)]
struct Shared { cancel: AtomicBool, quarantine: AtomicBool, hold: AtomicBool, seek: AtomicU64, lease: Lease }
pub(super) struct Open<S> {
    pub path: String, settings: Settings, updates: Receiver<Update<S>>, events: SyncSender<Event>, shared: Arc<Shared>,
}
pub(super) struct Session<S> { updates: SyncSender<Update<S>>, events: Receiver<Event>, shared: Arc<Shared> }
impl<S> Session<S> {
    pub fn open(service: &SyncSender<Open<S>>, path: String, settings: Settings) -> Result<Self, String> {
        let (updates, update_rx) = std::sync::mpsc::sync_channel(2);
        let (events, event_rx) = std::sync::mpsc::sync_channel(4);
        let shared = Arc::new(Shared::default());
        service.try_send(Open { path, settings, updates: update_rx, events, shared: shared.clone() })
            .map_err(|_| "movie service unavailable or open queue full".to_string())?;
        Ok(Self { updates, events: event_rx, shared })
    }
    pub fn update(&self, update: Update<S>) -> Result<(), Update<S>> {
        self.updates.try_send(update).map_err(|error| match error { TrySendError::Full(update) | TrySendError::Disconnected(update) => update })
    }
    pub fn poll(&self) -> Result<Option<Event>, String> {
        match self.events.try_recv() { Ok(event) => Ok(Some(event)), Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("movie owner exited".into()) }
    }
    pub fn set_hold(&self, hold: bool) {
        self.shared.hold.store(hold, Ordering::Release);
        if hold { self.shared.lease.revoke_unread(); }
    }
    pub fn note_seek(&self, serial: u64) {
        self.shared.seek.store(serial, Ordering::Release);
        self.shared.lease.revoke_unread();
    }
    pub fn acquire(&self, token: u64) -> bool { self.shared.lease.acquire(token) }
    pub fn ack(&self, token: u64) -> bool { self.shared.lease.ack(token) }
    pub fn quarantine(&self) { self.shared.quarantine.store(true, Ordering::Release); self.cancel(); }
    pub fn cancel(&self) {
        self.shared.cancel.store(true, Ordering::Release); self.shared.lease.revoke_unread();
    }
}
impl<S> Drop for Session<S> { fn drop(&mut self) { self.cancel(); } }

pub(super) trait Audio: Send {
    fn ready(&mut self) -> Result<bool, String>;
    fn submit(&mut self, frames: &[[f32; 2]]) -> Result<(), String>;
}
pub(super) trait Decoder {
    fn poll_start(&mut self) -> Result<bool, String>;
    fn hold(&mut self, hold: bool) -> Result<(), String>;
    fn seek(&mut self, ms: u64) -> Result<(), String>;
    fn video(&mut self) -> Result<Option<Frame>, String>;
    fn audio(&mut self, out: &mut [[f32; 2]; 960]) -> Result<usize, String>;
    fn eof(&mut self) -> Result<bool, String>;
    fn close(&mut self) -> Result<(), String>;
}
fn emit<S>(request: &Open<S>, mut event: Event) -> bool {
    loop {
        if request.shared.cancel.load(Ordering::Acquire) { return false; }
        match request.events.try_send(event) {
            Ok(()) => return true, Err(TrySendError::Disconnected(_)) => return false,
            Err(TrySendError::Full(value)) => event = value,
        }
        thread::sleep(Duration::from_millis(1));
    }
}
fn updates<S>(request: &Open<S>, settings: &mut Settings, sink: &mut Option<S>) {
    for _ in 0..2 {
        let Ok(update) = request.updates.try_recv() else { break; };
        *settings = update.settings;
        if let Some(audio) = update.audio { *sink = Some(audio); }
    }
}
fn changed<S>(request: &Open<S>, serial: u64) -> bool {
    request.shared.cancel.load(Ordering::Acquire) || request.shared.hold.load(Ordering::Acquire)
        || request.shared.seek.load(Ordering::Acquire) != serial
}
fn run<S: Audio, D: Decoder>(request: &Open<S>, decoder: &mut D) -> Result<(), String> {
    let (mut settings, mut sink) = (request.settings, None);
    let started = Instant::now();
    loop {
        if request.shared.cancel.load(Ordering::Acquire) { return Ok(()); }
        updates(request, &mut settings, &mut sink);
        let poll = Scope::new(Metric::MovieOwnerPoll);
        let ready = decoder.poll_start()?;
        poll.finish(0, u64::from(ready));
        if ready { break; }
        if started.elapsed() >= Duration::from_secs(5) { return Err("movie READY deadline (5s)".into()); }
        thread::sleep(Duration::from_millis(2));
    }
    if !emit(request, Event::Started) { return Ok(()); }
    let (mut paused, mut applied_seek, mut next_token) = (false, 0, 1u64);
    'playback: loop {
        if request.shared.cancel.load(Ordering::Acquire) { return Ok(()); }
        updates(request, &mut settings, &mut sink);
        // seek() and its replacement audio sink form one Update. Do not pull
        // old-source PCM between the request and that atomic command batch.
        if request.shared.seek.load(Ordering::Acquire) != settings.seek_serial {
            thread::sleep(Duration::from_millis(1)); continue;
        }
        let hold = request.shared.hold.load(Ordering::Acquire);
        if hold != paused {
            let probe = Scope::new(Metric::MovieOwnerControl); decoder.hold(hold)?; probe.finish(0, 1); paused = hold;
        }
        if settings.seek_serial != applied_seek {
            let probe = Scope::new(Metric::MovieOwnerControl); decoder.seek(settings.seek_ms)?; probe.finish(0, 1);
            applied_seek = settings.seek_serial;
        }
        if hold { thread::sleep(Duration::from_millis(2)); continue; }
        if let Some(audio) = &mut sink {
            for _ in 0..4 {
                if changed(request, settings.seek_serial) { continue 'playback; }
                let ready = audio.ready();
                if changed(request, settings.seek_serial) { continue 'playback; }
                if !ready? { break; }
                let mut chunk = [[0.0; 2]; 960];
                let read = decoder.audio(&mut chunk);
                if changed(request, settings.seek_serial) { continue 'playback; }
                let frames = read?;
                if frames == 0 { break; }
                if frames > chunk.len() { return Err("invalid movie PCM count".into()); }
                let submitted = audio.submit(&chunk[..frames]);
                // seek() precedes replacement of the old mixer voice. That
                // receiver may disconnect during any native pull/submit; a
                // changed control serial retires old PCM, not the new movie.
                if changed(request, settings.seek_serial) { continue 'playback; }
                submitted?;
            }
        }
        if changed(request, settings.seek_serial) { continue; }
        let probe = Scope::new(Metric::MovieOwnerFrame);
        let frame = decoder.video()?;
        probe.finish(0, u64::from(frame.is_some()));
        if let Some(mut frame) = frame {
            if request.shared.cancel.load(Ordering::Acquire) || request.shared.hold.load(Ordering::Acquire) { continue; }
            frame.token = next_token; frame.seek_serial = settings.seek_serial;
            next_token = next_token.wrapping_add(1).max(1);
            request.shared.lease.publish(frame.token);
            if !emit(request, Event::Frame(frame)) { request.shared.lease.revoke_unread(); return Ok(()); }
            // No SDK call (including PCM/control/Close) may revoke this surface
            // until the GXM owner has finished its cache pass and acknowledged.
            while request.shared.lease.active() {
                if request.shared.cancel.load(Ordering::Acquire) || request.shared.hold.load(Ordering::Acquire)
                    || request.shared.seek.load(Ordering::Acquire) != settings.seek_serial {
                    request.shared.lease.revoke_unread();
                }
                if request.shared.quarantine.load(Ordering::Acquire) { return Err("movie GPU lease quarantined".into()); }
                if request.shared.cancel.load(Ordering::Acquire) && request.shared.lease.active() {
                    request.shared.quarantine.store(true, Ordering::Release);
                    return Err("cancelled movie still has an acquired GPU lease".into());
                }
                thread::sleep(Duration::from_millis(1));
            }
        } else if decoder.eof()? {
            if settings.looping {
                let probe = Scope::new(Metric::MovieOwnerControl); decoder.seek(settings.loop_ms)?; probe.finish(0, 1);
            } else { emit(request, Event::Eof); return Ok(()); }
        } else { thread::sleep(Duration::from_millis(2)); }
    }
}
/// D intentionally has no Send bound: the factory builds it inside this thread.
/// A quarantined service never opens another native owner or frees unsafe reads.
pub(super) fn service<S: Audio, D: Decoder>(requests: Receiver<Open<S>>, mut open: impl FnMut(&str) -> Result<D, String>) {
    let mut quarantined = false;
    while let Ok(request) = requests.recv() {
        if request.shared.cancel.load(Ordering::Acquire) { continue; }
        if quarantined { emit(&request, Event::Failed("movie service quarantined after unsafe GPU/native shutdown".into())); continue; }
        let probe = Scope::new(Metric::MovieOwnerOpen);
        let opened = open(&request.path);
        probe.finish_hot(&request.path, 0, u64::from(opened.is_ok()));
        let mut decoder = match opened { Ok(decoder) => decoder, Err(error) => { emit(&request, Event::Failed(error)); continue; } };
        if let Err(error) = run(&request, &mut decoder) { emit(&request, Event::Failed(error)); }
        if request.shared.quarantine.load(Ordering::Acquire) || request.shared.lease.active() {
            quarantined = true; std::mem::forget(decoder); continue;
        }
        let probe = Scope::new(Metric::MovieOwnerClose);
        let closed = decoder.close();
        probe.finish_hot(&request.path, 0, u64::from(closed.is_ok()));
        if let Err(error) = closed {
            emit(&request, Event::Failed(format!("native close failed; owner retained: {error}")));
            quarantined = true; std::mem::forget(decoder);
        }
    }
}

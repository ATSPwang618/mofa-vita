//! Independent bounded stream channels. Decoder/file ownership never leaves
//! its worker; the mixer only polls ready PCM and never joins active IO.
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError}};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use knife_perf::{Gauge, Metric, Scope};

pub(super) const FRAMES: usize = 960;
const CHANNELS: usize = 8;
// Close to the native port's two 5120-frame buffers, without changing the
// mixer's 20ms envelopes or AudioOut format/latency contract.
const QUEUED: usize = 10;
const BLOCKS: usize = QUEUED + 2;
static WORKERS: AtomicUsize = AtomicUsize::new(0);
static NEXT_STREAM: AtomicUsize = AtomicUsize::new(1);

fn stream_label(name: &str) -> Arc<str> {
    // Identity is allocated once per stream, never in sample()/the mix loop.
    // Instance numbers distinguish reopen/seek generations of the same asset.
    let mut label = format!("#{}:", NEXT_STREAM.fetch_add(1, Ordering::Relaxed));
    for c in name.chars() {
        let c = if c.is_control() { '?' } else { c };
        if label.len() + c.len_utf8() > 80 { break; }
        label.push(c);
    }
    label.into()
}

struct WaitEpisode { scope: Scope, missing: u64 }
impl WaitEpisode {
    fn new(metric: Metric) -> Self { Self { scope: Scope::new(metric), missing: 0 } }
    fn finish(self, label: &str) { self.scope.finish_hot_try(label, 0, self.missing); }
}

pub(super) trait Reader {
    fn mono(&self) -> bool;
    fn read(&mut self, out: &mut [[f32; 2]]) -> Result<usize, String>;
}
struct Permit;
impl Permit {
    fn acquire() -> Result<Self, String> {
        WORKERS.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |n| (n < CHANNELS).then_some(n + 1)).map_err(|_| {
                knife_perf::add(Metric::AudioStreamLimit, 0, CHANNELS as u64);
                "independent audio stream channel limit (8)".to_string()
            })?;
        knife_perf::gauge_add(Gauge::AudioStreamChannels, 1);
        Ok(Self)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        WORKERS.fetch_sub(1, Ordering::AcqRel);
        knife_perf::gauge_sub(Gauge::AudioStreamChannels, 1);
    }
}
struct Block { pcm: Box<[[f32; 2]]>, frames: usize, mono: bool }
enum Packet { Data(Block), End, Failed(String) }
struct Channel { _permit: Permit, bytes: AtomicUsize }
impl Drop for Channel {
    fn drop(&mut self) {
        knife_perf::gauge_sub(Gauge::AudioStreamBufferBytes, self.bytes.load(Ordering::Relaxed) as u64);
    }
}
pub(super) enum Poll { Sample([f32; 2], bool), Pending, End, Failed(String) }
pub(super) struct Pump {
    ready: Receiver<Packet>, recycle: SyncSender<Block>, cancel: Arc<AtomicBool>,
    current: Option<Block>, cursor: usize, worker: Option<JoinHandle<()>>,
    started: bool, waiting_reported: bool, terminal: bool, _channel: Arc<Channel>,
    label: Arc<str>, wait: Option<WaitEpisode>,
}
impl Pump {
    pub(super) fn spawn_named<F, R>(name: &str, open: F) -> Result<Self, String>
    where F: FnOnce() -> Result<R, String> + Send + 'static, R: Reader + 'static {
        let channel = Arc::new(Channel { _permit: Permit::acquire()?, bytes: AtomicUsize::new(0) });
        let worker_channel = channel.clone();
        let label = stream_label(name);
        let worker_label = label.clone();
        let wait = Some(WaitEpisode::new(Metric::AudioStreamStartWall));
        let (sender, ready) = mpsc::sync_channel(QUEUED);
        let (recycle, free) = mpsc::sync_channel(BLOCKS);
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        let worker = thread::Builder::new().name("knife-audio-stream".into())
            .stack_size(128 * 1024).spawn(move || {
                let owner = worker_channel;
                let prepare = Scope::new(Metric::AudioStreamPrepare);
                if stopped.load(Ordering::Acquire) { return; }
                let mut reader = match open() {
                    Ok(reader) => reader,
                    Err(error) => {
                        if stopped.load(Ordering::Acquire) { return; }
                        knife_perf::add(Metric::AudioStreamFailed, 0, 1);
                        let _ = sender.send(Packet::Failed(error)); return;
                    }
                };
                prepare.finish_hot(&worker_label, 0, 1);
                if stopped.load(Ordering::Acquire) { return; }
                let mono = reader.mono();
                // Allocate once on the channel worker, not every mixer block.
                let make_pool = || -> Result<Vec<Block>, String> {
                    let mut pool = Vec::new();
                    pool.try_reserve_exact(BLOCKS).map_err(|_| "audio packet pool allocation")?;
                    for _ in 0..BLOCKS {
                        let mut pcm = Vec::new();
                        pcm.try_reserve_exact(FRAMES).map_err(|_| "audio PCM allocation")?;
                        pcm.resize(FRAMES, [0.0; 2]);
                        pool.push(Block { pcm: pcm.into_boxed_slice(), frames: 0, mono });
                    }
                    Ok(pool)
                };
                let mut initial = match make_pool() {
                    Ok(pool) => pool,
                    Err(error) => {
                        if !stopped.load(Ordering::Acquire) {
                            knife_perf::add(Metric::AudioStreamFailed, 0, 3);
                            let _ = sender.send(Packet::Failed(error));
                        }
                        return;
                    }
                };
                owner.bytes.store(BLOCKS * FRAMES * 8, Ordering::Relaxed);
                knife_perf::gauge_add(Gauge::AudioStreamBufferBytes, (BLOCKS * FRAMES * 8) as u64);
                loop {
                    if stopped.load(Ordering::Acquire) { break; }
                    let mut block = match initial.pop() {
                        Some(block) => block,
                        None => match free.recv_timeout(Duration::from_millis(20)) {
                            Ok(block) => block,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    };
                    let decode = Scope::new(Metric::AudioStreamDecode);
                    let result = reader.read(&mut block.pcm[..]);
                    if stopped.load(Ordering::Acquire) { break; }
                    match result {
                        Ok(0) => { let _ = sender.send(Packet::End); break; }
                        Ok(n) if n <= FRAMES => {
                            block.frames = n;
                            // Name only actual slow reads. Fast 20 ms blocks
                            // retain the old allocation-free aggregate path.
                            if decode.elapsed_us() >= 20_000 {
                                decode.finish_hot(&worker_label, (n * 8) as u64, 1);
                            } else { decode.finish((n * 8) as u64, 1); }
                            if sender.send(Packet::Data(block)).is_err() { break; }
                        }
                        result => {
                            knife_perf::add(Metric::AudioStreamFailed, 0, 2);
                            let error = result.err().unwrap_or_else(|| "invalid stream PCM frame count".into());
                            let _ = sender.send(Packet::Failed(error)); break;
                        }
                    }
                }
            }).map_err(|error| format!("audio stream worker: {error}"))?;
        Ok(Self { ready, recycle, cancel, current: None, cursor: 0,
            worker: Some(worker), started: false, waiting_reported: false, terminal: false, _channel: channel,
            label, wait })
    }
    fn finish_wait(&mut self) {
        if let Some(wait) = self.wait.take() { wait.finish(&self.label); }
    }
    /// Called once for an incomplete mixer block, not once per missing sample.
    /// A submitted device block can contain silence for just this one stream.
    pub(super) fn missing_frames(&mut self, frames: usize) {
        let frames = frames.min(FRAMES) as u64;
        if let Some(wait) = &mut self.wait { wait.missing = wait.missing.saturating_add(frames); }
        knife_perf::add(Metric::AudioStreamMissingFrames, 0, frames);
    }
    /// Intentional pause is not starvation. Close a partial episode now, then
    /// start another only if the resumed stream really polls an empty queue.
    pub(super) fn suspend_wait(&mut self) {
        self.finish_wait();
        self.waiting_reported = false;
    }
    pub(super) fn sample(&mut self) -> Poll {
        if self.terminal { return Poll::End; }
        if self.current.as_ref().is_none_or(|b| self.cursor == b.frames) {
            if let Some(block) = self.current.take() { let _ = self.recycle.try_send(block); }
            match self.ready.try_recv() {
                Ok(Packet::Data(block)) => {
                    self.current = Some(block); self.cursor = 0;
                    self.finish_wait();
                    if !self.started { knife_perf::add(Metric::AudioStreamReady, 0, 1); }
                    self.started = true; self.waiting_reported = false;
                }
                Ok(Packet::End) => { self.finish_wait(); self.terminal = true; return Poll::End; }
                Ok(Packet::Failed(error)) => { self.finish_wait(); self.terminal = true; return Poll::Failed(error); }
                Err(TryRecvError::Empty) => {
                    if !self.waiting_reported {
                        knife_perf::add(if self.started { Metric::AudioStreamStarved }
                            else { Metric::AudioStreamPending }, 0, 1);
                        self.wait.get_or_insert_with(|| WaitEpisode::new(if self.started {
                            Metric::AudioStreamStarveWall
                        } else { Metric::AudioStreamStartWall }));
                        self.waiting_reported = true;
                    }
                    return Poll::Pending;
                }
                Err(TryRecvError::Disconnected) => {
                    self.finish_wait();
                    self.terminal = true;
                    return Poll::Failed("audio stream worker exited without EOF".into());
                }
            }
        }
        let block = self.current.as_ref().unwrap();
        let value = block.pcm[self.cursor]; self.cursor += 1;
        Poll::Sample(value, block.mono)
    }
}
impl Drop for Pump {
    fn drop(&mut self) {
        self.finish_wait();
        self.cancel.store(true, Ordering::Release);
        if !self.terminal { knife_perf::add(Metric::AudioStreamCancelled, 0, 1); }
        // Dropping receivers wakes queued sends. A blocked SDK read/open keeps
        // its own decoder and permit until it exits; never wait on the mixer.
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            let _ = self.worker.take().unwrap().join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    struct Sequence { at: usize, end: usize }
    impl Reader for Sequence {
        fn mono(&self) -> bool { false }
        fn read(&mut self, out: &mut [[f32; 2]]) -> Result<usize, String> {
            let n = (self.end - self.at).min(out.len());
            for p in &mut out[..n] { *p = [self.at as f32, -(self.at as f32)]; self.at += 1; }
            Ok(n)
        }
    }
    fn drain_workers() {
        let until = Instant::now() + Duration::from_secs(2);
        while WORKERS.load(Ordering::Acquire) != 0 && Instant::now() < until { thread::yield_now(); }
        assert_eq!(WORKERS.load(Ordering::Acquire), 0);
    }
    #[test]
    fn blocked_open_does_not_block_poll_or_another_channel_and_late_cancel_is_safe() {
        let _lock = SERIAL.lock().unwrap(); drain_workers();
        let (tx, rx) = mpsc::channel();
        let mut blocked = Pump::spawn_named("test-stream", move || { rx.recv().unwrap(); Ok(Sequence { at: 0, end: 1 }) }).unwrap();
        for _ in 0..1000 { assert!(matches!(blocked.sample(), Poll::Pending)); }
        let mut other = Pump::spawn_named("test-stream", || Ok(Sequence { at: 0, end: 7 })).unwrap();
        let until = Instant::now() + Duration::from_secs(2); let mut n = 0;
        loop { match other.sample() {
            Poll::Sample(v, false) => { assert_eq!(v, [n as f32, -(n as f32)]); n += 1; }
            Poll::End => break, Poll::Pending => thread::yield_now(), _ => panic!("bad packet")
        } assert!(Instant::now() < until); }
        assert_eq!(n, 7); drop(blocked); tx.send(()).unwrap(); drop(other); drain_workers();
    }
    #[test]
    fn recycled_pool_preserves_pcm_across_queue_backpressure_and_partial_eof() {
        let _lock = SERIAL.lock().unwrap(); drain_workers();
        let total = FRAMES * (BLOCKS + 5) + 37;
        let mut pump = Pump::spawn_named("test-stream", move || Ok(Sequence { at: 0, end: total })).unwrap();
        let mut n = 0; let until = Instant::now() + Duration::from_secs(3);
        loop { match pump.sample() {
            Poll::Sample(v, false) => { assert_eq!(v, [n as f32, -(n as f32)]); n += 1; }
            Poll::End => break, Poll::Pending => thread::yield_now(), _ => panic!("bad packet")
        } assert!(Instant::now() < until); }
        assert_eq!(n, total); drop(pump); drain_workers();
    }
    #[test]
    fn failed_open_is_terminal_and_channels_are_bounded() {
        let _lock = SERIAL.lock().unwrap(); drain_workers();
        let mut pump = Pump::spawn_named("test-stream", || -> Result<Sequence, String> { Err("broken ogg".into()) }).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        loop { match pump.sample() { Poll::Failed(e) => { assert_eq!(e, "broken ogg"); break; },
            Poll::Pending => thread::yield_now(), _ => panic!("not an error") }
            assert!(Instant::now() < until); }
        drop(pump); drain_workers();
        let held: Vec<_> = (0..CHANNELS).map(|_| Pump::spawn_named("test-stream", || Ok(Sequence { at: 0, end: usize::MAX })).unwrap()).collect();
        assert!(Pump::spawn_named("test-stream", || Ok(Sequence { at: 0, end: 0 })).is_err());
        drop(held); drain_workers();
    }
    #[test]
    fn queued_pcm_keeps_channel_permit_after_decoder_eof_and_decoder_is_thread_local() {
        let _lock = SERIAL.lock().unwrap(); drain_workers();
        struct Local { at: std::rc::Rc<std::cell::Cell<usize>>, thread: thread::ThreadId }
        impl Reader for Local {
            fn mono(&self) -> bool { true }
            fn read(&mut self, out: &mut [[f32;2]]) -> Result<usize,String> {
                assert_eq!(self.thread, thread::current().id());
                if self.at.get() != 0 { return Ok(0); }
                out[0] = [0.25,0.25]; self.at.set(1); Ok(1)
            }
        }
        let mut pump = Pump::spawn_named("test-stream", || Ok(Local { at: std::rc::Rc::new(std::cell::Cell::new(0)),
            thread: thread::current().id() })).unwrap();
        let until = Instant::now() + Duration::from_secs(2);
        while !pump.worker.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < until); thread::yield_now();
        }
        assert_eq!(WORKERS.load(Ordering::Acquire), 1);
        assert!(matches!(pump.sample(),Poll::Sample([0.25,0.25],true)));
        assert!(matches!(pump.sample(),Poll::End));
        assert_eq!(WORKERS.load(Ordering::Acquire), 1);
        drop(pump); drain_workers();
    }
}

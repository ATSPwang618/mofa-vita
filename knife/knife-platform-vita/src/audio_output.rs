//! Single-port Vita AudioOut worker. Blocking SDK calls stay off the GXM thread.
use std::{collections::VecDeque, marker::PhantomData, rc::Rc,
    sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError}},
    thread::{self, JoinHandle}, time::{Duration, Instant}};

#[path = "audio_metrics.rs"]
pub mod metrics;

pub const FRAMES: usize = 960;
pub const SAMPLE_RATE: u32 = 48_000;
const SAMPLES: usize = FRAMES * 2;
pub const QUEUE_BLOCKS: usize = 4;
pub const POOL_BLOCKS: usize = QUEUE_BLOCKS + 2;
pub const POOL_BYTES: usize = POOL_BLOCKS * SAMPLES * size_of::<i16>();
const PRIME_BLOCKS: usize = 2;
const PRIME_TIMEOUT: Duration = Duration::from_millis(40);
const EDGE_FRAMES: usize = 128;

unsafe extern "C" {
    fn kv_audio_port_open(port: *mut i32) -> i32;
    fn kv_audio_port_output(port: i32, pcm: *const i16) -> i32;
    fn kv_audio_port_volume(port: i32, left: u32, right: u32) -> i32;
    fn kv_audio_port_drain(port: i32) -> i32;
    fn kv_audio_port_close(port: i32) -> i32;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputError {
    InvalidBlock, InvalidVolume, Allocation, QueueFull, Closed, Spawn,
    Open(i32), Output(i32), Volume(i32), Drain(i32), Close(i32), Panicked,
}
impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "Vita AudioOut: {self:?}") }
}
impl std::error::Error for OutputError {}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct OutputStats {
    pub source_frames: u64,
    pub output_blocks: u64,
    pub silence_blocks: u64,
    pub underruns: u64,
    pub drained: bool,
    pub released: bool,
}
#[derive(Debug)]
struct Block { samples: Vec<i16>, source_frames: u32 }
enum Command { Pause(bool), Volume(u32, u32), End, Stop }
fn command_error(error: TrySendError<Command>) -> OutputError {
    match error { TrySendError::Full(_) => OutputError::QueueFull,
        TrySendError::Disconnected(_) => OutputError::Closed }
}

/// Clone and move this producer to a mixer/decoder thread. try_submit never waits
/// for AudioOut or the output queue; a full queue is reported to its caller.
#[derive(Clone)]
pub struct AudioProducer {
    blocks: SyncSender<Block>,
    free: Arc<Mutex<Receiver<Block>>>,
    recycle: SyncSender<Block>,
    state: Arc<QueueState>,
}
#[derive(Default)]
struct QueueState { queued: AtomicU32, active: AtomicBool, closed: AtomicBool }

fn pool() -> Result<(AudioProducer, Receiver<Block>), OutputError> {
    let (blocks, receiver) = mpsc::sync_channel(QUEUE_BLOCKS);
    let (recycle, free) = mpsc::sync_channel(POOL_BLOCKS);
    for _ in 0..POOL_BLOCKS {
        let mut samples = Vec::new();
        samples.try_reserve_exact(SAMPLES).map_err(|_| OutputError::Allocation)?;
        samples.resize(SAMPLES, 0);
        recycle.try_send(Block { samples, source_frames: 0 }).map_err(|_| OutputError::Allocation)?;
    }
    Ok((AudioProducer { blocks, free: Arc::new(Mutex::new(free)), recycle,
        state: Arc::new(QueueState::default()) }, receiver))
}
impl AudioProducer {
    pub fn try_submit(&self, interleaved: &[i16]) -> Result<(), OutputError> {
        if interleaved.is_empty() || interleaved.len() > SAMPLES || interleaved.len() % 2 != 0 {
            return Err(OutputError::InvalidBlock);
        }
        if self.state.closed.load(Ordering::Acquire) { return Err(OutputError::Closed); }
        // Reserve queue capacity before borrowing/copying a buffer. A full queue
        // performs no allocation and no PCM copy, including mixer retry loops.
        let queued = self.state.queued.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |n| (n < QUEUE_BLOCKS as u32).then_some(n + 1)).map_err(|_| {
                metrics::QUEUE_FULL.fetch_add(1, Ordering::Relaxed);
                OutputError::QueueFull
            })? + 1;
        let free = self.free.try_lock().ok().and_then(|rx| rx.try_recv().ok());
        let Some(mut block) = free else {
            self.state.queued.fetch_sub(1, Ordering::AcqRel);
            metrics::QUEUE_FULL.fetch_add(1, Ordering::Relaxed);
            return Err(OutputError::QueueFull);
        };
        block.samples[..interleaved.len()].copy_from_slice(interleaved);
        block.samples[interleaved.len()..].fill(0);
        block.source_frames = (interleaved.len() / 2) as u32;
        self.state.active.store(true, Ordering::Release);
        match self.blocks.try_send(block) {
            Ok(()) => {
                metrics::SUBMITTED.fetch_add(1, Ordering::Relaxed);
                metrics::PEAK_QUEUED.fetch_max(queued, Ordering::Relaxed);
                Ok(())
            },
            Err(error) => {
                self.state.queued.fetch_sub(1, Ordering::AcqRel);
                let (block, error) = match error {
                    TrySendError::Full(b) => (b, OutputError::QueueFull),
                    TrySendError::Disconnected(b) => (b, OutputError::Closed),
                };
                let _ = self.recycle.try_send(block);
                Err(error)
            },
        }
    }
}

pub struct AudioOutput {
    producer: AudioProducer,
    commands: SyncSender<Command>,
    worker: Option<JoinHandle<Result<OutputStats, OutputError>>>,
    _thread_bound: PhantomData<Rc<()>>,
}
impl AudioOutput {
    pub fn open() -> Result<Self, OutputError> {
        let (producer, rx) = pool()?;
        let state = producer.state.clone();
        let recycle = producer.recycle.clone();
        let (commands, controls) = mpsc::sync_channel(16);
        let (startup, ready) = mpsc::sync_channel(1);
        let worker = thread::Builder::new().name("knife-audio-out".into())
            .stack_size(64 * 1024).spawn(move || run(rx, controls, startup, recycle, state))
            .map_err(|_| OutputError::Spawn)?;
        match ready.recv() {
            Ok(Ok(())) => Ok(Self { producer, commands,
                worker: Some(worker), _thread_bound: PhantomData }),
            Ok(Err(error)) => { let _ = worker.join(); Err(error) }
            Err(_) => { let _ = worker.join(); Err(OutputError::Panicked) }
        }
    }
    pub fn producer(&self) -> AudioProducer { self.producer.clone() }
    /// Distinguish intentional idle from a running mixer missing its deadline.
    pub fn set_active(&self, active: bool) {
        self.producer.state.active.store(active, Ordering::Release);
    }
    pub fn try_submit(&self, interleaved: &[i16]) -> Result<(), OutputError> {
        self.producer.try_submit(interleaved)
    }
    pub fn set_paused(&self, paused: bool) -> Result<(), OutputError> {
        self.commands.try_send(Command::Pause(paused)).map_err(command_error)
    }
    /// Hardware gain (0..32768); voice/category envelopes belong to the mixer.
    pub fn set_volume(&self, left: u32, right: u32) -> Result<(), OutputError> {
        if left > 32768 || right > 32768 { return Err(OutputError::InvalidVolume); }
        self.commands.try_send(Command::Volume(left, right)).map_err(command_error)
    }
    /// Drain queued PCM and the final SDK buffer, close the port, and join.
    /// Producer threads must finish submitting before calling this method.
    pub fn finish(mut self) -> Result<OutputStats, OutputError> {
        let _ = self.commands.send(Command::End);
        self.join()
    }
    /// Discard queued blocks and join after the current blocking SDK call.
    pub fn stop(mut self) -> Result<OutputStats, OutputError> {
        let _ = self.commands.send(Command::Stop);
        self.join()
    }
    fn join(&mut self) -> Result<OutputStats, OutputError> {
        self.worker.take().ok_or(OutputError::Closed)?.join().map_err(|_| OutputError::Panicked)?
    }
}
impl Drop for AudioOutput {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.commands.send(Command::Stop);
            let _ = worker.join();
        }
    }
}
struct PlaybackBuffers {
    inflight: Option<Block>,
    failed: Option<Block>,
    gaps: [[i16; SAMPLES]; 2],
}
fn fade_in(samples: &mut [i16]) {
    for (i, frame) in samples.chunks_exact_mut(2).take(EDGE_FRAMES).enumerate() {
        for sample in frame { *sample = (i32::from(*sample) * i as i32 / EDGE_FRAMES as i32) as i16; }
    }
}
fn fade_out(samples: &mut [i16], last: [i16; 2]) {
    samples.fill(0);
    for (i, frame) in samples.chunks_exact_mut(2).take(EDGE_FRAMES).enumerate() {
        for c in 0..2 {
            frame[c] = (i32::from(last[c]) * (EDGE_FRAMES - i) as i32 / EDGE_FRAMES as i32) as i16;
        }
    }
}
fn run(blocks: Receiver<Block>, controls: Receiver<Command>,
       startup: SyncSender<Result<(), OutputError>>, recycle: SyncSender<Block>,
       state: Arc<QueueState>) -> Result<OutputStats, OutputError> {
    let mut port = -1;
    // SAFETY: exclusive worker owns a valid output slot; C reserves at most one port.
    let opened = unsafe { kv_audio_port_open(&mut port) };
    if opened < 0 {
        let error = OutputError::Open(opened);
        let _ = startup.send(Err(error));
        return Err(error);
    }
    let _ = startup.send(Ok(()));
    // These allocations/arrays outlive play_loop, drain, and every close retry.
    // The driver may retain the latest submitted pointer after Output returns.
    // Keep even the two gap buffers heap-owned: on the exceptional path where
    // both drain and release fail, forgetting this Box keeps their addresses
    // valid after the worker returns, just like the pooled PCM vectors.
    let mut buffers = Box::new(PlaybackBuffers { inflight: None, failed: None, gaps: [[0; SAMPLES]; 2] });
    let result = play_loop(port, blocks, controls, &recycle, &state, &mut buffers);
    state.closed.store(true, Ordering::Release);
    let already_drained = result.as_ref().is_ok_and(|stats| stats.drained);
    let settled = already_drained || unsafe { kv_audio_port_drain(port) } >= 0;
    let mut close_error = None;
    for _ in 0..3 {
        // SAFETY: C retains port ownership after a failed release for a retry.
        let status = unsafe { kv_audio_port_close(port) };
        if status >= 0 { close_error = None; break; }
        close_error = Some(OutputError::Close(status));
    }
    if !settled && close_error.is_some() {
        // Both driver settlement operations failed. Preserve the bounded last
        // buffers rather than allowing a still-owned DMA pointer to dangle.
        std::mem::forget(buffers);
    }
    match (result, close_error) {
        (Ok(mut stats), None) => { stats.released = true; Ok(stats) }
        (Err(error), _) => Err(error),
        (Ok(_), Some(error)) => Err(error),
    }
}
fn play_loop(port: i32, blocks: Receiver<Block>, controls: Receiver<Command>,
             recycle: &SyncSender<Block>, state: &QueueState,
             buffers: &mut PlaybackBuffers) -> Result<OutputStats, OutputError> {
    let mut stats = OutputStats::default();
    let mut paused = false;
    let mut ending = false;
    let mut ready = VecDeque::with_capacity(QUEUE_BLOCKS);
    let mut priming = true;
    let mut prime_started = None;
    let mut gap_index = 0;
    let mut gap = true;
    let mut has_played = false;
    let mut last = [0i16; 2];
    loop {
        loop {
            match controls.try_recv() {
                Ok(Command::Pause(value)) => paused = value,
                Ok(Command::Volume(l, r)) => {
                    // SAFETY: worker exclusively uses the opened port, values checked by public API.
                    let status = unsafe { kv_audio_port_volume(port, l, r) };
                    if status < 0 { return Err(OutputError::Volume(status)); }
                }
                Ok(Command::End) => { ending = true; paused = false; },
                Ok(Command::Stop) => return Ok(stats),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return Ok(stats),
            }
        }
        while ready.len() < QUEUE_BLOCKS {
            match blocks.try_recv() { Ok(block) => ready.push_back(block), Err(_) => break }
        }
        if ending && ready.is_empty() { break; }
        if priming && !ready.is_empty() {
            let started = prime_started.get_or_insert_with(Instant::now);
            if ending || ready.len() >= PRIME_BLOCKS || started.elapsed() >= PRIME_TIMEOUT { priming = false; }
        }
        let mut item = if !paused && !priming { ready.pop_front() } else { None };
        if item.is_some() { state.queued.fetch_sub(1, Ordering::AcqRel); }
        let source_frames = item.as_ref().map(|b| b.source_frames);
        let data = if let Some(block) = item.as_mut() {
            if gap { fade_in(&mut block.samples); }
            last.copy_from_slice(&block.samples[SAMPLES - 2..]);
            gap = false;
            has_played = true;
            block.samples.as_ptr()
        } else {
            // Repriming after a missed block is still starvation of an active
            // stream. Only initial priming, pause and declared idle are idle.
            let empty_running = !paused && has_played && state.active.load(Ordering::Acquire) && !ending;
            if empty_running {
                stats.underruns += 1;
                metrics::UNDERRUNS.fetch_add(1, Ordering::Relaxed);
            } else { metrics::IDLE.fetch_add(1, Ordering::Relaxed); }
            fade_out(&mut buffers.gaps[gap_index], last);
            last = [0; 2];
            let data = buffers.gaps[gap_index].as_ptr();
            gap_index ^= 1;
            gap = true;
            if ready.is_empty() { priming = true; prime_started = None; }
            data
        };
        // SAFETY: new and previous PCM remain owned through the next blocking
        // call, including the final drain and stop/error/release paths.
        let status = unsafe { kv_audio_port_output(port, data) };
        if status < 0 { buffers.failed = item; return Err(OutputError::Output(status)); }
        if let Some(previous) = buffers.inflight.take() { let _ = recycle.try_send(previous); }
        buffers.inflight = item;
        stats.output_blocks += 1;
        metrics::OUTPUT.fetch_add(1, Ordering::Relaxed);
        if let Some(frames) = source_frames { stats.source_frames += u64::from(frames); }
        else { stats.silence_blocks += 1; }
    }
    // SAFETY: the worker still owns this port; NULL requests drain of its final buffer.
    let status = unsafe { kv_audio_port_drain(port) };
    if status < 0 { return Err(OutputError::Drain(status)); }
    stats.drained = true;
    Ok(stats)
}
#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" {
        fn kv_test_fail_open(count: i32);
        fn kv_test_fail_output(count: i32);
        fn kv_test_fail_release(count: i32);
        fn kv_test_opened() -> i32;
        fn kv_test_closed() -> i32;
    }
    #[test]
    fn bounded_pool_reuses_only_settled_buffers_and_full_retries_do_not_copy() {
        let (producer, receiver) = pool().unwrap();
        let mut addresses = std::collections::HashSet::new();
        let mut previous = None;
        for cycle in 0..100 {
            for i in 0..QUEUE_BLOCKS { producer.try_submit(&[cycle, i as i16]).unwrap(); }
            for _ in 0..50 { assert_eq!(producer.try_submit(&[123, 456]), Err(OutputError::QueueFull)); }
            for i in 0..QUEUE_BLOCKS {
                let block = receiver.try_recv().unwrap();
                producer.state.queued.fetch_sub(1, Ordering::AcqRel);
                addresses.insert(block.samples.as_ptr() as usize);
                assert_eq!(&block.samples[..2], &[cycle, i as i16]);
                assert!(block.samples[2..].iter().all(|&v| v == 0));
                // The previous buffer returns only after a simulated output
                // has consumed it, while this newly submitted buffer remains held.
                if let Some(done) = previous.take() { producer.recycle.try_send(done).unwrap(); }
                previous = Some(block);
            }
        }
        assert!(addresses.len() <= POOL_BLOCKS);
        assert_eq!(POOL_BYTES, 23_040);
        producer.state.closed.store(true, Ordering::Release);
        assert_eq!(producer.try_submit(&[1, 2]), Err(OutputError::Closed));
    }
    #[test]
    fn starvation_edges_are_short_continuous_ramps_without_changing_later_samples() {
        let mut samples = [10_000; SAMPLES];
        fade_in(&mut samples);
        assert_eq!(&samples[..2], &[0, 0]);
        assert_eq!(samples[EDGE_FRAMES * 2], 10_000);
        assert!(samples[..EDGE_FRAMES * 2].windows(2).all(|w| w[0] <= w[1]));
        fade_out(&mut samples, [10_000, -8_000]);
        assert_eq!(&samples[..2], &[10_000, -8_000]);
        assert!(samples[EDGE_FRAMES * 2..].iter().all(|&v| v == 0));
        assert!(samples.chunks_exact(2).all(|s| s[0] >= 0 && s[1] <= 0));
    }
    #[test]
    fn sdk_failures_release_or_retry_the_single_owner() {
        unsafe { kv_test_fail_open(1) };
        assert_eq!(AudioOutput::open().err(), Some(OutputError::Open(-44)));
        let output = AudioOutput::open().unwrap();
        unsafe { kv_test_fail_release(1) };
        assert!(output.stop().unwrap().released);
        let output = AudioOutput::open().unwrap();
        unsafe { kv_test_fail_output(1) };
        std::thread::sleep(std::time::Duration::from_millis(10));
        assert_eq!(output.finish(), Err(OutputError::Output(-45)));
        assert_eq!(unsafe { kv_test_opened() }, unsafe { kv_test_closed() });
    }
    #[test]
    fn queue_and_lifecycle() {
        let output = AudioOutput::open().unwrap();
        assert_eq!(AudioOutput::open().err(), Some(OutputError::Open(-2)));
        assert_eq!(output.try_submit(&[1]), Err(OutputError::InvalidBlock));
        assert_eq!(output.set_volume(32769, 0), Err(OutputError::InvalidVolume));
        output.set_paused(true).unwrap();
        output.set_volume(1000, 2000).unwrap();
        let producer = output.producer();
        producer.try_submit(&[1, -1, 2, -2]).unwrap();
        output.set_paused(false).unwrap();
        let mut saw_full = false;
        for _ in 0..1024 {
            if output.set_volume(1000, 2000)==Err(OutputError::QueueFull) {
                saw_full = true;
                break;
            }
        }
        assert!(saw_full, "control queue failed to enforce its capacity");
        let stats = output.finish().unwrap();
        assert_eq!(stats.source_frames, 2);
        assert!(stats.drained && stats.released);
        let another = AudioOutput::open().unwrap();
        another.stop().unwrap();
    }

    #[test]
    fn rebuffering_an_active_stream_remains_underrun_until_audio_resumes() {
        let output = AudioOutput::open().unwrap();
        output.try_submit(&[100; SAMPLES]).unwrap();
        output.try_submit(&[100; SAMPLES]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let before = metrics::snapshot();
        while metrics::snapshot().output.wrapping_sub(before.output) < 12 {
            assert!(Instant::now() < deadline, "mock output stalled");
            thread::sleep(Duration::from_millis(1));
        }
        let starved = metrics::snapshot();
        assert!(starved.underruns.wrapping_sub(before.underruns) >= 2,
            "active rebuffer silence must not be mislabeled as intentional idle");
        output.set_active(false);
        let stats = output.stop().unwrap();
        assert!(stats.drained || stats.released);
    }
}

//! Streaming audio service. No TJS values or platform device APIs enter here.
pub mod at9;
mod decoder;
/// Inspect the built-in decoder path without a device or extended host backend.
/// Opens the container and decodes its first packet; not a whole-file validator.
pub fn inspect_builtin(plan: krkr_assets::ReadPlan) -> Result<Format> {
    decoder::inspect_builtin(plan)
}
/// Offline TCWF metadata inspection; playback still requires plugin registration.
pub fn inspect_tcwf(plan: krkr_assets::ReadPlan) -> Result<Format> {
    tcwf::Decoder::open(plan.open().map_err(|e| e.to_string())?, plan.bytes).map(|d| d.format)
}
pub mod filter;
mod registration;
mod tcwf;
mod visualization;
pub use registration::{Codec, Registration};
/// Portable decoded stereo frame; source channel count remains in Format.
#[derive(Clone, Copy, Default)]
pub struct DecodedFrame {
    pub sample: [f32; 2],
    /// Source channels before mixer gain, pan and stereo downmix.
    pub pcm: [i16; 8],
    pub position: u64,
}
pub trait StreamDecoder: Send {
    fn format(&self) -> Format;
    fn seek(&mut self, frame: u64) -> Result<()>;
    fn next(&mut self) -> Result<Option<DecodedFrame>>;
    /// Fill up to `output.len()` source frames without allocating. A short
    /// block is allowed; zero means EOF unless the destination is empty.
    /// The default keeps existing host backends on their sample-exact path.
    fn read_frames(&mut self, output: &mut [DecodedFrame]) -> Result<usize> {
        let mut count = 0;
        for target in output {
            let Some(frame) = self.next()? else {
                break;
            };
            *target = frame;
            count += 1;
        }
        Ok(count)
    }
}
pub trait DecoderBackend: Send + Sync {
    fn open(
        &self,
        plan: &krkr_assets::ReadPlan,
        budget: Budget,
        opus: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn StreamDecoder>>;

    /// Reuse the stream and metadata from format detection. Backends that need
    /// to open their own IO context can keep implementing only `open`.
    fn open_prepared(
        &self,
        plan: &krkr_assets::ReadPlan,
        budget: Budget,
        opus: bool,
        cancel: Arc<AtomicBool>,
        source: DecoderSource,
    ) -> Result<Box<dyn StreamDecoder>> {
        drop(source);
        self.open(plan, budget, opus, cancel)
    }
}
/// An already-open source. Its cursor is unspecified; seek before decoding.
pub struct DecoderSource {
    pub stream: Box<dyn krkr_assets::Stream>,
    pub at9: Option<at9::Header>,
}
mod mixing;
mod pcm;
pub use pcm::Pcm;
pub mod loops;
use decoder::Decoder;
pub use krkr_protocol::audio::{Format, PlaybackId, Position};
use krkr_protocol::budget::{Budget, Permit};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
pub type Result<T> = std::result::Result<T, String>;
const FRAMES: usize = 4096;
const STAMPS: usize = 512;
const NOTIFICATIONS: usize = 1024;
static NEXT_PLAYBACK: AtomicU64 = AtomicU64::new(1);

/// Implemented by a device host. Called lazily on a worker, never by a VM or
/// device callback. The host retains its device stream for its own lifetime.
pub trait OutputHost: Send + Sync {
    fn start(&self, mixer: Mixer) -> Result<()>;
}
#[derive(Clone)]
pub struct Service(Arc<World>);
struct World {
    codecs: Arc<Mutex<[usize; 4]>>,
    decoder_backend: Mutex<Option<Arc<dyn DecoderBackend>>>,
    voices: Mutex<Vec<Weak<Voice>>>,
    voice_revision: AtomicU32,
    output: Mutex<Option<Arc<dyn OutputHost>>>,
    started: Mutex<bool>,
    error: Arc<Mutex<Option<String>>>,
    pub budget: Budget,
    origin: Instant,
}
impl Default for Service {
    fn default() -> Self {
        Self::new(Budget::new(16 * 1024 * 1024))
    }
}
impl Service {
    pub fn new(budget: Budget) -> Self {
        Self(Arc::new(World {
            codecs: Arc::default(),
            decoder_backend: Mutex::default(),
            voices: Mutex::new(Vec::with_capacity(64)),
            voice_revision: AtomicU32::new(0),
            output: Mutex::new(None),
            started: Mutex::new(false),
            error: Arc::new(Mutex::new(None)),
            budget,
            origin: Instant::now(),
        }))
    }
    pub fn set_output(&self, output: impl OutputHost + 'static) -> Result<()> {
        if *self.0.started.lock().unwrap() {
            return Err("audio output already started".into());
        }
        *self.0.output.lock().unwrap() = Some(Arc::new(output));
        Ok(())
    }
    pub fn set_decoder_backend(&self, backend: impl DecoderBackend + 'static) {
        *self.0.decoder_backend.lock().unwrap() = Some(Arc::new(backend));
    }
    pub fn error(&self) -> Option<String> {
        self.0.error.lock().unwrap().clone()
    }
    pub fn budget(&self) -> &Budget {
        &self.0.budget
    }
    pub fn open(
        &self,
        plan: krkr_assets::ReadPlan,
        sli: Option<krkr_assets::ReadPlan>,
        stop: &AtomicBool,
    ) -> Result<Handle> {
        self.open_filtered(plan, sli, Vec::new(), stop)
    }
    pub fn open_filtered(
        &self,
        plan: krkr_assets::ReadPlan,
        sli: Option<krkr_assets::ReadPlan>,
        filters: Vec<filter::PhaseVocoder>,
        stop: &AtomicBool,
    ) -> Result<Handle> {
        if filters.len() > 16 {
            return Err("audio filter chain capacity reached".into());
        }
        let connections = filters
            .iter()
            .map(filter::PhaseVocoder::connect)
            .collect::<Result<Vec<_>>>()?;
        if stop.load(Ordering::Relaxed) {
            return Err("audio open cancelled".into());
        }
        let codecs = *self.0.codecs.lock().unwrap();
        let backend = self.0.decoder_backend.lock().unwrap().clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let decoder = Decoder::open(
            plan,
            codecs,
            backend,
            self.0.budget.clone(),
            stop,
            cancel.clone(),
        )?;
        let format = decoder.format;
        let info = if let Some(sli) = sli {
            if sli.bytes > 64 * 1024 {
                return Err("loop information exceeds budget".into());
            }
            loops::Information::parse(
                &sli.read_interruptible(0, || stop.load(Ordering::Relaxed))
                    .map_err(|e| e.to_string())?,
            )?
        } else {
            Default::default()
        };
        let decoder = loops::Stream::new(decoder, info, &self.0.budget)?;
        let (handle, rx) = self.voice(
            format,
            decoder.flags.clone(),
            decoder.info.clone(),
            connections,
        )?;
        let decoder = filter::Stream::new(
            decoder,
            filters,
            self.0.budget.clone(),
            format.channels as usize,
        );
        let voice = handle.0.voice.clone();
        *voice.decode_cancel.lock().unwrap() = Some(cancel);
        if stop.load(Ordering::Relaxed) {
            return Err("audio open cancelled".into());
        }
        let data = voice.clone();
        let thread = std::thread::Builder::new()
            .name("krkr-audio-decode".into())
            .spawn(move || decode_loop(data, decoder, rx))
            .map_err(|e| e.to_string())?;
        let _ = voice.worker.set(thread.thread().clone());
        Ok(handle)
    }
    fn voice(
        &self,
        format: Format,
        flags: Arc<[AtomicI32; 16]>,
        info: Arc<loops::Information>,
        connections: Vec<filter::Connection>,
    ) -> Result<(Handle, Receiver<Command>)> {
        // Reserve the queues actually allocated by every voice. Decoder,
        // packet, crossfade and visualization storage have separate owners.
        let notifications = if info.labels.is_empty() {
            0
        } else {
            NOTIFICATIONS
        };
        let bytes = std::mem::size_of::<Voice>()
            + FRAMES * std::mem::size_of::<Frame>()
            + STAMPS * std::mem::size_of::<Stamp>()
            + notifications * std::mem::size_of::<(Duration, usize)>();
        let permit = self.0.budget.reserve(bytes).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::sync_channel(2);
        let voice = Arc::new(Voice {
            flags,
            info,
            id: PlaybackId(NEXT_PLAYBACK.fetch_add(1, Ordering::Relaxed)),
            format,
            queue: Mutex::new(Queue {
                visual: None,
                frames: VecDeque::with_capacity(FRAMES),
                eof: false,
                phase: 0.0,
                stamps: VecDeque::with_capacity(STAMPS),
                notifications: VecDeque::with_capacity(notifications),
                labels_sent: false,
                played: 0,
                end_at: None,
            }),
            alive: AtomicBool::new(true),
            playing: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            looping: AtomicBool::new(false),
            frequency: AtomicI32::new(format.rate as i32),
            gain: AtomicI32::new(100000),
            pan: AtomicI32::new(0),
            decoded: AtomicU64::new(0),
            submitted: AtomicU64::new(0),
            ended: AtomicBool::new(false),
            error: Mutex::new(None),
            commands: tx,
            worker: OnceLock::new(),
            wake_generation: AtomicU64::new(0),
            decode_cancel: Mutex::new(None),
            origin: self.0.origin,
            underruns: AtomicU64::new(0),
            _permit: permit,
            budget: self.0.budget.clone(),
        });
        {
            let mut voices = self.0.voices.lock().unwrap();
            voices.retain(|v| v.upgrade().is_some_and(|v| v.alive.load(Ordering::Relaxed)));
            if voices.len() == 64 {
                return Err("audio voice capacity reached".into());
            }
            voices.push(Arc::downgrade(&voice));
            self.0.voice_revision.fetch_add(1, Ordering::Release);
        }
        let handle = Handle(Arc::new(Lease {
            voice: voice.clone(),
            _filters: connections,
        }));
        let mut started = self.0.started.lock().unwrap();
        if !*started {
            let output = self
                .0
                .output
                .lock()
                .unwrap()
                .clone()
                .ok_or("no audio output host")?;
            output.start(Mixer {
                world: Arc::downgrade(&self.0),
                error: self.0.error.clone(),
                voices: Vec::with_capacity(64),
                voice_revision: 0,
            })?;
            *started = true;
        }
        drop(started);
        Ok((handle, rx))
    }
}
#[derive(Clone, Copy)]
struct Frame {
    sample: [f32; 2],
    pcm: [i16; 8],
    position: u64,
    labels: [u16; 2],
}
struct Stamp {
    at: Duration,
    position: u64,
}
struct Queue {
    visual: Option<visualization::Buffer>,
    frames: VecDeque<Frame>,
    eof: bool,
    phase: f64,
    stamps: VecDeque<Stamp>,
    played: u64,
    end_at: Option<Duration>,
    notifications: VecDeque<(Duration, usize)>,
    labels_sent: bool,
}
impl Queue {
    fn labels(&mut self, frame: Frame, at: impl FnOnce() -> Duration) -> bool {
        if self.labels_sent {
            return true;
        }
        if self.notifications.len() + usize::from(frame.labels[1] - frame.labels[0]) > 1024 {
            return false;
        }
        if frame.labels[0] != frame.labels[1] {
            let at = at();
            self.notifications
                .extend((frame.labels[0]..frame.labels[1]).map(|i| (at, i as usize)));
        }
        self.labels_sent = true;
        true
    }
}
enum Command {
    Seek(u64, SyncSender<Result<()>>),
    Start(SyncSender<Result<()>>),
}
struct Voice {
    budget: Budget,
    flags: Arc<[AtomicI32; 16]>,
    info: Arc<loops::Information>,
    id: PlaybackId,
    format: Format,
    queue: Mutex<Queue>,
    alive: AtomicBool,
    playing: AtomicBool,
    paused: AtomicBool,
    looping: AtomicBool,
    frequency: AtomicI32,
    gain: AtomicI32,
    pan: AtomicI32,
    decoded: AtomicU64,
    submitted: AtomicU64,
    ended: AtomicBool,
    error: Mutex<Option<String>>,
    commands: SyncSender<Command>,
    worker: OnceLock<std::thread::Thread>,
    wake_generation: AtomicU64,
    decode_cancel: Mutex<Option<Arc<AtomicBool>>>,
    origin: Instant,
    underruns: AtomicU64,
    _permit: Permit,
}
struct Lease {
    voice: Arc<Voice>,
    _filters: Vec<filter::Connection>,
}
impl Voice {
    fn wake(&self) {
        // Decoder/IO code can consume the thread's unpark token while it
        // waits on a Rust channel. Preserve controls and PCM consumption
        // across those calls, just as the video worker preserves its controls.
        self.wake_generation.fetch_add(1, Ordering::Release);
        if let Some(worker) = self.worker.get() {
            worker.unpark();
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.voice.playing.store(false, Ordering::Relaxed);
        self.voice.alive.store(false, Ordering::Release);
        if let Some(cancel) = self.voice.decode_cancel.lock().unwrap().as_ref() {
            cancel.store(true, Ordering::Release);
        }
        self.voice.wake();
    }
}
#[derive(Clone)]
pub struct Handle(Arc<Lease>);
impl Handle {
    pub fn flag(&self, index: usize) -> i32 {
        self.voice().flags[index].load(Ordering::Relaxed)
    }
    pub fn set_flag(&self, index: usize, value: i32) {
        self.voice().flags[index].store(value.clamp(0, 9999), Ordering::Relaxed);
    }
    pub fn labels(&self) -> &[loops::Label] {
        &self.voice().info.labels
    }
    pub fn take_label_if(&self, accept: impl FnOnce() -> bool) -> Option<String> {
        let v = self.voice();
        let mut q = v.queue.lock().unwrap();
        if q.notifications
            .front()
            .is_some_and(|(at, _)| *at <= v.origin.elapsed())
            && accept()
        {
            let (_, index) = q.notifications.pop_front()?;
            Some(v.info.labels[index].name.clone())
        } else {
            None
        }
    }
    pub fn label_delay(&self) -> Option<Duration> {
        let v = self.voice();
        v.queue
            .lock()
            .unwrap()
            .notifications
            .front()
            .map(|(at, _)| at.saturating_sub(v.origin.elapsed()))
    }
    fn voice(&self) -> &Voice {
        &self.0.voice
    }
    pub fn id(&self) -> PlaybackId {
        self.voice().id
    }
    pub fn format(&self) -> Format {
        self.voice().format
    }
    /// Prepare PCM on the decoder worker after script flags and looping have
    /// been set. Like seek, this must only be called from a blocking IO worker.
    pub fn play(&self, stop: &AtomicBool) -> Result<()> {
        self.command(stop, Command::Start)
    }
    pub fn stop(&self) {
        self.voice().playing.store(false, Ordering::Release);
    }
    pub fn pause(&self, paused: bool) {
        self.voice().paused.store(paused, Ordering::Relaxed);
    }
    pub fn looping(&self, looping: bool) {
        self.voice().looping.store(looping, Ordering::Relaxed);
        self.voice().wake();
    }
    pub fn frequency(&self, frequency: i32) {
        self.voice().frequency.store(frequency, Ordering::Relaxed);
    }
    pub fn gain(&self, gain: i32, pan: i32) {
        self.voice().gain.store(gain, Ordering::Relaxed);
        self.voice().pan.store(pan, Ordering::Relaxed);
    }
    pub fn position(&self) -> Position {
        let v = self.voice();
        let mut q = v.queue.lock().unwrap();
        let now = v.origin.elapsed();
        while q.stamps.front().is_some_and(|s| s.at <= now) {
            q.played = q.stamps.pop_front().unwrap().position;
        }
        Position {
            decoded: v.decoded.load(Ordering::Relaxed),
            submitted: v.submitted.load(Ordering::Relaxed),
            played: q.played,
        }
    }
    pub fn finished(&self) -> bool {
        let v = self.voice();
        v.ended.load(Ordering::Acquire)
            && v.queue
                .lock()
                .unwrap()
                .end_at
                .is_none_or(|at| v.origin.elapsed() >= at)
    }
    pub fn error(&self) -> Option<String> {
        self.voice().error.lock().unwrap().clone()
    }
    pub fn underruns(&self) -> u64 {
        self.voice().underruns.load(Ordering::Relaxed)
    }
    pub fn seek(&self, position: u64, stop: &AtomicBool) -> Result<()> {
        let v = self.voice();
        if v.format.frames != 0 && position >= v.format.frames {
            return Ok(());
        }
        self.command(stop, |reply| Command::Seek(position, reply))
    }
    fn command(
        &self,
        stop: &AtomicBool,
        make: impl FnOnce(SyncSender<Result<()>>) -> Command,
    ) -> Result<()> {
        if stop.load(Ordering::Relaxed) {
            return Err("audio operation cancelled".into());
        }
        let v = self.voice();
        let (tx, rx) = mpsc::sync_channel(1);
        v.commands
            .try_send(make(tx))
            .map_err(|_| "audio command queue unavailable")?;
        v.wake();
        loop {
            if stop.load(Ordering::Relaxed) {
                v.playing.store(false, Ordering::Release);
                if let Some(cancel) = v.decode_cancel.lock().unwrap().as_ref() {
                    cancel.store(true, Ordering::Release);
                }
                return Err("audio operation cancelled".into());
            }
            match krkr_protocol::channel::recv_timeout(&rx, Duration::from_millis(10)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("audio decoder has stopped".into()),
            }
        }
    }
}
fn prefill(voice: &Voice, decoder: &mut filter::Stream) -> Result<()> {
    loop {
        let q = voice.queue.lock().unwrap();
        if q.frames.len() == FRAMES || q.eof || !voice.alive.load(Ordering::Acquire) {
            return Ok(());
        }
        drop(q);
        fill(voice, decoder)?;
    }
}
fn fill(voice: &Voice, decoder: &mut filter::Stream) -> Result<()> {
    decoder.update();
    // Decode outside the PCM lock. Only a small, fixed batch is copied while
    // holding it; the output callback uses try_lock and never waits for IO.
    let mut batch = [Frame {
        sample: [0.0; 2],
        pcm: [0; 8],
        position: 0,
        labels: [0, 0],
    }; 256];
    let space = FRAMES - voice.queue.lock().unwrap().frames.len();
    let (count, eof) = decoder.read_frames(voice, &mut batch[..space.min(256)])?;
    let mut q = voice.queue.lock().unwrap();
    q.frames.extend(batch[..count].iter().copied());
    q.eof = eof;
    voice.decoded.store(decoder.position(), Ordering::Relaxed);
    Ok(())
}
fn decode_loop(voice: Arc<Voice>, mut decoder: filter::Stream, rx: Receiver<Command>) {
    while voice.alive.load(Ordering::Acquire) {
        let wake_seen = voice.wake_generation.load(Ordering::Acquire);
        while let Ok(command) = rx.try_recv() {
            let (position, reply) = match command {
                Command::Start(reply) => {
                    let result = if voice.playing.load(Ordering::Acquire) {
                        Ok(())
                    } else {
                        prefill(&voice, &mut decoder).map(|()| {
                            voice.ended.store(false, Ordering::Relaxed);
                            voice.playing.store(true, Ordering::Release);
                        })
                    };
                    let _ = reply.send(result);
                    continue;
                }
                Command::Seek(position, reply) => (position, reply),
            };
            let playing = voice.playing.swap(false, Ordering::AcqRel);
            let result = decoder.seek(position);
            if result.is_ok() {
                let mut q = voice.queue.lock().unwrap();
                q.frames.clear();
                if let Some(visual) = &mut q.visual {
                    visual.clear();
                }
                q.stamps.clear();
                q.notifications.clear();
                q.labels_sent = false;
                q.phase = 0.0;
                q.eof = false;
                q.played = position;
                q.end_at = None;
                voice.submitted.store(position, Ordering::Relaxed);
                voice.ended.store(false, Ordering::Release);
                *voice.error.lock().unwrap() = None;
            }
            let result = result.and_then(|()| {
                if playing {
                    prefill(&voice, &mut decoder)
                } else {
                    Ok(())
                }
            });
            if result.is_ok() {
                voice.playing.store(playing, Ordering::Release);
            }
            let _ = reply.send(result);
        }
        let q = voice.queue.lock().unwrap();
        let wait = !voice.playing.load(Ordering::Acquire)
            || q.frames.len() == FRAMES
            || (q.eof && (!voice.looping.load(Ordering::Relaxed) || decoder.position() == 0));
        drop(q);
        if wait || voice.error.lock().unwrap().is_some() {
            // No polling while stopped or full. The mixer wakes the decoder
            // after consuming PCM; commands, looping changes and drop also wake.
            // Untimed park avoids Vita pthread's broken absolute timeout clock.
            if voice.wake_generation.load(Ordering::Acquire) == wake_seen {
                std::thread::park();
            }
            continue;
        }
        if let Err(error) = fill(&voice, &mut decoder) {
            *voice.error.lock().unwrap() = Some(error);
            voice.queue.lock().unwrap().eof = true;
        }
    }
}

/// Device-side mixer: consumes only bounded PCM, no IO, decoding or script locks.
pub struct Mixer {
    world: Weak<World>,
    error: Arc<Mutex<Option<String>>>,
    // Refresh only while the registry is available. Creating a sound must not
    // silence every already-playing voice for a whole device callback.
    // Weak entries neither extend a stopped voice's lifetime nor pin its PCM.
    voices: Vec<Weak<Voice>>,
    voice_revision: u32,
}
impl Mixer {
    pub fn fail(&self, error: String) {
        *self.error.lock().unwrap() = Some(error);
    }
    pub fn error_sink(&self) -> Arc<Mutex<Option<String>>> {
        self.error.clone()
    }
    pub fn render(&mut self, output: &mut [f32], channels: usize, rate: u32, delay: Duration) {
        self.render_inner(output, channels, rate, delay, false);
    }
    /// A dedicated blocking output worker may wait for the bounded PCM copy.
    /// Decoding and IO never hold the queue lock. This avoids replacing a
    /// whole device block with silence when the producer is briefly preempted.
    pub fn render_device(
        &mut self,
        output: &mut [f32],
        channels: usize,
        rate: u32,
        delay: Duration,
    ) {
        self.render_inner(output, channels, rate, delay, true);
    }
    fn render_inner(
        &mut self,
        output: &mut [f32],
        channels: usize,
        rate: u32,
        delay: Duration,
        wait_for_pcm: bool,
    ) {
        output.fill(0.0);
        if channels == 0 || rate == 0 {
            return;
        }
        let Some(world) = self.world.upgrade() else {
            return;
        };
        if self.voice_revision != world.voice_revision.load(Ordering::Acquire)
            && let Ok(voices) = world.voices.try_lock()
        {
            self.voices.clear();
            self.voices.extend(voices.iter().cloned());
            // Publish the revision only after a successful snapshot. A
            // contended registry must be retried on the next callback.
            self.voice_revision = world.voice_revision.load(Ordering::Relaxed);
        }
        if self.voices.is_empty() {
            return;
        }
        let mut mixed = false;
        let start = world.origin.elapsed() + delay;
        let sample_period = Duration::from_secs_f64(1.0 / rate as f64);
        for voice in self.voices.iter().filter_map(Weak::upgrade) {
            if !voice.alive.load(Ordering::Acquire)
                || !voice.playing.load(Ordering::Acquire)
                || voice.paused.load(Ordering::Relaxed)
            {
                continue;
            }
            let queue = if wait_for_pcm {
                voice.queue.lock().ok()
            } else {
                voice.queue.try_lock().ok()
            };
            let Some(mut q) = queue else {
                voice.underruns.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let gain = voice.gain.load(Ordering::Relaxed) as f32 / 100000.0;
            let pan = voice.pan.load(Ordering::Relaxed) as f32 / 100000.0;
            let gains = [gain * (1.0 - pan.max(0.0)), gain * (1.0 + pan.min(0.0))];
            let ratio = voice.frequency.load(Ordering::Relaxed).max(1) as f64 / rate as f64;
            let mut last_position = None;
            let mut last_index = 0;
            if q.visual.is_none() && voice.info.labels.is_empty() {
                (last_position, last_index) = if ratio == 1.0 && (q.phase == 0.0 || q.phase == 1.0)
                {
                    q.mix_native(&voice, output, channels, gains, start, rate)
                } else {
                    q.mix_resampled(
                        &voice,
                        output,
                        mixing::OutputFormat { channels, rate },
                        gains,
                        start,
                        ratio,
                    )
                };
            } else {
                'frames: for (index, out) in output.chunks_exact_mut(channels).enumerate() {
                    // Most frames have no timestamp consumer. Resolve once on
                    // demand for labels, visualization, position stamps or EOF;
                    // keep the original absolute-index rounding (no clock drift).
                    let mut timestamp = None;
                    let mut at = || {
                        *timestamp.get_or_insert_with(|| {
                            start + Duration::from_secs_f64(index as f64 / rate as f64)
                        })
                    };
                    while q.phase >= 1.0 && !q.frames.is_empty() {
                        let frame = *q.frames.front().unwrap();
                        if !q.labels(frame, &mut at) {
                            break 'frames;
                        }
                        q.frames.pop_front();
                        if let Some(visual) = &mut q.visual {
                            visual.push(frame.pcm, at());
                        }
                        q.labels_sent = false;
                        q.phase -= 1.0;
                    }
                    let Some(a) = q.frames.front().copied() else {
                        if q.eof {
                            voice.ended.store(true, Ordering::Release);
                            voice.playing.store(false, Ordering::Release);
                            q.end_at = Some(at());
                        } else {
                            voice.underruns.fetch_add(1, Ordering::Relaxed);
                        }
                        break;
                    };
                    if !q.labels(a, &mut at) {
                        break;
                    }
                    let b = q.frames.get(1).copied().unwrap_or(a);
                    let t = q.phase as f32;
                    let left = (a.sample[0] + (b.sample[0] - a.sample[0]) * t) * gains[0];
                    let right = (a.sample[1] + (b.sample[1] - a.sample[1]) * t) * gains[1];
                    if channels == 1 {
                        out[0] += (left + right) * 0.5;
                    } else {
                        out[0] += left;
                        out[1] += right;
                    }
                    if index % 64 == 0 || last_position.is_some_and(|p| a.position < p) {
                        if q.stamps.len() == 512 {
                            q.stamps.pop_front();
                        }
                        q.stamps.push_back(Stamp {
                            at: at(),
                            position: a.position,
                        });
                    }
                    last_position = Some(a.position);
                    last_index = index;
                    q.phase += ratio;
                }
            }
            if let Some(position) = last_position {
                mixed = true;
                voice.submitted.store(position, Ordering::Relaxed);
                if q.stamps.len() == 512 {
                    q.stamps.pop_front();
                }
                q.stamps.push_back(Stamp {
                    at: start
                        + Duration::from_secs_f64(last_index as f64 / rate as f64)
                        + sample_period,
                    position,
                });
            }
            let refill = q.frames.len() < FRAMES && !q.eof;
            drop(q);
            if refill && voice.worker.get().is_some() {
                // Never take a worker mutex on the realtime output thread,
                // and release the PCM lock before making the producer runnable.
                voice.wake();
            }
        }
        if mixed {
            for sample in output {
                *sample = sample.clamp(-1.0, 1.0);
            }
        }
    }
}

#[cfg(test)]
#[path = "../tests/internal/registry_contention.rs"]
mod registry_contention;

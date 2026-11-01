//! Vita mixer: bounded command batches, sample-accurate 48 kHz voices and AudioOut handoff.
use super::*;
#[cfg(target_os = "vita")]
use crate::nopus_stream::NintendoStream;
#[cfg(target_os = "vita")]
use knife_platform_vita::{
    audio_decode::{AudioStream, CHUNK_FRAMES},
    audio_output::{AudioOutput, OutputError},
};
#[cfg(target_os = "vita")]
use std::{io::Read, path::PathBuf};
use std::{
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread::{self, JoinHandle},
    time::Duration,
};
#[cfg(not(target_os = "vita"))]
const CHUNK_FRAMES: usize = 960;
const MAX_VOICES: usize = 64;
const MAX_PENDING_VOICES: usize = 64;
const OUTPUT_QUEUE_BLOCKS: usize = 8;
const SAMPLE_TIME: f64 = 1.0 / 48_000.0;
const MOVIE_QUEUE_BLOCKS: usize = 4;
struct MovieChunk { samples: [[f32; 2]; CHUNK_FRAMES], frames: usize }
pub struct MovieAudioSink { sender: SyncSender<MovieChunk>, pending: Option<MovieChunk> }
impl MovieAudioSink {
    pub fn ready(&mut self) -> Result<bool, &'static str> {
        if let Some(chunk) = self.pending.take() {
            match self.sender.try_send(chunk) {
                Ok(()) => {},
                Err(TrySendError::Full(chunk)) => self.pending = Some(chunk),
                Err(TrySendError::Disconnected(_)) => return Err("movie audio worker exited"),
            }
        }
        Ok(self.pending.is_none())
    }
    pub fn submit(&mut self, frames: &[[f32; 2]]) -> Result<(), &'static str> {
        if frames.is_empty() || frames.len() > CHUNK_FRAMES || !self.ready()? { return Err("movie sink not ready"); }
        let mut samples = [[0.0; 2]; CHUNK_FRAMES];
        samples[..frames.len()].copy_from_slice(frames);
        let chunk = MovieChunk { samples, frames: frames.len() };
        match self.sender.try_send(chunk) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(chunk)) => { self.pending = Some(chunk); Ok(()) },
            Err(TrySendError::Disconnected(_)) => Err("movie audio worker exited"),
        }
    }
}
struct MovieSound {
    receiver: Receiver<MovieChunk>, current: Option<MovieChunk>, cursor: usize,
    ctl: Arc<Mutex<Control>>, finished: Arc<AtomicBool>, done: bool,
}
impl MovieSound {
    fn process_frames(&mut self, mix: &mut [[f32; 2]; CHUNK_FRAMES]) {
        let mut ctl = self.ctl.lock();
        for dst in mix.iter_mut() {
            if ctl.stop_now { self.done = true; break; }
            if ctl.paused { continue; }
            if self.current.as_ref().is_none_or(|c| self.cursor >= c.frames) {
                match self.receiver.try_recv() {
                    Ok(chunk) => { self.current = Some(chunk); self.cursor = 0; },
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => { self.done = true; break; }
                }
            }
            let sample = self.current.as_ref().unwrap().samples[self.cursor];
            self.cursor += 1;
            ctl.volume.t += SAMPLE_TIME;
            if ctl.stop_after_ramp && ctl.volume.done() && ctl.volume.value() <= 0.0 { self.done = true; break; }
            let v = ctl.curve.apply(ctl.volume.value() * ctl.gain);
            dst[0] += sample[0] * v;
            dst[1] += sample[1] * v;
        }
        if self.done { self.finished.store(true, Ordering::Release); }
    }
}


#[cfg(not(target_os = "vita"))]
mod host_output {
    use std::time::Duration;
    #[cfg(test)]
    pub(super) static FORCE_QUEUE_FULL: std::sync::Mutex<Option<std::thread::ThreadId>> =
        std::sync::Mutex::new(None);
    #[cfg(test)]
    pub(super) static QUEUE_FULL_HITS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);
    #[derive(Debug)]
    pub enum OutputError {
        QueueFull,
    }
    impl std::fmt::Display for OutputError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    pub struct AudioOutput;
    impl AudioOutput {
        pub fn open() -> Result<Self, OutputError> {
            Ok(Self)
        }
        pub fn try_submit(&self, _: &[i16]) -> Result<(), OutputError> {
            #[cfg(test)]
            if *FORCE_QUEUE_FULL.lock().unwrap() == Some(std::thread::current().id()) {
                QUEUE_FULL_HITS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(OutputError::QueueFull);
            }
            std::thread::sleep(Duration::from_millis(1));
            Ok(())
        }
        pub fn set_active(&self, _: bool) {}
        pub fn stop(self) -> Result<(), OutputError> {
            Ok(())
        }
    }
}
#[cfg(not(target_os = "vita"))]
use host_output::{AudioOutput, OutputError};

struct Start {
    source: Source,
    ctl: Arc<Mutex<Control>>,
    finished: Arc<AtomicBool>,
    looping: bool,
    start: usize,
}
enum Source {
    Movie { receiver: Receiver<MovieChunk> },
    Pcm {
        pcm: Arc<Pcm>,
        prefix_base: Option<usize>,
    },
    #[cfg(target_os = "vita")]
    Stream {
        path: PathBuf,
        loop_range: Option<(u64, u64)>,
    },
}
#[derive(Default)]
struct Batch {
    changes: Vec<(Arc<Mutex<Control>>, Change)>,
    starts: Vec<Start>,
}
enum Message {
    Batch(Batch),
    Shutdown,
}

pub struct Audio {
    sender: SyncSender<Message>,
    worker: Option<JoinHandle<()>>,
    queued: Vec<Start>,
    pending: Option<Batch>,
    staged: Staged,
    failure: Arc<Mutex<Option<String>>>,
}
impl Audio {
    pub fn try_new() -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(OUTPUT_QUEUE_BLOCKS);
        let (ready, started) = mpsc::sync_channel(1);
        let failure = Arc::new(Mutex::new(None));
        let worker_failure = failure.clone();
        let worker = thread::Builder::new()
            .name("knife-audio-mixer".into())
            .stack_size(128 * 1024)
            .spawn(move || mixer_worker(receiver, ready, worker_failure))
            .map_err(|e| format!("audio mixer spawn: {e}"))?;
        match started.recv() {
            Ok(Ok(())) => Ok(Self {
                sender,
                worker: Some(worker),
                queued: Vec::new(),
                pending: None,
                staged: Staged::default(),
                failure,
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err("audio mixer startup exited".into())
            }
        }
    }
    pub fn new() -> Self {
        Self::try_new().unwrap_or_else(|error| {
            tracing::error!("Vita audio disabled: {error}");
            let (sender, receiver) = mpsc::sync_channel(1);
            drop(receiver);
            Self {
                sender,
                worker: None,
                queued: Vec::new(),
                pending: None,
                staged: Staged::default(),
                failure: Arc::new(Mutex::new(Some(error))),
            }
        })
    }
    pub fn last_error(&self) -> Option<String> {
        self.failure.lock().clone()
    }
    pub fn flush(&mut self) {
        if let Some(pending) = self.pending.take() {
            match self.sender.try_send(Message::Batch(pending)) {
                Ok(()) => {}
                Err(TrySendError::Full(Message::Batch(pending))) => {
                    self.pending = Some(pending);
                    return;
                }
                Err(TrySendError::Disconnected(Message::Batch(pending))) => {
                    self.retire_batch(pending, "mixer worker exited");
                    return;
                }
                _ => unreachable!(),
            }
        }
        let changes = std::mem::take(&mut *self.staged.lock());
        let starts = std::mem::take(&mut self.queued);
        if changes.is_empty() && starts.is_empty() {
            return;
        }
        let batch = Batch { changes, starts };
        match self.sender.try_send(Message::Batch(batch)) {
            Ok(()) => {}
            Err(TrySendError::Full(Message::Batch(batch))) => self.pending = Some(batch),
            Err(TrySendError::Disconnected(Message::Batch(batch))) => {
                self.retire_batch(batch, "mixer worker exited")
            }
            _ => unreachable!(),
        }
    }
    fn retire_batch(&self, batch: Batch, message: &str) {
        *self.failure.lock() = Some(message.into());
        for start in batch.starts {
            start.finished.store(true, Ordering::Release);
        }
        for (ctl, _) in batch.changes {
            ctl.lock().stop_now = true;
        }
    }
    fn start(
        &mut self,
        source: Source,
        volume: f32,
        fade_in: f64,
        looping: bool,
        start: usize,
        curve: Curve,
        gain: f32,
        pan: f32,
    ) -> Voice {
        let ctl = Arc::new(Mutex::new(Control {
            volume: Ramp {
                from: if fade_in > 0.0 { 0.0 } else { volume },
                to: volume,
                len: fade_in,
                t: 0.0,
            },
            gain,
            curve,
            pan: pan.clamp(-1.0, 1.0),
            stop_after_ramp: false,
            stop_now: false,
            paused: false,
            swap: None,
        }));
        let finished = Arc::new(AtomicBool::new(false));
        let voice = Voice {
            ctl: ctl.clone(),
            finished: finished.clone(),
            staged: self.staged.clone(),
        };
        if self.queued.len() >= MAX_PENDING_VOICES {
            finished.store(true, Ordering::Release);
            *self.failure.lock() = Some("pending audio voices exceed 64".into());
        } else {
            self.queued.push(Start {
                source,
                ctl,
                finished,
                looping,
                start,
            });
        }
        voice
    }
    pub fn play(&mut self, pcm: Arc<Pcm>, volume: f32, fade_in: f64, looping: bool) -> Voice {
        self.play_at(pcm, volume, fade_in, looping, 0, Curve::Linear)
    }
    pub fn play_at(
        &mut self,
        pcm: Arc<Pcm>,
        volume: f32,
        fade_in: f64,
        looping: bool,
        start: usize,
        curve: Curve,
    ) -> Voice {
        self.start(
            Source::Pcm {
                pcm,
                prefix_base: None,
            },
            volume,
            fade_in,
            looping,
            start,
            curve,
            1.0,
            0.0,
        )
    }
    pub fn play_gain(
        &mut self,
        pcm: Arc<Pcm>,
        (volume, gain, pan): (f32, f32, f32),
        fade_in: f64,
        looping: bool,
        curve: Curve,
    ) -> Voice {
        self.start(
            Source::Pcm {
                pcm,
                prefix_base: None,
            },
            volume,
            fade_in,
            looping,
            0,
            curve,
            gain,
            pan,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn play_prefix(
        &mut self,
        pcm: Arc<Pcm>,
        base: usize,
        volume: f32,
        fade_in: f64,
        looping: bool,
        start: usize,
        curve: Curve,
    ) -> Voice {
        self.start(
            Source::Pcm {
                pcm,
                prefix_base: Some(base),
            },
            volume,
            fade_in,
            looping,
            start,
            curve,
            1.0,
            0.0,
        )
    }
    #[cfg(target_os = "vita")]
    pub fn queue_movie_audio(&mut self, volume: f32) -> (Voice, MovieAudioSink) {
        let (sender, receiver) = mpsc::sync_channel(MOVIE_QUEUE_BLOCKS);
        let voice = self.start(Source::Movie { receiver }, volume, 0.0, false, 0, Curve::Linear, 1.0, 0.0);
        (voice, MovieAudioSink { sender, pending: None })
    }
    /// Queues a compressed file; opening/decoding happens only on the mixer thread.
    /// A returned Voice is pending, and last_error/finished report asynchronous failure.
    #[cfg(target_os = "vita")]
    #[allow(clippy::too_many_arguments)]
    pub fn queue_stream(
        &mut self,
        path: PathBuf,
        volume: f32,
        fade_in: f64,
        looping: bool,
        start: u64,
        loop_range: Option<(u64, u64)>,
        curve: Curve,
    ) -> Result<Voice, String> {
        let start = usize::try_from(start).map_err(|_| "stream start exceeds address space")?;
        if let Some((a, b)) = loop_range
            && (a >= b || b > i64::MAX as u64)
        {
            return Err("invalid stream loop range".into());
        }
        if path.as_os_str().is_empty() || self.queued.len() >= MAX_PENDING_VOICES {
            return Err("stream path empty or mixer queue full".into());
        }
        Ok(self.start(
            Source::Stream { path, loop_range },
            volume,
            fade_in,
            looping,
            start,
            curve,
            1.0,
            0.0,
        ))
    }
}
impl Default for Audio {
    fn default() -> Self {
        Self::new()
    }
}
impl Drop for Audio {
    fn drop(&mut self) {
        for start in self.queued.drain(..) {
            start.finished.store(true, Ordering::Release);
        }
        if let Some(batch) = self.pending.take() {
            self.retire_batch(batch, "audio shutdown");
        }
        let _ = self.sender.send(Message::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Mixer {
    voices: Vec<Active>,
}
enum Active {
    Pcm(PcmSound),
    Movie(MovieSound),
    #[cfg(target_os = "vita")]
    Stream(StreamSound),
}
impl Mixer {
    fn new() -> Self {
        Self { voices: Vec::new() }
    }
    fn apply(&mut self, batch: Batch, failure: &Arc<Mutex<Option<String>>>) {
        for (ctl, change) in batch.changes {
            change(&mut ctl.lock());
        }
        for start in batch.starts {
            if self.voices.len() >= MAX_VOICES {
                start.finished.store(true, Ordering::Release);
                *failure.lock() = Some("active audio voices exceed 64".into());
                continue;
            }
            match start.source {
                Source::Movie { receiver } => self.voices.push(Active::Movie(MovieSound { receiver, current: None, cursor: 0, ctl: start.ctl, finished: start.finished, done: false })),
                Source::Pcm { pcm, prefix_base } => self.voices.push(Active::Pcm(PcmSound {
                    pcm,
                    partial: prefix_base.is_some(),
                    base: prefix_base.unwrap_or(0),
                    pos: start.start as f64,
                    looping: start.looping,
                    ctl: start.ctl,
                    finished: start.finished,
                    done: false,
                    kernel: None,
                })),
                #[cfg(target_os = "vita")]
                Source::Stream { path, loop_range } => {
                    match StreamSound::spawn(
                        path,
                        loop_range,
                        start.ctl,
                        start.finished,
                        start.start,
                        start.looping,
                    ) {
                        Ok(sound) => self.voices.push(Active::Stream(sound)),
                        Err((error, finished)) => {
                            finished.store(true, Ordering::Release);
                            *failure.lock() = Some(error);
                        }
                    }
                }
            }
        }
    }
    fn render(
        &mut self,
        output: &mut [i16; CHUNK_FRAMES * 2],
        #[allow(unused_variables)] failure: &Arc<Mutex<Option<String>>>,
    ) -> u32 {
        let mut mix = [[0.0f32; 2]; CHUNK_FRAMES];
        for voice in &mut self.voices {
            match voice {
                Active::Movie(sound) => sound.process_frames(&mut mix),
                Active::Pcm(sound) => sound.process_frames(CHUNK_FRAMES, SAMPLE_TIME, |i, s| {
                    mix[i][0] += s[0];
                    mix[i][1] += s[1];
                }),
                #[cfg(target_os = "vita")]
                Active::Stream(sound) => {
                    if let Err(error) = sound.process_frames(&mut mix) {
                        sound.finished.store(true, Ordering::Release);
                        sound.done = true;
                        *failure.lock() = Some(error);
                    }
                }
            }
        }
        self.voices.retain(|voice| match voice {
            Active::Pcm(sound) => !sound.done,
            Active::Movie(sound) => !sound.done,
            #[cfg(target_os = "vita")]
            Active::Stream(sound) => !sound.done,
        });
        let mut clipped = 0;
        for (frame, dst) in mix.iter().zip(output.chunks_exact_mut(2)) {
            for ch in 0..2 {
                if frame[ch].is_finite() && frame[ch].abs() > 1.0 { clipped += 1; }
                let v = if frame[ch].is_finite() {
                    frame[ch].clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                dst[ch] = (v * 32767.0).round() as i16;
            }
        }
        clipped
    }
}

#[cfg(target_os = "vita")]
enum StreamDecoder {
    Ogg(AudioStream<'static>),
    Nintendo(NintendoStream),
}
#[cfg(target_os = "vita")]
impl StreamDecoder {
    fn open(path: &std::path::Path) -> Result<Self, String> {
        let _probe = knife_perf::Scope::new(knife_perf::Metric::AudioStreamOpen);
        // Manifest-backed Ogg names can go straight to the validating decoder.
        if path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("ogg")) {
            let name = path.to_str().ok_or("stream path is not UTF-8")?;
            return AudioStream::open_file(name).map(Self::Ogg).map_err(|e| e.to_string());
        }
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let mut magic = [0u8; 4];
        file.read_exact(&mut magic).map_err(|e| e.to_string())?;
        if &magic == b"OggS" {
            let name = path.to_str().ok_or("stream path is not UTF-8")?;
            AudioStream::open_file(name)
                .map(Self::Ogg)
                .map_err(|e| e.to_string())
        } else {
            NintendoStream::open(file)
                .map(Self::Nintendo)
                .map_err(|e| e.to_string())
        }
    }
    fn channels(&self) -> u32 {
        match self {
            Self::Ogg(s) => s.channels(),
            Self::Nintendo(s) => s.channels(),
        }
    }
    fn total_frames(&self) -> u64 {
        match self {
            Self::Ogg(s) => s.total_frames(),
            Self::Nintendo(s) => s.total_frames(),
        }
    }
    fn seek(&mut self, frame: u64) -> Result<(), String> {
        match self {
            Self::Ogg(s) => s.seek(frame).map_err(|e| e.to_string()),
            Self::Nintendo(s) => s.seek(frame).map_err(|e| e.to_string()),
        }
    }
    fn read(&mut self, out: &mut [[f32; 2]]) -> Result<usize, String> {
        match self {
            Self::Ogg(s) => s.read(out).map_err(|e| e.to_string()),
            Self::Nintendo(s) => s.read(out).map_err(|e| e.to_string()),
        }
    }
}
#[cfg(any(test, target_os = "vita"))]
#[path = "stream_pump.rs"]
mod stream_pump;
#[cfg(target_os = "vita")]
struct StreamCursor {
    decoder: StreamDecoder,
    pcm: [[f32; 2]; CHUNK_FRAMES], cursor: usize, filled: usize,
    position: u64, loop_range: Option<(u64, u64)>, looping: bool, mono: bool,
}
#[cfg(target_os = "vita")]
impl StreamCursor {
    fn open(path: PathBuf, loop_range: Option<(u64, u64)>, start: usize, looping: bool)
        -> Result<Self, String>
    {
        let mut decoder = StreamDecoder::open(&path)?;
        let loop_range = loop_range.map(|(a,b)| (a.min(decoder.total_frames()),b.min(decoder.total_frames())));
        if loop_range.is_some_and(|(a,b)| a >= b) { return Err("stream loop range outside source".into()); }
        if start as u64 > decoder.total_frames() { return Err("stream start exceeds source".into()); }
        // Both decoders finish opening at logical PCM frame zero (including
        // pre-skip). Re-seeking a fresh stream repeats demux/decoder work.
        // Nonzero restore positions and later loop rewinds keep exact seeking.
        if start != 0 { decoder.seek(start as u64)?; }
        let mono = decoder.channels() == 1;
        Ok(Self { decoder, pcm: [[0.0;2];CHUNK_FRAMES], cursor:0, filled:0,
            position:start as u64, loop_range, looping, mono })
    }
    fn sample(&mut self) -> Result<Option<[f32; 2]>, String> {
        if self.looping {
            let (from, to) = self.loop_range.unwrap_or((0, self.decoder.total_frames()));
            if to <= from {
                return Err("empty loop range".into());
            }
            if self.position >= to {
                self.decoder.seek(from).map_err(|e| e.to_string())?;
                self.position = from;
                self.cursor = self.filled;
            }
        }
        if self.cursor == self.filled {
            self.filled = self
                .decoder
                .read(&mut self.pcm)
                .map_err(|e| e.to_string())?;
            self.cursor = 0;
            if self.filled == 0 {
                if !self.looping {
                    return Ok(None);
                }
                let from = self.loop_range.map_or(0, |x| x.0);
                self.decoder.seek(from).map_err(|e| e.to_string())?;
                self.position = from;
                self.filled = self
                    .decoder
                    .read(&mut self.pcm)
                    .map_err(|e| e.to_string())?;
                if self.filled == 0 {
                    return Err("empty Opus stream loop".into());
                }
            }
        }
        let value = self.pcm[self.cursor];
        self.cursor += 1;
        self.position += 1;
        Ok(Some(value))
    }
}
#[cfg(target_os = "vita")]
impl stream_pump::Reader for StreamCursor {
    fn mono(&self) -> bool { self.mono }
    fn read(&mut self, out: &mut [[f32;2]]) -> Result<usize,String> {
        let mut n=0;
        for dst in out { let Some(value)=self.sample()? else { break }; *dst=value; n+=1; }
        Ok(n)
    }
}
#[cfg(target_os = "vita")]
struct StreamSound {
    pump: stream_pump::Pump, ctl: Arc<Mutex<Control>>, finished: Arc<AtomicBool>, done: bool,
}
#[cfg(target_os = "vita")]
impl StreamSound {
    fn spawn(path: PathBuf, loop_range: Option<(u64,u64)>, ctl: Arc<Mutex<Control>>,
        finished: Arc<AtomicBool>, start: usize, looping: bool)
        -> Result<Self,(String,Arc<AtomicBool>)>
    {
        let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let pump=stream_pump::Pump::spawn_named(&name, move || StreamCursor::open(path,loop_range,start,looping))
            .map_err(|error|(error,finished.clone()))?;
        Ok(Self { pump,ctl,finished,done:false })
    }
    fn process_frames(&mut self, mix: &mut [[f32;2];CHUNK_FRAMES]) -> Result<(),String> {
        let control=self.ctl.clone(); let mut ctl=control.lock();
        if ctl.paused { self.pump.suspend_wait(); }
        let mut matrix = None;
        for (index, dst) in mix.iter_mut().enumerate() {
            if self.done || ctl.stop_now { self.done=true; break; }
            if ctl.paused { continue; }
            let (sample,mono)=match self.pump.sample() {
                stream_pump::Poll::Sample(sample,mono)=>(sample,mono),
                stream_pump::Poll::Pending=>{
                    self.pump.missing_frames(CHUNK_FRAMES - index);
                    // A stopped/fading pending channel must not live forever
                    // merely because its file worker has not produced PCM yet.
                    if ctl.stop_after_ramp {
                        ctl.volume.t += SAMPLE_TIME * (CHUNK_FRAMES - index) as f64;
                        if ctl.volume.done() && ctl.volume.value() <= 0.0 { self.done = true; }
                    }
                    break; // other tracks still mix this block
                },
                stream_pump::Poll::End=>{self.done=true;break;},
                stream_pump::Poll::Failed(error)=>return Err(error),
            };
            ctl.volume.t+=SAMPLE_TIME;
            if ctl.stop_after_ramp && ctl.volume.done() && ctl.volume.value()<=0.0 {self.done=true;break;}
            let pan=*matrix.get_or_insert_with(|| pan_matrix(mono,ctl.pan));
            let v=ctl.curve.apply(ctl.volume.value()*ctl.gain);
            dst[0]+=(sample[0]*pan[0][0]+sample[1]*pan[1][0])*v;
            dst[1]+=(sample[0]*pan[0][1]+sample[1]*pan[1][1])*v;
        }
        if self.done { self.finished.store(true,Ordering::Release); }
        Ok(())
    }
}

fn mixer_worker(
    receiver: Receiver<Message>,
    ready: SyncSender<Result<(), String>>,
    failure: Arc<Mutex<Option<String>>>,
) {
    let output = match AudioOutput::open() {
        Ok(output) => {
            let _ = ready.send(Ok(()));
            output
        }
        Err(error) => {
            let error = error.to_string();
            *failure.lock() = Some(error.clone());
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut mixer = Mixer::new();
    let mut buffer = [0i16; CHUNK_FRAMES * 2];
    let mut running = true;
    while running {
        if mixer.voices.is_empty() {
            output.set_active(false);
            match receiver.recv_timeout(Duration::from_millis(10)) {
                Ok(Message::Batch(batch)) => mixer.apply(batch, &failure),
                Ok(Message::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
            }
        }
        loop {
            match receiver.try_recv() {
                Ok(Message::Batch(batch)) => mixer.apply(batch, &failure),
                Ok(Message::Shutdown) | Err(TryRecvError::Disconnected) => {
                    running = false;
                    break;
                }
                Err(TryRecvError::Empty) => break,
            }
        }
        if !running || mixer.voices.is_empty() {
            continue;
        }
        output.set_active(true);
        #[cfg(target_os = "vita")]
        let mix_started = std::time::Instant::now();
        let _clipped = mixer.render(&mut buffer, &failure);
        #[cfg(target_os = "vita")]
        knife_platform_vita::audio_output::metrics::record_mix(
            mix_started.elapsed().as_micros().min(u32::MAX as u128) as u32, _clipped);
        loop {
            match output.try_submit(&buffer) {
                Ok(()) => break,
                Err(OutputError::QueueFull) => {
                    match receiver.try_recv() {
                        Ok(Message::Batch(batch)) => mixer.apply(batch, &failure),
                        Ok(Message::Shutdown) | Err(TryRecvError::Disconnected) => {
                            running = false;
                            break;
                        }
                        Err(TryRecvError::Empty) => {}
                    }
                    thread::sleep(Duration::from_millis(1));
                }
                #[cfg(target_os = "vita")]
                Err(error) => {
                    *failure.lock() = Some(error.to_string());
                    running = false;
                    break;
                }
            }
        }
    }
    while let Ok(message) = receiver.try_recv() {
        if let Message::Batch(batch) = message {
            for start in batch.starts {
                start.finished.store(true, Ordering::Release);
            }
        }
    }
    for voice in mixer.voices {
        match voice {
            Active::Pcm(sound) => sound.finished.store(true, Ordering::Release),
            Active::Movie(sound) => sound.finished.store(true, Ordering::Release),
            #[cfg(target_os = "vita")]
            Active::Stream(sound) => sound.finished.store(true, Ordering::Release),
        }
    }
    if let Err(error) = output.stop() {
        *failure.lock() = Some(error.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pcm(frames: &[[f32; 2]]) -> Arc<Pcm> {
        Arc::new(Pcm {
            sample_rate: 48000,
            frames: frames.to_vec(),
            loop_range: None,
            mono: false,
        })
    }
    #[test]
    fn movie_queue_is_bounded_and_disconnect_is_error() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut sink = MovieAudioSink { sender, pending: None };
        assert!(sink.ready().unwrap());
        sink.submit(&[[0.25, -0.25]; CHUNK_FRAMES]).unwrap();
        sink.submit(&[[0.5, 0.5]; CHUNK_FRAMES]).unwrap();
        assert!(!sink.ready().unwrap());
        assert!(sink.submit(&[[0.1, 0.1]; 1]).is_err());
        assert_eq!(receiver.try_recv().unwrap().frames, CHUNK_FRAMES);
        assert!(sink.ready().unwrap());
        assert_eq!(receiver.try_recv().unwrap().samples[0], [0.5, 0.5]);
        drop(receiver);
        assert!(sink.ready().is_ok()); // a disconnected empty sender is learned on submit
        assert!(sink.submit(&[[0.1, 0.1]; 1]).is_err());
    }
    #[test]
    fn movie_mix_pause_eof_stop_and_concurrent_pcm() {
        let mut audio = Audio::try_new().unwrap();
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut sink = MovieAudioSink { sender, pending: None };
        let voice = audio.start(Source::Movie { receiver }, 0.5, 0.0, false, 0, Curve::Linear, 1.0, 0.0);
        let pcm_voice = audio.play(pcm(&[[0.25, 0.0]; 3]), 1.0, 0.0, false);
        let starts = std::mem::take(&mut audio.queued);
        let mut mixer = Mixer::new();
        let failure = Arc::new(Mutex::new(None));
        mixer.apply(Batch { starts, changes: vec![] }, &failure);
        sink.submit(&[[0.5, -0.5]; 3]).unwrap();
        voice.set_paused(true);
        let changes = std::mem::take(&mut *audio.staged.lock());
        mixer.apply(Batch { starts: vec![], changes }, &failure);
        let mut block = [0i16; CHUNK_FRAMES * 2];
        mixer.render(&mut block, &failure);
        assert_ne!(block[0], 0); // independent PCM advances during movie pause
        assert_eq!(block[1], 0);
        assert!(pcm_voice.finished());
        voice.set_paused(false);
        let changes = std::mem::take(&mut *audio.staged.lock());
        mixer.apply(Batch { starts: vec![], changes }, &failure);
        mixer.render(&mut block, &failure);
        assert_eq!(block[0], 8192);
        assert_eq!(block[1], -8192);
        drop(sink);
        mixer.render(&mut block, &failure);
        assert!(voice.finished());
        assert!(mixer.voices.is_empty());
        assert!(failure.lock().is_none());
    }
    #[test]
    fn batch_volume_pan_pause_and_finished() {
        let mut audio = Audio::try_new().unwrap();
        let voice = audio.play(pcm(&[[0.5, 0.25]; 5000]), 1.0, 0.0, false);
        voice.set_gain(0.5);
        voice.set_pan(1.0);
        voice.set_paused(true);
        assert_eq!(voice.volume(), 1.0);
        audio.flush();
        assert!(!voice.finished());
        voice.set_paused(false);
        audio.flush();
        for _ in 0..200 {
            if voice.finished() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        assert!(voice.finished());
        assert_eq!(audio.last_error(), None);
    }
    #[test]
    fn prefix_swap_and_stop_are_committed_by_flush() {
        let mut mixer = Mixer::new();
        let failure = Arc::new(Mutex::new(None));
        let mut audio = Audio::try_new().unwrap();
        let voice = audio.play_prefix(
            pcm(&[[0.2, 0.3]; 100]),
            0,
            1.0,
            0.0,
            false,
            0,
            Curve::Linear,
        );
        let first = std::mem::take(&mut audio.queued);
        mixer.apply(
            Batch {
                starts: first,
                changes: vec![],
            },
            &failure,
        );
        let mut block = [0i16; CHUNK_FRAMES * 2];
        mixer.render(&mut block, &failure);
        assert!(block[..160].iter().any(|&x| x != 0));
        assert!(block[200..].iter().all(|&x| x == 0));
        voice.replace_source(pcm(&[[0.4, 0.2]; 1920]));
        voice.stop(0.0);
        let changes = std::mem::take(&mut *audio.staged.lock());
        mixer.apply(
            Batch {
                changes,
                starts: vec![],
            },
            &failure,
        );
        mixer.render(&mut block, &failure);
        assert!(voice.finished());
        assert!(mixer.voices.is_empty());
    }
    #[test]
    fn gain_and_loop_mix_as_stereo_s16() {
        let mut audio = Audio::try_new().unwrap();
        let a = audio.play(pcm(&[[0.25, 0.0]; 960]), 1.0, 0.0, true);
        let b = audio.play(pcm(&[[0.0, 0.25]; 960]), 1.0, 0.0, true);
        let failure = Arc::new(Mutex::new(None));
        let mut mixer = Mixer::new();
        mixer.apply(
            Batch {
                starts: std::mem::take(&mut audio.queued),
                changes: vec![],
            },
            &failure,
        );
        let mut block = [0i16; CHUNK_FRAMES * 2];
        mixer.render(&mut block, &failure);
        assert!(block.chunks_exact(2).all(|f| f == [8192, 8192]));
        mixer.render(&mut block, &failure);
        assert!(!a.finished() && !b.finished());
        a.stop(0.0);
        b.stop(0.0);
        mixer.apply(
            Batch {
                changes: std::mem::take(&mut *audio.staged.lock()),
                starts: vec![],
            },
            &failure,
        );
        mixer.render(&mut block, &failure);
        assert!(a.finished() && b.finished());
    }
    #[test]
    fn pending_limit_and_drop_release_voices() {
        let mut audio = Audio::try_new().unwrap();
        let source = pcm(&[[0.2, 0.2]; 960]);
        let voices = (0..MAX_PENDING_VOICES)
            .map(|_| audio.play(source.clone(), 1.0, 0.0, true))
            .collect::<Vec<_>>();
        let rejected = audio.play(source, 1.0, 0.0, true);
        assert!(rejected.finished());
        assert!(audio.last_error().unwrap().contains("64"));
        drop(audio);
        assert!(voices.iter().all(Voice::finished));
    }
    #[test]
    fn paused_voice_renders_silence_then_resumes() {
        let mut audio = Audio::try_new().unwrap();
        let voice = audio.play(pcm(&[[0.5, 0.25]; 1920]), 1.0, 0.0, false);
        voice.set_paused(true);
        let mut mixer = Mixer::new();
        let failure = Arc::new(Mutex::new(None));
        mixer.apply(
            Batch {
                starts: std::mem::take(&mut audio.queued),
                changes: std::mem::take(&mut *audio.staged.lock()),
            },
            &failure,
        );
        let mut block = [0i16; CHUNK_FRAMES * 2];
        mixer.render(&mut block, &failure);
        assert!(block.iter().all(|&v| v == 0));
        voice.set_paused(false);
        mixer.apply(
            Batch {
                starts: vec![],
                changes: std::mem::take(&mut *audio.staged.lock()),
            },
            &failure,
        );
        mixer.render(&mut block, &failure);
        assert_eq!(&block[..2], &[16384, 8192]);
    }
    #[test]
    fn shutdown_completes_when_output_queue_stays_full() {
        let mut audio = Audio::try_new().unwrap();
        host_output::QUEUE_FULL_HITS.store(0, Ordering::SeqCst);
        *host_output::FORCE_QUEUE_FULL.lock().unwrap() =
            Some(audio.worker.as_ref().unwrap().thread().id());
        let voice = audio.play(pcm(&[[0.1, 0.1]; 960]), 1.0, 0.0, true);
        audio.flush();
        for _ in 0..200 {
            if host_output::QUEUE_FULL_HITS.load(Ordering::SeqCst) > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            host_output::QUEUE_FULL_HITS.load(Ordering::SeqCst) > 0,
            "fault injection reached output retry"
        );
        let (tx, rx) = mpsc::sync_channel(1);
        let join = thread::spawn(move || {
            drop(audio);
            let _ = tx.send(());
        });
        let completed = rx.recv_timeout(Duration::from_millis(200)).is_ok();
        *host_output::FORCE_QUEUE_FULL.lock().unwrap() = None;
        if !completed {
            rx.recv_timeout(Duration::from_secs(2))
                .expect("release full queue allows worker to stop");
        }
        join.join().unwrap();
        assert!(voice.finished());
        assert!(
            completed,
            "mixer Drop must honor Shutdown while output remains QueueFull"
        );
    }
}

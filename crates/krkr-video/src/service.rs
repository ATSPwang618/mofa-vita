use super::*;
use krkr_audio::Pcm;
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
const QUEUED_FRAMES: usize = 4;

#[derive(Clone)]
pub struct Service {
    backend: Arc<Mutex<Option<Arc<dyn Backend>>>>,
    audio: krkr_audio::Service,
    budget: Budget,
}
impl Service {
    pub fn new(audio: krkr_audio::Service) -> Self {
        Self {
            backend: Arc::default(),
            audio,
            budget: Budget::new(256 * 1024 * 1024),
        }
    }
    pub fn set_backend(&self, backend: impl Backend + 'static) {
        *self.backend.lock().unwrap() = Some(Arc::new(backend));
    }
    pub fn trace(&self, event: &str) {
        let backend = self.backend.lock().unwrap().clone();
        if let Some(backend) = backend {
            backend.trace(event);
        }
    }
    /// Runs on the IO worker. Codec state is born on its own worker and never
    /// requires Send; open cancellation also interrupts AVIO probing.
    pub fn open(&self, plan: krkr_assets::ReadPlan, cancelled: &AtomicBool) -> Result<Handle> {
        self.open_with_audio(plan, cancelled, true)
    }
    /// Effect movies have no audio renderer; do not start a sound device or
    /// allow an unconsumed PCM stream to stall their clock.
    pub fn open_silent(
        &self,
        plan: krkr_assets::ReadPlan,
        cancelled: &AtomicBool,
    ) -> Result<Handle> {
        self.open_with_audio(plan, cancelled, false)
    }
    fn open_with_audio(
        &self,
        plan: krkr_assets::ReadPlan,
        cancelled: &AtomicBool,
        with_audio: bool,
    ) -> Result<Handle> {
        let backend = self
            .backend
            .lock()
            .unwrap()
            .clone()
            .ok_or("no video backend installed")?;
        let cancel = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(State::default()));
        let (tx, rx) = mpsc::sync_channel(4);
        let (ready, result) = mpsc::sync_channel(1);
        let worker_state = state.clone();
        let worker_cancel = cancel.clone();
        let wake_generation = Arc::new(WorkerNotify::default());
        let worker_wake = wake_generation.clone();
        let budget = self.budget.clone();
        let audio = self.audio.clone();
        let worker = std::thread::Builder::new()
            .name("krkr-video-decode".into())
            .spawn(move || {
                let opened = (|| {
                    *worker_wake.primitive.lock().unwrap() = Some(backend.worker_wake()?);
                    // Includes fixed PCM, demux IO and conversion scratch; decoded
                    // pixel planes carry separate tickets through GPU upload.
                    let scratch = budget.reserve(2 * 1024 * 1024).map_err(|e| e.to_string())?;
                    let decoder = if with_audio {
                        backend.open(plan, budget, worker_cancel.clone())?
                    } else {
                        backend.open_silent(plan, budget, worker_cancel.clone())?
                    };
                    let info = decoder.info().clone();
                    let pcm = if with_audio && info.audio_streams > 0 {
                        Some(Arc::new(audio.pcm(info.audio_rate)?))
                    } else {
                        None
                    };
                    Ok::<_, String>((decoder, info, pcm, scratch))
                })();
                match opened {
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                    Ok((decoder, info, pcm, _scratch)) => {
                        worker_state.lock().unwrap().audio = pcm.clone();
                        let info = Arc::new(Mutex::new(info));
                        backend.trace("worker: publishing open completion");
                        if ready.send(Ok(info.clone())).is_ok()
                            && let Err(error) = run(
                                decoder,
                                info,
                                worker_state.clone(),
                                worker_cancel,
                                rx,
                                pcm,
                                worker_wake,
                            )
                        {
                            let mut s = worker_state.lock().unwrap();
                            s.playing = false;
                            if let Some(pcm) = &s.audio {
                                pcm.play(false);
                            }
                            s.error = Some(error);
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let lease = Arc::new(Lease {
            state,
            cancel,
            commands: tx,
            worker: worker.thread().clone(),
            wake_generation,
        });
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Err("video open cancelled".into());
            }
            match krkr_protocol::channel::recv_timeout(&result, Duration::from_millis(10)) {
                Ok(info) => {
                    self.trace("IO: received open completion");
                    return Ok(Handle {
                        lease,
                        info: info?,
                        logical_size: None,
                    });
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("video worker stopped during open".into()),
            }
        }
    }
}
struct State {
    frames: VecDeque<Frame>,
    audio: Option<Arc<Pcm>>,
    audio_started: bool,
    playing: bool,
    base: f64,
    since: Instant,
    speed: f64,
    eof: bool,
    end: f64,
    error: Option<String>,
    generation: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            frames: VecDeque::with_capacity(QUEUED_FRAMES),
            audio: None,
            audio_started: false,
            playing: false,
            base: 0.0,
            since: Instant::now(),
            speed: 1.0,
            eof: false,
            end: 0.0,
            error: None,
            generation: 0,
        }
    }
}
impl State {
    fn position(&self, rate: u32) -> f64 {
        if !self.playing {
            return self.base;
        }
        if let Some(pcm) = &self.audio
            && self.audio_started
        {
            return pcm.position().played as f64 / rate as f64;
        }
        self.base + self.since.elapsed().as_secs_f64() * self.speed
    }
}
enum Command {
    Seek(f64, SyncSender<Result<()>>),
    Audio(Option<usize>, f64, SyncSender<Result<()>>),
    Video(usize, f64, SyncSender<Result<()>>),
}
#[derive(Default)]
struct WorkerNotify {
    generation: AtomicU64,
    primitive: Mutex<Option<Arc<dyn WorkerWake>>>,
}
struct Lease {
    state: Arc<Mutex<State>>,
    cancel: Arc<AtomicBool>,
    commands: SyncSender<Command>,
    worker: std::thread::Thread,
    wake_generation: Arc<WorkerNotify>,
}
impl Lease {
    fn wake(&self) {
        self.wake_generation
            .generation
            .fetch_add(1, Ordering::Release);
        if let Some(wake) = self.wake_generation.primitive.lock().unwrap().as_ref() {
            wake.signal();
        } else {
            self.worker.unpark();
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(audio) = &self.state.lock().unwrap().audio {
            audio.play(false);
        }
        self.wake();
    }
}
#[derive(Clone)]
pub struct Handle {
    lease: Arc<Lease>,
    info: Arc<Mutex<Info>>,
    logical_size: Option<Size>,
}
impl Handle {
    pub fn info(&self) -> Info {
        let mut info = self.info.lock().unwrap().clone();
        if let Some(size) = self.logical_size {
            info.size = size;
        }
        info
    }
    pub fn stored_size(&self) -> Size {
        self.info.lock().unwrap().size
    }
    pub fn with_logical_size(mut self, size: Size) -> Self {
        self.logical_size = Some(size);
        self
    }
    pub fn position(&self) -> f64 {
        let rate = self.info().audio_rate;
        self.lease.state.lock().unwrap().position(rate)
    }
    pub fn play(&self, play: bool) {
        let rate = self.info().audio_rate;
        let mut s = self.lease.state.lock().unwrap();
        s.base = s.position(rate);
        s.since = Instant::now();
        s.playing = play;
        if let Some(pcm) = &s.audio {
            pcm.play(play && s.audio_started);
        }
        self.lease.wake();
    }
    pub fn rate(&self, rate: f64) -> Result<()> {
        if !rate.is_finite() || !(0.1..=8.0).contains(&rate) {
            return Err("video playRate must be between 0.1 and 8".into());
        }
        let audio_rate = self.info().audio_rate;
        let mut s = self.lease.state.lock().unwrap();
        s.base = s.position(audio_rate);
        s.since = Instant::now();
        s.speed = rate;
        if let Some(pcm) = &s.audio {
            pcm.rate(rate);
        }
        self.lease.wake();
        Ok(())
    }
    pub fn gain(&self, gain: i32, pan: i32) {
        if let Some(pcm) = &self.lease.state.lock().unwrap().audio {
            pcm.gain(gain, pan);
        }
    }
    /// Latest due frame; dropping overdue frames keeps audio authoritative.
    /// While paused, seeking still presents the first decoded target frame.
    pub fn frame(&self) -> Option<Frame> {
        let rate = self.info().audio_rate;
        let mut s = self.lease.state.lock().unwrap();
        let time = s.position(rate);
        let mut frame = None;
        while s.frames.front().is_some_and(|f| f.time <= time + 0.001) {
            frame = s.frames.pop_front();
        }
        if frame.is_some() {
            self.lease.wake();
        }
        frame
    }
    /// One completed sample per continuous callback, retaining backpressure
    /// rather than draining several due samples without delivering callbacks.
    pub fn next_frame(&self) -> Option<Frame> {
        let rate = self.info().audio_rate;
        let mut s = self.lease.state.lock().unwrap();
        let time = s.position(rate);
        if s.frames.front().is_some_and(|f| f.time <= time + 0.001) {
            let frame = s.frames.pop_front();
            self.lease.wake();
            frame
        } else {
            None
        }
    }
    pub fn finished(&self) -> bool {
        let rate = self.info().audio_rate;
        let s = self.lease.state.lock().unwrap();
        s.eof
            && s.frames.is_empty()
            && s.position(rate) + 0.002 >= s.end
            && s.audio
                .as_ref()
                .is_none_or(|a| !s.audio_started || a.finished())
    }
    pub fn error(&self) -> Option<String> {
        self.lease.state.lock().unwrap().error.clone()
    }
    pub fn seek(&self, time: f64, cancelled: &AtomicBool) -> Result<()> {
        self.command(cancelled, |reply| Command::Seek(time.max(0.0), reply))
    }
    pub fn audio_stream(&self, stream: Option<usize>, cancelled: &AtomicBool) -> Result<()> {
        let time = self.position();
        self.command(cancelled, |reply| Command::Audio(stream, time, reply))
    }
    pub fn video_stream(&self, stream: usize, cancelled: &AtomicBool) -> Result<()> {
        let time = self.position();
        self.command(cancelled, |reply| Command::Video(stream, time, reply))
    }
    fn command(
        &self,
        cancelled: &AtomicBool,
        make: impl FnOnce(SyncSender<Result<()>>) -> Command,
    ) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.lease
            .commands
            .try_send(make(tx))
            .map_err(|_| "video command queue unavailable")?;
        self.lease.wake();
        loop {
            if cancelled.load(Ordering::Relaxed) {
                self.lease.cancel.store(true, Ordering::Release);
                self.lease.wake();
                return Err("video operation cancelled".into());
            }
            match krkr_protocol::channel::recv_timeout(&rx, Duration::from_millis(10)) {
                Ok(result) => return result,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return Err("video decoder has stopped".into()),
            }
        }
    }
}
fn run(
    mut decoder: Box<dyn Decoder>,
    shared_info: Arc<Mutex<Info>>,
    state: Arc<Mutex<State>>,
    cancel: Arc<AtomicBool>,
    commands: Receiver<Command>,
    pcm: Option<Arc<Pcm>>,
    wake_generation: Arc<WorkerNotify>,
) -> Result<()> {
    let mut info = shared_info.lock().unwrap().clone();
    let mut pending_audio: Option<(u64, Vec<[f32; 2]>, usize)> = None;
    let mut decoder_finished = false;
    let mut audio_cursor = 0u64;
    let mut audio_enabled = pcm.is_some();
    let mut discard_before = 0.0;
    while !cancel.load(Ordering::Acquire) {
        let wake_seen = wake_generation.generation.load(Ordering::Acquire);
        while let Ok(command) = commands.try_recv() {
            let (time, reply, selection, video) = match command {
                Command::Seek(t, r) => (t, r, None, None),
                Command::Audio(i, t, r) => (t, r, Some(i), None),
                Command::Video(i, t, r) => (t, r, None, Some(i)),
            };
            let result: Result<()> = (|| {
                if !time.is_finite() {
                    return Err("invalid video position".into());
                }
                if let Some(selection) = selection {
                    decoder.audio_stream(selection)?;
                    audio_enabled = selection.is_some();
                }
                if let Some(video) = video {
                    decoder.video_stream(video)?;
                    info = decoder.info().clone();
                    *shared_info.lock().unwrap() = info.clone();
                }
                if let Some(pcm) = &pcm {
                    pcm.play(false);
                }
                decoder.seek(time)?;
                let mut s = state.lock().unwrap();
                pending_audio = None;
                decoder_finished = false;
                audio_cursor = (time * info.audio_rate as f64) as u64;
                discard_before = time;
                s.frames.clear();
                s.eof = false;
                s.error = None;
                s.base = time;
                s.end = time;
                s.since = Instant::now();
                s.audio_started = false;
                s.generation += 1;
                if let Some(pcm) = &pcm {
                    pcm.flush((time * info.audio_rate as f64) as u64);
                }
                Ok(())
            })();
            if let Err(error) = &result {
                let mut s = state.lock().unwrap();
                s.playing = false;
                s.error = Some(error.clone());
            }
            let _ = reply.send(result);
        }
        {
            let mut s = state.lock().unwrap();
            // After the audio tail drains, continue the video tail from the
            // audible position instead of jumping to wall time since play().
            if s.playing && s.audio_started && pcm.as_ref().is_some_and(|p| p.finished()) {
                s.base = pcm.as_ref().unwrap().position().played as f64 / info.audio_rate as f64;
                s.since = Instant::now();
                s.audio_started = false;
            }
            if s.eof {
                // The PCM device does not wake this worker. Keep polling until
                // its audible tail ends and the video clock can take over.
                let poll_audio = s.playing && s.audio_started;
                drop(s);
                wait(&wake_generation, wake_seen, poll_audio)?;
                continue;
            }
        }
        if let Some((position, samples, offset)) = &mut pending_audio {
            if let Some(pcm) = &pcm {
                let target = audio_cursor;
                *offset = (*offset)
                    .max(target.saturating_sub(*position).min(samples.len() as u64) as usize);
                let position = *position + *offset as u64;
                let count = if position > audio_cursor {
                    let silence = [[0.0; 2]; 256];
                    pcm.push(
                        audio_cursor,
                        &silence[..(position - audio_cursor).min(256) as usize],
                    )
                } else {
                    let count = pcm.push(position, &samples[*offset..]);
                    *offset += count;
                    count
                };
                audio_cursor += count as u64;
                if count > 0 {
                    let mut s = state.lock().unwrap();
                    s.audio_started = true;
                    pcm.play(s.playing);
                }
                if *offset == samples.len() {
                    pending_audio = None;
                }
            } else {
                pending_audio = None;
            }
        }
        if decoder_finished {
            if pending_audio.is_none() {
                state.lock().unwrap().eof = true;
                if audio_enabled && let Some(pcm) = &pcm {
                    pcm.eof();
                }
            }
            wait(&wake_generation, wake_seen, true)?;
            continue;
        }
        let want_video = state.lock().unwrap().frames.len() < QUEUED_FRAMES;
        let (playing, rate) = {
            let s = state.lock().unwrap();
            (s.playing, s.speed)
        };
        decoder.playback(playing, rate)?;
        let want_audio =
            audio_enabled && pending_audio.is_none() && pcm.as_ref().is_some_and(|p| p.space() > 0);
        if !want_video && !want_audio {
            // Frame consumption and controls explicitly unpark us; only a
            // running PCM consumer can free space without sending a wakeup.
            wait(
                &wake_generation,
                wake_seen,
                playing && audio_enabled && pcm.is_some(),
            )?;
            continue;
        }
        match decoder.next(want_video, want_audio)? {
            None => decoder_finished = true,
            Some(Decoded::Pending) => wait(&wake_generation, wake_seen, true)?,
            Some(Decoded::Suspended) => wait(&wake_generation, wake_seen, false)?,
            Some(Decoded::AudioEnd) => {
                if let Some(pcm) = &pcm {
                    pcm.eof();
                }
            }
            Some(Decoded::Audio { position, samples }) => {
                pending_audio = Some((position, samples, 0))
            }
            Some(Decoded::Video(mut frame)) => {
                let mut s = state.lock().unwrap();
                s.end = s.end.max(frame.time + 1.0 / info.fps.max(1.0));
                if frame.time + 1.0 / info.fps.max(1.0) >= discard_before {
                    frame.time = frame.time.max(discard_before);
                    s.frames.push_back(frame);
                }
            }
        }
    }
    Ok(())
}

fn wait(notify: &WorkerNotify, seen: u64, polling: bool) -> Result<()> {
    // Codec IO may consume a thread park token. Preserve controls received
    // during decoding even when the backend uses the default thread wake.
    if notify.generation.load(Ordering::Acquire) != seen {
        return Ok(());
    }
    let wake = notify
        .primitive
        .lock()
        .unwrap()
        .clone()
        .expect("registered decoder wake");
    wake.wait(polling.then_some(Duration::from_millis(2)))
}

#[cfg(test)]
#[path = "../tests/internal/service.rs"]
mod tests;

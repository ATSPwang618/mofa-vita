//! Native lifecycle lives on one persistent owner; GXM leases new YVU frames
//! only until their owned 960x540 RGBA cache conversion completes.
use std::{path::Path, sync::{OnceLock, mpsc::{self, SyncSender}}, time::Instant};
use anyhow::{Context, Result, ensure};
use knife_platform_vita::movie::{MoviePlayer, MovieState, VideoFormat};
use knife_platform_vita::{movie_geometry::{self, VideoGeometry}, movie_view};
use knife_render::{MovieGpuFence, RenderTarget, Renderer, TexRef};
use knife_audio::MovieAudioSink;
use knife_perf::{Gauge, Metric, Scope};
#[path = "owner.rs"] mod owner;
use owner::{Event, Settings, Session};
fn service() -> Result<&'static SyncSender<owner::Open<MovieAudioSink>>> {
    static SERVICE: OnceLock<Result<SyncSender<owner::Open<MovieAudioSink>>, String>> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new().name("knife-movie-owner".into()).stack_size(512 * 1024)
            .spawn(move || owner::service(receiver, NativeDecoder::open))
            .map_err(|error| format!("movie owner spawn: {error}"))?;
        Ok(sender)
    }).as_ref().map_err(|error| anyhow::anyhow!(error.clone()))
}
pub struct Movie {
    session: Session<MovieAudioSink>, fence: MovieGpuFence,
    pub tex: TexRef, pub finished: bool, pub looping: bool, pub hold: bool,
    pub alpha: f32, pub closed: bool, pub loop_start: f32,
    has_frame: bool, fading_in: bool, close_steps: u8, ready_wait: u8,
    position_ms: u64, frame_serial: u64, starting: Option<Instant>, started: bool,
    cache: Option<RenderTarget>, source: Option<TexRef>, cache_bytes: u64,
    audio: Option<MovieAudioSink>, settings: Settings, sent: Option<Settings>, quarantined: bool,
}
impl Movie {
    pub fn open(r: &Renderer, path: &Path, looping: bool) -> Result<Self> {
        let name = path.to_str().context("movie path is not UTF-8")?;
        ensure!(name.len() <= 1024, "movie open path exceeds budget");
        let settings = Settings { looping, loop_ms: 0, seek_serial: 0, seek_ms: 0 };
        let session = Session::open(service()?, name.into(), settings).map_err(anyhow::Error::msg)?;
        Ok(Self { session, fence: r.movie_gpu_fence(), tex: r.white.clone(), finished: false,
            looping, hold: false, alpha: 0.0, closed: false, loop_start: 0.0, has_frame: false,
            fading_in: true, close_steps: 0, ready_wait: 4, position_ms: 0, frame_serial: 0,
            starting: Some(Instant::now()), started: false, cache: None, source: None, cache_bytes: 0,
            audio: None, settings, sent: None, quarantined: false })
    }
    fn flush(&mut self) {
        self.session.set_hold(self.hold);
        self.settings.looping = self.looping;
        self.settings.loop_ms = if self.loop_start.is_finite() { (self.loop_start.max(0.0) * 1000.0) as u64 } else { 0 };
        if self.sent == Some(self.settings) && self.audio.is_none() { return; }
        let update = owner::Update { settings: self.settings, audio: self.audio.take() };
        match self.session.update(update) { Ok(()) => self.sent = Some(self.settings), Err(update) => self.audio = update.audio }
    }
    pub fn starting(&self) -> bool { self.starting.is_some() && !self.finished }
    pub fn poll_start(&mut self) -> Result<bool> {
        let _probe = Scope::new(Metric::MovieStartPoll);
        self.flush();
        if self.started { return Ok(true); }
        match self.session.poll().map_err(anyhow::Error::msg)? {
            Some(Event::Started) => {
                self.started = true;
                if let Some(started) = self.starting.take() { knife_perf::add(Metric::MovieStartReady, 0, started.elapsed().as_micros() as u64); }
                Ok(true)
            }
            Some(Event::Failed(error)) => Err(anyhow::anyhow!(error)),
            Some(_) => Err(anyhow::anyhow!("movie data preceded READY")),
            None => { knife_perf::add(Metric::MovieStartPending, 0, 1); Ok(false) }
        }
    }
    pub fn set_audio(&mut self, audio: MovieAudioSink) { self.audio = Some(audio); }
    pub fn set_hold(&mut self, hold: bool) { self.hold = hold; self.session.set_hold(hold); }
    pub fn position(&self) -> f32 { self.position_ms as f32 / 1000.0 }
    pub fn ready(&self) -> bool { self.ready_wait == 0 && !self.finished }
    pub fn has_frame(&self) -> bool { self.has_frame }
    pub fn uv_rect(&self) -> [f32; 4] { [0.0, 0.0, 1.0, 1.0] }
    fn failed(&mut self, error: impl std::fmt::Display) {
        tracing::error!("movie owner/cache failure: {error}");
        self.session.cancel(); self.finished = true; self.starting = None; self.audio = None; self.fading_in = false;
    }
    fn cache_frame(&mut self, r: &Renderer, frame: owner::Frame) -> Result<()> {
        if !self.session.acquire(frame.token) { return Ok(()); }
        let result = (|| {
            if self.cache.is_none() {
                let target = r.try_create_target(960, 540, "movie-frame-cache")?;
                let [w, h] = target.tex.physical_size(); let bytes = u64::from(w) * u64::from(h) * 4;
                ensure!(bytes <= knife_perf::memory_budget::GPU_MOVIE_FRAME_BYTES as u64, "movie cache exceeds GPU budget");
                self.cache_bytes = bytes; knife_perf::gauge_add(Gauge::MovieFrameCacheBytes, bytes); self.cache = Some(target);
            }
            if let Some(source) = &self.source {
                if source.physical_size() == [frame.width, frame.height] {
                    // SAFETY: the actor retains this acquired native lease and
                    // the prior frame was ACKed only after GPU completion.
                    unsafe { r.try_movie_rebind(source, frame.pointer as *const u8, frame.width, frame.height) }?;
                } else {
                    self.source = Some(unsafe { r.try_movie_texture(frame.pointer as *const u8, frame.width, frame.height) }?);
                }
            } else {
                // SAFETY: no next decoder/control/Close occurs before our ACK.
                self.source = Some(unsafe { r.try_movie_texture(frame.pointer as *const u8, frame.width, frame.height) }?);
            }
            r.try_cache_movie_frame(self.source.as_ref().unwrap(), self.cache.as_ref().unwrap(), frame.uv)
        })();
        if result.is_err() && self.fence.finish().is_err() {
            self.quarantined = true; self.session.quarantine(); return result;
        }
        ensure!(self.session.ack(frame.token), "movie GPU ACK token mismatch");
        result?;
        self.tex = self.cache.as_ref().unwrap().tex.clone(); self.position_ms = frame.timestamp_ms;
        self.frame_serial = frame.seek_serial; self.has_frame = true;
        Ok(())
    }
    pub fn update(&mut self, r: &Renderer, dt: f32) {
        let frames = dt * 0.06;
        if self.finished {
            if self.alpha > 0.0 { self.alpha = (self.alpha - frames / 30.0).max(0.0); }
            else if !self.closed { self.close_steps += 1; self.closed = self.close_steps >= 3; }
            return;
        }
        if self.starting() {
            match self.poll_start() { Ok(true) => {}, Ok(false) => return, Err(error) => { self.failed(error); return; } }
        }
        self.flush(); if self.hold { return; }
        self.ready_wait = if self.has_frame && self.frame_serial == self.settings.seek_serial {
            self.ready_wait.saturating_sub(1)
        } else { 4 };
        if self.has_frame && self.fading_in { self.alpha = (self.alpha + 0.05 * frames).min(1.0); }
        for _ in 0..4 {
            match self.session.poll() {
                Ok(Some(Event::Frame(frame))) => {
                    if frame.seek_serial != self.settings.seek_serial { continue; }
                    if let Err(error) = self.cache_frame(r, frame) { self.failed(error); } break;
                }
                Ok(Some(Event::Eof)) => { self.finished = true; self.audio = None; break; }
                Ok(Some(Event::Failed(error))) | Err(error) => { self.failed(error); break; }
                Ok(Some(Event::Started)) => {}, Ok(None) => break,
            }
        }
    }
    pub fn seek(&mut self, secs: f32) -> Result<()> {
        ensure!(secs.is_finite() && secs >= 0.0 && secs <= u64::MAX as f32 / 1000.0, "invalid movie seek");
        ensure!(!self.finished && !self.quarantined, "movie is no longer playing");
        self.settings.seek_serial = self.settings.seek_serial.wrapping_add(1).max(1);
        self.settings.seek_ms = (secs * 1000.0) as u64; self.session.note_seek(self.settings.seek_serial);
        // A following set_audio is flushed in this same bounded command batch.
        self.position_ms = self.settings.seek_ms; self.ready_wait = 4; Ok(())
    }
    pub fn stop(&mut self) {
        self.session.cancel(); self.audio = None; self.starting = None; self.finished = true; self.fading_in = false;
    }
}
impl Drop for Movie {
    fn drop(&mut self) {
        self.session.cancel();
        if self.quarantined {
            // A single quarantined service rejects subsequent opens. Preserve
            // descriptors/cache if GXM cannot prove their last completion.
            if let Some(source) = self.source.take() { std::mem::forget(source); }
            if let Some(cache) = self.cache.take() { std::mem::forget(cache); }
            std::mem::forget(self.tex.clone());
        } else { knife_perf::gauge_sub(Gauge::MovieFrameCacheBytes, self.cache_bytes); }
    }
}
impl owner::Audio for MovieAudioSink {
    fn ready(&mut self) -> Result<bool, String> { MovieAudioSink::ready(self).map_err(str::to_string) }
    fn submit(&mut self, frames: &[[f32; 2]]) -> Result<(), String> { MovieAudioSink::submit(self, frames).map_err(str::to_string) }
}
struct NativeDecoder { player: MoviePlayer, geometry: VideoGeometry }
impl NativeDecoder {
    fn open(path: &str) -> Result<Self, String> {
        (|| -> Result<Self> {
            let geometry = Scope::new(Metric::MovieGeometry);
            let source = movie_geometry::read_file(path).context("read bounded movie SPS geometry")?;
            geometry.finish_hot(path, 0, 1);
            let view = movie_view::resolve(source, [source.coded_width, source.coded_height])?;
            ensure!(view.visible_width == 960 && view.visible_height == 540
                && (view.display_aspect - 16.0 / 9.0).abs() < 0.02, "movie requires 960x540 visible 16:9");
            let open = Scope::new(Metric::MovieOpen); let player = MoviePlayer::open_file(path).context("open native movie")?;
            open.finish_hot(path, 0, 1); Ok(Self { player, geometry: source })
        })().map_err(|error| format!("{error:#}"))
    }
}
impl owner::Decoder for NativeDecoder {
    fn poll_start(&mut self) -> Result<bool, String> {
        if !self.player.source_ready().map_err(|e| e.to_string())? { return Ok(false); }
        let select = Scope::new(Metric::MovieOwnerControl);
        self.player.select_streams().map_err(|e| e.to_string())?;
        select.finish_hot("AvPlayerSelectStreams", 0, 1);
        let start = Scope::new(Metric::MovieOwnerControl);
        self.player.start_selected().map_err(|e| e.to_string())?;
        start.finish_hot("AvPlayerStart", 0, 1);
        Ok(true)
    }
    fn hold(&mut self, hold: bool) -> Result<(), String> { (if hold { self.player.pause() } else { self.player.resume() }).map_err(|e| e.to_string()) }
    fn seek(&mut self, ms: u64) -> Result<(), String> { self.player.seek_ms(ms).map_err(|e| e.to_string()) }
    fn video(&mut self) -> Result<Option<owner::Frame>, String> {
        let Some(frame) = self.player.next_video().map_err(|e| e.to_string())? else { return Ok(None); };
        if frame.format != VideoFormat::GxmYvu420P2Csc1 { return Err("unsupported native movie pixel format".into()); }
        let view = movie_view::resolve(self.geometry, [frame.width, frame.height]).map_err(|e| e.to_string())?;
        Ok(Some(owner::Frame { token: 0, seek_serial: 0, pointer: frame.native_data() as usize, width: frame.width,
            height: frame.height, timestamp_ms: frame.timestamp_ms, uv: view.uv }))
    }
    fn audio(&mut self, out: &mut [[f32; 2]; 960]) -> Result<usize, String> { self.player.read_audio(out).map(|r| r.frames).map_err(|e| e.to_string()) }
    fn eof(&mut self) -> Result<bool, String> { self.player.poll_state().map(|s| s == MovieState::Eof).map_err(|e| e.to_string()) }
    fn close(&mut self) -> Result<(), String> { self.player.close().map_err(|e| e.to_string()) }
}

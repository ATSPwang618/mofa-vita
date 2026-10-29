//! Vita AvPlayer video: native YVU420P2_CSC1 surfaces are only sampled through GXM.
use std::path::{Path, PathBuf};
use anyhow::{Context, Result, ensure};
use knife_platform_vita::movie::{MoviePlayer, MovieState, VideoFormat};
use knife_platform_vita::{movie_geometry::{self, VideoGeometry}, movie_view};
use knife_render::{MovieGpuFence, Renderer, TexRef};
use knife_audio::MovieAudioSink;

pub struct Movie {
    player: MoviePlayer,
    fence: MovieGpuFence,
    pub tex: TexRef,
    pub finished: bool,
    pub looping: bool,
    pub hold: bool,
    pub alpha: f32,
    pub closed: bool,
    pub loop_start: f32,
    has_frame: bool,
    paused: bool,
    fading_in: bool,
    close_steps: u8,
    ready_wait: u8,
    position_ms: u64,
    path: PathBuf,
    audio: Option<MovieAudioSink>,
    source_geometry: VideoGeometry,
    uv: [f32; 4],
}
impl Movie {
    pub fn open(r: &Renderer, path: &Path, looping: bool) -> Result<Self> {
        let name = path.to_str().context("movie path is not UTF-8")?;
        let source_geometry = movie_geometry::read_file(name).context("read bounded movie SPS geometry")?;
        let source_view = movie_view::resolve(source_geometry,
            [source_geometry.coded_width, source_geometry.coded_height])?;
        ensure!(source_view.visible_width == 960 && source_view.visible_height == 540
            && (source_view.display_aspect - 16.0 / 9.0).abs() < 0.02,
            "movie source must have a 960x540 visible 16:9 region");
        let mut player = MoviePlayer::open_file(name).with_context(|| format!("open movie {}", path.display()))?;
        player.start().context("start native movie")?;
        Ok(Self { player, fence: r.movie_gpu_fence(), tex: r.create_writable(2, 2, false, "movie-before-first-frame"),
            finished: false, looping, hold: false, alpha: 0.0, closed: false, loop_start: 0.0,
            has_frame: false, paused: false, fading_in: true, close_steps: 0, ready_wait: 4,
            position_ms: 0, path: path.to_path_buf(), audio: None, source_geometry, uv: source_view.uv })
    }
    pub fn set_audio(&mut self, audio: MovieAudioSink) { self.audio = Some(audio); }
    pub fn position(&self) -> f32 { self.position_ms as f32 / 1000.0 }
    pub fn ready(&self) -> bool { self.ready_wait == 0 && !self.finished }
    pub fn has_frame(&self) -> bool { self.has_frame }
    pub fn uv_rect(&self) -> [f32; 4] { self.uv }
    fn failed(&mut self, error: impl std::fmt::Display) {
        tracing::error!(path=%self.path.display(), "AvPlayer movie failure: {error}");
        self.finished = true;
        self.audio = None;
        self.has_frame = false; // the decoder may revoke the borrowed frame
    }
    pub fn update(&mut self, r: &Renderer, dt: f32) {
        let frames = dt * 0.06;
        if self.finished {
            if self.alpha > 0.0 { self.alpha = (self.alpha - frames / 30.0).max(0.0); }
            else if !self.closed { self.close_steps += 1; self.closed = self.close_steps >= 3; }
            return;
        }
        if self.hold != self.paused {
            if let Err(error) = self.fence.finish() { self.failed(error); return; }
            // After a control call no old decoder pointer can be sampled again.
            self.has_frame = false;
            let control = if self.hold { self.player.pause() } else { self.player.resume() };
            if let Err(error) = control { self.failed(error); return; }
            self.paused = self.hold;
        }
        if self.hold { return; }
        if let Err(error) = self.fence.finish() { self.failed(error); return; }
        if let Some(sink) = &mut self.audio {
            for _ in 0..4 { // 4 * 960 frames/update covers 48 kHz at 30 fps with bounded headroom
                match sink.ready() {
                    Ok(true) => {
                        let mut chunk = [[0.0f32; 2]; 960];
                        match self.player.read_audio(&mut chunk) {
                            Ok(read) if read.frames > 0 => {
                                if let Err(error) = sink.submit(&chunk[..read.frames]) { self.failed(error); return; }
                            }
                            Ok(_) => break,
                            Err(error) => { self.failed(error); return; }
                        }
                    }
                    Ok(false) => break,
                    Err(error) => { self.failed(error); return; }
                }
            }
        }
        self.ready_wait = if self.has_frame { self.ready_wait.saturating_sub(1) } else { 4 };
        if self.has_frame && self.fading_in { self.alpha = (self.alpha + 0.05 * frames).min(1.0); }
        match self.player.next_video() {
            Ok(Some(frame)) => {
                if frame.format != VideoFormat::GxmYvu420P2Csc1 { self.failed("unsupported native movie pixel format"); return; }
                let view = match movie_view::resolve(self.source_geometry, [frame.width, frame.height]) {
                    Ok(view) => view,
                    Err(error) => { self.failed(error); return; }
                };
                let (width, height, ptr, stamp) = (frame.width, frame.height, frame.native_data(), frame.timestamp_ms);
                // SAFETY: AvPlayer owns the mapped YVU frame; the fence above completed previous reads.
                // The next decoder/control call must also be preceded by a fence after the draw.
                let texture = unsafe { r.try_movie_texture(ptr, width, height) };
                match texture {
                    Ok(tex) => {
                        self.tex = tex;
                        self.uv = view.uv;
                        self.position_ms = stamp;
                        if !self.has_frame { tracing::info!(path=%self.path.display(),width,height,"native movie frame bound"); }
                        self.has_frame = true;
                    }
                    Err(error) => self.failed(error),
                }
            }
            Ok(None) => {
                match self.player.poll_state() {
                    Ok(MovieState::Eof) if self.looping => {
                        if let Err(error) = self.seek(self.loop_start) { self.failed(error); }
                    }
                    Ok(MovieState::Eof) => { self.finished = true; self.audio = None; self.has_frame = false; }
                    Ok(_) => {},
                    Err(error) => self.failed(error),
                }
            }
            Err(error) => self.failed(error),
        }
    }
    pub fn seek(&mut self, secs: f32) -> Result<()> {
        ensure!(secs.is_finite() && secs >= 0.0 && secs <= u64::MAX as f32 / 1000.0, "invalid movie seek");
        self.fence.finish()?;
        self.player.seek_ms((secs * 1000.0) as u64).context("native movie seek unsupported or failed")?;
        self.position_ms = (secs * 1000.0) as u64;
        self.has_frame = false;
        self.ready_wait = 4;
        Ok(())
    }
    pub fn stop(&mut self) {
        if self.finished { return; }
        if let Err(error) = self.fence.finish().and_then(|_| self.player.stop().map_err(Into::into)) {
            self.failed(error);
        }
        self.has_frame = false;
        self.audio = None;
        self.finished = true;
        self.fading_in = false;
    }
}
impl Drop for Movie {
    fn drop(&mut self) {
        if let Err(error) = self.fence.finish() { tracing::error!("movie GPU completion before AvPlayer close: {error:#}"); }
        if let Err(error) = self.player.close() { tracing::error!("AvPlayer close failed: {error}"); }
    }
}

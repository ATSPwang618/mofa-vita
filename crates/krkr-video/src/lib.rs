//! Platform-independent video playback, bounded buffers and audio clock.
mod service;
pub use krkr_protocol::pixels::VideoPixels;
use krkr_protocol::{budget::Budget, graphics::Size};
pub use service::{Handle, Service};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};

pub type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Debug)]
pub struct Info {
    pub size: Size,
    pub fps: f64,
    pub frames: u64,
    pub duration: f64,
    pub audio_streams: usize,
    pub video_streams: usize,
    pub video_stream: usize,
    pub audio_rate: u32,
}
pub struct Frame {
    pub time: f64,
    pub pixels: VideoPixels,
}
pub enum Decoded {
    /// Requested streams are drained; other streams still have buffered work.
    Pending,
    /// A paused hardware clock cannot produce more requested samples. Wait for
    /// playback control, a seek, or queue consumption instead of polling it.
    Suspended,
    AudioEnd,
    Video(Frame),
    Audio {
        position: u64,
        samples: Vec<[f32; 2]>,
    },
}
/// Constructed and destroyed on the media worker; no native pointers or
/// codec-specific types reach the engine or the renderer.
pub trait Decoder {
    fn info(&self) -> &Info;
    fn next(&mut self, video: bool, audio: bool) -> Result<Option<Decoded>>;
    fn seek(&mut self, seconds: f64) -> Result<()>;
    fn audio_stream(&mut self, index: Option<usize>) -> Result<()>;
    fn video_stream(&mut self, index: usize) -> Result<()>;
    /// Hardware players have their own clock. Software decoders need no action.
    fn playback(&mut self, _playing: bool, _rate: f64) -> Result<()> {
        Ok(())
    }
}
pub trait Backend: Send + Sync {
    /// Called on the decoder worker. Signals must remain pending until waited on.
    fn worker_wake(&self) -> Result<Arc<dyn WorkerWake>> {
        Ok(Arc::new(ThreadWake(std::thread::current())))
    }
    /// Optional lifecycle diagnostics. Called only at open/control boundaries,
    /// never for each decoded frame; the backend chooses the output sink.
    fn trace(&self, _event: &str) {}
    fn open_silent(
        &self,
        plan: krkr_assets::ReadPlan,
        budget: Budget,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn Decoder>> {
        let mut decoder = self.open(plan, budget, cancel)?;
        decoder.audio_stream(None)?;
        Ok(decoder)
    }
    fn open(
        &self,
        plan: krkr_assets::ReadPlan,
        budget: Budget,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn Decoder>>;
}

/// Platform wait primitive for decoder polling and playback controls.
pub trait WorkerWake: Send + Sync {
    fn signal(&self);
    fn wait(&self, timeout: Option<Duration>) -> Result<()>;
}
struct ThreadWake(std::thread::Thread);
impl WorkerWake for ThreadWake {
    fn signal(&self) {
        self.0.unpark();
    }
    fn wait(&self, timeout: Option<Duration>) -> Result<()> {
        match timeout {
            Some(timeout) => std::thread::park_timeout(timeout),
            None => std::thread::park(),
        }
        Ok(())
    }
}

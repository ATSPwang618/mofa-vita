//! Run the audio clock without a device. Optional AT9 substitution retains duration.
use krkr_audio::{DecodedFrame, DecoderBackend, Format, Result, StreamDecoder};
use krkr_protocol::budget::Budget;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct Output {
    stopped: Arc<AtomicBool>,
    workers: Arc<Mutex<Vec<std::thread::JoinHandle<()>>>>,
}
impl Output {
    pub fn new(stopped: Arc<AtomicBool>) -> Self {
        Self {
            stopped,
            workers: Default::default(),
        }
    }
    pub fn join(&self) -> Result<()> {
        let workers = std::mem::take(&mut *self.workers.lock().unwrap());
        for thread in workers {
            thread.join().map_err(|_| "audio clock panicked")?;
        }
        Ok(())
    }
}
impl krkr_audio::OutputHost for Output {
    fn start(&self, mut mixer: krkr_audio::Mixer) -> Result<()> {
        let stopped = self.stopped.clone();
        let worker = std::thread::Builder::new()
            .name("profile-audio".into())
            .spawn(move || {
                let mut samples = [0.; 960];
                let interval = std::time::Duration::from_millis(10);
                let mut next = std::time::Instant::now();
                while !stopped.load(Ordering::Acquire) {
                    {
                        let _span = krkr_protocol::profile::span("audio.mix");
                        mixer.render(&mut samples, 2, 48000, std::time::Duration::ZERO);
                    }
                    next += interval;
                    std::thread::sleep(next.saturating_duration_since(std::time::Instant::now()));
                }
            })
            .map_err(|e| e.to_string())?;
        self.workers.lock().unwrap().push(worker);
        Ok(())
    }
}
pub struct At9Clock;
impl DecoderBackend for At9Clock {
    fn open(
        &self,
        plan: &krkr_assets::ReadPlan,
        _: Budget,
        _: bool,
        _: Arc<AtomicBool>,
    ) -> Result<Box<dyn StreamDecoder>> {
        let mut stream = plan.open().map_err(|e| e.to_string())?;
        let header = krkr_audio::at9::inspect(&mut stream, plan.bytes)?
            .ok_or("AT9 clock substitution requires an AT9 source")?;
        Ok(Box::new(Silent {
            format: header.format,
            position: 0,
        }))
    }
}
struct Silent {
    format: Format,
    position: u64,
}
impl StreamDecoder for Silent {
    fn format(&self) -> Format {
        self.format
    }
    fn seek(&mut self, frame: u64) -> Result<()> {
        self.position = frame.min(self.format.frames);
        Ok(())
    }
    fn next(&mut self) -> Result<Option<DecodedFrame>> {
        if self.position >= self.format.frames {
            return Ok(None);
        }
        let frame = DecodedFrame {
            position: self.position,
            ..Default::default()
        };
        self.position += 1;
        Ok(Some(frame))
    }
}

//! Bounded external PCM producer, used by the video demux worker. The same
//! mixer, device timestamps and voice lifetime apply to sound and movie audio.
use super::*;

#[cfg(test)]
#[path = "../tests/internal/native_mixing.rs"]
mod native_mixing;

pub struct Pcm {
    handle: Handle,
}
impl Service {
    /// Call on a media worker: starting the device can block.
    pub fn pcm(&self, rate: u32) -> Result<Pcm> {
        if !(8000..=192000).contains(&rate) {
            return Err("unsupported PCM sample rate".into());
        }
        let (handle, _) = self.voice(
            Format {
                rate,
                channels: 2,
                bits: 32,
                frames: 0,
            },
            Arc::new(std::array::from_fn(|_| AtomicI32::new(0))),
            Arc::default(),
            Vec::new(),
        )?;
        Ok(Pcm { handle })
    }
}
impl Pcm {
    pub fn position(&self) -> Position {
        self.handle.position()
    }
    pub fn finished(&self) -> bool {
        self.handle.finished()
    }
    pub fn gain(&self, gain: i32, pan: i32) {
        self.handle.gain(gain, pan);
    }
    pub fn rate(&self, speed: f64) {
        self.handle
            .frequency((self.handle.format().rate as f64 * speed) as i32);
    }
    pub fn play(&self, playing: bool) {
        self.handle
            .voice()
            .playing
            .store(playing, Ordering::Release);
    }
    pub fn space(&self) -> usize {
        FRAMES - self.handle.voice().queue.lock().unwrap().frames.len()
    }
    /// Accept as much as fits. Positions use samples on the absolute media
    /// timeline, so A/V offsets and seeks share the device's audible clock.
    pub fn push(&self, position: u64, samples: &[[f32; 2]]) -> usize {
        let v = self.handle.voice();
        // Conversion must not hold the queue lock: the realtime mixer uses
        // try_lock and emits silence if a producer owns it. A bounded stack
        // batch keeps conversion and float rounding outside the critical path.
        let mut batch = [Frame {
            sample: [0.; 2],
            pcm: [0; 8],
            position: 0,
            labels: [0, 0],
        }; 256];
        let mut accepted = 0;
        for samples in samples.chunks(batch.len()) {
            let count = samples.len();
            for (out, frame) in
                batch.iter_mut().zip(
                    samples[..count]
                        .iter()
                        .enumerate()
                        .map(|(i, sample)| Frame {
                            sample: *sample,
                            pcm: std::array::from_fn(|c| {
                                sample.get(c).map_or(0, |s| {
                                    (*s * 32768.).round().clamp(-32768., 32767.) as i16
                                })
                            }),
                            position: position + accepted as u64 + i as u64,
                            labels: [0, 0],
                        }),
                )
            {
                *out = frame;
            }
            let mut q = v.queue.lock().unwrap();
            let stored = count.min(FRAMES - q.frames.len());
            q.frames.extend(batch[..stored].iter().copied());
            accepted += stored;
            drop(q);
            if stored < count {
                break;
            }
        }
        v.decoded
            .store(position + accepted as u64, Ordering::Relaxed);
        accepted
    }
    pub fn eof(&self) {
        self.handle.voice().queue.lock().unwrap().eof = true;
    }
    /// Caller pauses first and owns the producer. The device callback uses the
    /// same queue lock, so no pre-seek PCM/stamps can re-enter the new epoch.
    pub fn flush(&self, position: u64) {
        let v = self.handle.voice();
        v.playing.store(false, Ordering::Release);
        let mut q = v.queue.lock().unwrap();
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
        v.decoded.store(position, Ordering::Relaxed);
        v.submitted.store(position, Ordering::Relaxed);
        v.ended.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Output(Arc<Mutex<Option<Mixer>>>);
    impl OutputHost for Output {
        fn start(&self, mixer: Mixer) -> Result<()> {
            *self.0.lock().unwrap() = Some(mixer);
            Ok(())
        }
    }
    #[test]
    fn source_pcm_device_delay_ahead_pause_flush_and_budget() {
        let budget = Budget::new(8 * 1024 * 1024);
        let service = Service::new(budget.clone());
        let output = Arc::new(Mutex::new(None));
        service.set_output(Output(output.clone())).unwrap();
        let pcm = service.pcm(48000).unwrap();
        let h = &pcm.handle;
        let baseline = budget.used();
        h.visualization(true).unwrap();
        assert!(budget.used() > baseline);
        let samples: Vec<_> = (0..512)
            .map(|i| if i < 256 { [0.5, -0.25] } else { [-1., 0.5] })
            .collect();
        pcm.push(0, &samples);
        pcm.gain(0, 100000);
        pcm.play(true);
        let mut rendered = [1.; 256];
        output.lock().unwrap().as_mut().unwrap().render(
            &mut rendered,
            2,
            48000,
            Duration::from_secs(1),
        );
        assert!(rendered.iter().all(|&v| v == 0.));
        let mut mono = [0; 8];
        assert_eq!(h.read_visualization(&mut mono, 1, 0), 8);
        assert_eq!(mono, [4096; 8]);
        assert_eq!(h.read_visualization(&mut mono, 1, 256), 8);
        assert_eq!(mono, [-8192; 8]);
        let mut stereo = [0; 4];
        assert_eq!(h.read_visualization(&mut stereo, 2, 0), 2);
        assert_eq!(stereo, [16384, -8192, 16384, -8192]);
        h.pause(true);
        assert_eq!(h.read_visualization(&mut mono, 1, 0), 0);
        h.pause(false);
        pcm.flush(9000);
        pcm.push(9000, &[[0.25, 0.25]; 32]);
        pcm.play(true);
        assert_eq!(h.read_visualization(&mut mono, 1, 0), 8);
        assert_eq!(mono, [8192; 8]);
        h.visualization(false).unwrap();
        assert_eq!(h.read_visualization(&mut mono, 1, 0), 0);
        assert_eq!(budget.used(), baseline);
        drop(pcm);
        assert_eq!(budget.used(), 0);
    }
}

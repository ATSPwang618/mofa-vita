//! Bulk queue consumption when no labels or visualization need per-frame
//! delivery. Retain the interpolation boundary and audible timestamps.
use super::*;

pub(super) struct OutputFormat {
    pub channels: usize,
    pub rate: u32,
}

impl Queue {
    pub(super) fn mix_resampled(
        &mut self,
        voice: &Voice,
        output: &mut [f32],
        format: OutputFormat,
        gains: [f32; 2],
        start: Duration,
        ratio: f64,
    ) -> (Option<u64>, usize) {
        let OutputFormat { channels, rate } = format;
        // Walk the two ring slices once; avoid repeated front/get/pop and
        // copying visualization PCM on every interpolated output sample.
        let mut frames = self.frames.iter();
        let mut a = frames.next();
        let mut b = frames.next();
        let mut consumed = 0;
        let mut phase = self.phase;
        let mut last = None;
        let mut last_index = 0;
        for (index, out) in output.chunks_exact_mut(channels).enumerate() {
            // Keep the generic mixer's exact subtraction/addition order.
            // floor/modulo would change rounding at fractional boundaries,
            // including phase debt carried through an input underrun.
            while phase >= 1.0 && a.is_some() {
                a = b;
                b = frames.next();
                consumed += 1;
                phase -= 1.0;
                self.labels_sent = false;
            }
            let Some(frame) = a else {
                if self.eof {
                    voice.ended.store(true, Ordering::Release);
                    voice.playing.store(false, Ordering::Release);
                    self.end_at = Some(start + Duration::from_secs_f64(index as f64 / rate as f64));
                } else {
                    voice.underruns.fetch_add(1, Ordering::Relaxed);
                }
                break;
            };
            self.labels_sent = true;
            let next = b.unwrap_or(frame);
            let t = phase as f32;
            let left = (frame.sample[0] + (next.sample[0] - frame.sample[0]) * t) * gains[0];
            let right = (frame.sample[1] + (next.sample[1] - frame.sample[1]) * t) * gains[1];
            if channels == 1 {
                out[0] += (left + right) * 0.5;
            } else {
                out[0] += left;
                out[1] += right;
            }
            if index % 64 == 0 || last.is_some_and(|p| frame.position < p) {
                if self.stamps.len() == STAMPS {
                    self.stamps.pop_front();
                }
                self.stamps.push_back(Stamp {
                    at: start + Duration::from_secs_f64(index as f64 / rate as f64),
                    position: frame.position,
                });
            }
            last = Some(frame.position);
            last_index = index;
            phase += ratio;
        }
        self.frames.drain(..consumed);
        self.phase = phase;
        (last, last_index)
    }

    pub(super) fn mix_native(
        &mut self,
        voice: &Voice,
        output: &mut [f32],
        channels: usize,
        gains: [f32; 2],
        start: Duration,
        rate: u32,
    ) -> (Option<u64>, usize) {
        let requested = output.len() / channels;
        if requested == 0 {
            return (None, 0);
        }
        // The generic mixer consumes the previously submitted sample at the
        // beginning of the next output frame. Preserve that boundary, including
        // empty callbacks, underflows, and a later fractional-rate callback.
        if self.phase == 1.0 && self.frames.pop_front().is_some() {
            self.phase = 0.0;
            self.labels_sent = false;
        }
        let count = requested.min(self.frames.len());
        let mut last = None;
        for (index, (frame, out)) in self
            .frames
            .iter()
            .zip(output.chunks_exact_mut(channels))
            .enumerate()
        {
            let left = frame.sample[0] * gains[0];
            let right = frame.sample[1] * gains[1];
            if channels == 1 {
                out[0] += (left + right) * 0.5;
            } else {
                out[0] += left;
                out[1] += right;
            }
            if index % 64 == 0 || last.is_some_and(|p| frame.position < p) {
                if self.stamps.len() == STAMPS {
                    self.stamps.pop_front();
                }
                self.stamps.push_back(Stamp {
                    at: start + Duration::from_secs_f64(index as f64 / rate as f64),
                    position: frame.position,
                });
            }
            last = Some(frame.position);
        }
        if count != 0 {
            let retained = usize::from(count == requested);
            self.frames.drain(..count - retained);
            self.phase = retained as f64;
            self.labels_sent = retained != 0;
        }
        if count < requested {
            if self.eof {
                voice.ended.store(true, Ordering::Release);
                voice.playing.store(false, Ordering::Release);
                self.end_at = Some(start + Duration::from_secs_f64(count as f64 / rate as f64));
            } else {
                voice.underruns.fetch_add(1, Ordering::Relaxed);
            }
        }
        (last, count.saturating_sub(1))
    }
}

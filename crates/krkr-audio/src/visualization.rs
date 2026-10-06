//! Source PCM history follows the device's audible clock, before volume/pan.
//! Allocated only on request; the callback only pushes into reserved storage.
use super::*;
const HISTORY: usize = 32768;
struct Sample {
    pcm: [i16; 8],
    at: Duration,
}

pub(super) struct Buffer {
    history: VecDeque<Sample>,
    _permit: Permit,
}
impl Buffer {
    pub fn clear(&mut self) {
        self.history.clear();
    }
    pub fn push(&mut self, pcm: [i16; 8], at: Duration) {
        if self.history.len() == HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(Sample { pcm, at });
    }
}
impl Handle {
    pub fn visualization(&self, enabled: bool) -> Result<()> {
        let v = self.voice();
        let buffer = if enabled {
            Some(Buffer {
                _permit: v
                    .budget
                    .reserve(HISTORY * std::mem::size_of::<Sample>())
                    .map_err(|e| e.to_string())?,
                history: VecDeque::with_capacity(HISTORY),
            })
        } else {
            None
        };
        v.queue.lock().unwrap().visual = buffer;
        Ok(())
    }
    /// Returns frames, not scalar sample count. Unsupported channel selections
    /// and unavailable/stopped audio return zero and leave the destination alone.
    pub fn read_visualization(&self, dest: &mut [i16], channels: usize, ahead: i32) -> usize {
        let v = self.voice();
        if channels == 0
            || (channels != 1 && channels != v.format.channels as usize)
            || !v.playing.load(Ordering::Acquire)
            || v.paused.load(Ordering::Relaxed)
        {
            return 0;
        }
        let q = v.queue.lock().unwrap();
        let Some(visual) = &q.visual else {
            return 0;
        };
        let now = v.origin.elapsed();
        let base = visual.history.partition_point(|s| s.at < now);
        let start = base as i64 + i64::from(ahead);
        if start < 0 {
            return 0;
        }
        let start = start as usize;
        let total = visual.history.len() + q.frames.len();
        if start >= total {
            return 0;
        }
        let count = (dest.len() / channels).min(total - start);
        for (i, out) in dest.chunks_exact_mut(channels).take(count).enumerate() {
            let n = start + i;
            let pcm = if n < visual.history.len() {
                &visual.history[n].pcm
            } else {
                &q.frames[n - visual.history.len()].pcm
            };
            if channels == 1 {
                let source = &pcm[..v.format.channels as usize];
                out[0] = (source.iter().map(|&s| i32::from(s)).sum::<i32>() / source.len() as i32)
                    as i16;
            } else {
                out.copy_from_slice(&pcm[..channels]);
            }
        }
        count
    }
}

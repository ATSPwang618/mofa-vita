//! AvPlayer timestamps have millisecond resolution; PCM has sample resolution.
#[derive(Default)]
pub struct AudioClock {
    next: Option<u64>,
}
impl AudioClock {
    pub fn reset(&mut self) {
        self.next = None;
    }
    pub fn packet(&mut self, milliseconds: u64, rate: u32, samples: usize) -> u64 {
        let rate = u64::from(rate);
        let reported = milliseconds.saturating_mul(rate) / 1000;
        let position = self
            .next
            .filter(|&next| next.abs_diff(reported) <= rate.div_ceil(1000))
            .unwrap_or(reported);
        self.next = Some(position.saturating_add(samples as u64));
        position
    }
}

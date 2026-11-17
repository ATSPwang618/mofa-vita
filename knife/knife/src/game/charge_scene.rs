//! Timing probes for the original scene 38 stage. They never own assets or
//! alter script dispatch, presentation, or sound.
pub(super) const SCENE: usize = 38;
pub(super) const FADE: usize = 1192;
pub(super) const COMMAND: usize = 1245;
pub(super) const DURATION: i32 = 4000;

pub(super) fn tick(scene: usize, pc: usize, left: f32, click: bool) -> Option<usize> {
    if scene != SCENE || pc != COMMAND + 1 || click || !(0.0..=DURATION as f32).contains(&left) {
        return None;
    }
    Some((((DURATION as f32 - left) * 0.06).round() as usize).min(239))
}

#[cfg(target_os = "vita")]
#[derive(Default)]
pub(super) struct Trace {
    last_phase: Option<usize>,
    pending: Option<(usize, std::time::Instant)>,
    presented: std::cell::Cell<Option<usize>>,
}

#[cfg(target_os = "vita")]
impl Trace {
    pub(super) fn begin_phase(&mut self, pc: usize) -> bool {
        if self.last_phase == Some(pc) { return false; }
        self.last_phase = Some(pc);
        self.pending = Some((pc, std::time::Instant::now()));
        true
    }

    pub(super) fn presented_phase(&mut self) -> Option<(usize, u128)> {
        let (pc, started) = self.pending.take()?;
        self.presented.set(Some(pc));
        Some((pc, started.elapsed().as_micros()))
    }

    pub(super) fn take_presented_pc(&self) -> Option<usize> { self.presented.take() }
}

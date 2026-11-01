//! PCM preparation changes no mixer or VM state. A command stays at its exact
//! PC; a due delayed batch keeps its unconsumed timer decrement until ready.
#[cfg(target_os = "vita")]
use super::*;
#[cfg(target_os = "vita")]
use crate::assets::SoundNeeded;
use std::time::{Duration, Instant};

pub(super) enum Kind { Command(f32), Delayed(f32) }
pub(super) struct Barrier { scene: usize, pc: usize, names: Vec<String>, kind: Kind, started: Instant }

fn unique_names(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if !name.is_empty() && !out.iter().any(|other| other.eq_ignore_ascii_case(&name)) { out.push(name); }
    }
    out
}
fn due(left: f32, fresh: bool, ms: f32) -> bool {
    left - if fresh { (ms - 1000.0 / 60.0).max(0.0) } else { ms } <= 0.01
}
fn direct_play_needs_pcm(looping: bool, skip: bool, same_loop: bool, word: i16) -> bool {
    // Mirrors cmd_se/SEPL's two early returns. Queued SEPL deliberately has
    // no same-loop guard: run_se_queued restarts that sound even if identical.
    !(!looping && skip || looping && same_loop && word > 0)
}
fn queued_play_needs_pcm(looping: bool, skip: bool) -> bool { looping || !skip }
#[cfg(target_os = "vita")]
impl Game {
    fn direct_sound_name<'a>(&self, command: &'a Command) -> Option<&'a str> {
        let (name, looping) = match command.name.as_str() {
            "SEPL" => (command.arg(2)?, command.int_or(1, 0) == 1),
            // Immediate SEAC forwards this loop value unchanged to SEPL.
            "SEAC" => (command.arg(1)?, command.int_or(2, 0) == 1),
            _ => return None,
        };
        let channel = command.int_or(0, 0) % 20;
        let same_loop = self.sound.looping_on(channel).is_some_and(|current| current.eq_ignore_ascii_case(name));
        let word = usize::try_from(channel).ok().and_then(|channel| self.sound.se_word.get(channel)).copied().unwrap_or(0);
        direct_play_needs_pcm(looping, self.skip, same_loop, word).then_some(name)
    }
    pub(super) fn prefetch_sound_effects(&mut self) {
        let mut names = Vec::with_capacity(8);
        if !self.paused && !self.seek_node {
            for (queued, _, _) in &self.se_delayed {
                if matches!(queued.kind, 1 | 4) && let Some(name) = &queued.name { names.push(name.clone()); }
            }
            let end = (self.pc + 24).min(self.script.commands.len());
            for command in self.script.commands.get(self.pc..end).unwrap_or_default() {
                let name = match command.name.as_str() { "SEPL" => command.arg(2), "SEAC" => command.arg(1), _ => None };
                if let Some(name) = name { names.push(name.into()); }
            }
        }
        self.assets.prefetch_sounds(&unique_names(names));
    }
    pub(super) fn prepare_sound_command(&mut self, command: &Command, ms: f32) -> bool {
        if self.seek_audio.is_some() || !matches!(command.name.as_str(), "SEPL" | "SEAC" | "SEFD" | "SEVL") { return true; }
        let mut names = Vec::new();
        if let Some(delay) = command.keyed_int(68) {
            // Replacing a queued slot executes its OLD op first. The newly
            // queued sound is only prefetched now and checked again when due.
            let channel = command.int_or(0, if matches!(command.name.as_str(), "SEPL" | "SEAC") { 0 } else { -1 });
            for (queued, _, _) in &self.se_delayed {
                if (channel < 0 || queued.channel == channel % 20) && matches!(queued.kind, 1 | 4)
                    && queued_play_needs_pcm(queued.looping, self.skip)
                    && let Some(name) = &queued.name { names.push(name.clone()); }
            }
            let frames = if (-32..=0).contains(&delay) { 1 } else { (3 * delay + 49) / 50 };
            if frames <= 1 {
                let name = match command.name.as_str() {
                    "SEPL" if queued_play_needs_pcm(command.int_or(1, 0) != 0, self.skip) => command.arg(2),
                    "SEAC" if queued_play_needs_pcm(command.int_or(2, 0) != 0, self.skip) => command.arg(1),
                    _ => None,
                };
                if let Some(name) = name { names.push(name.into()); }
            }
        } else {
            if let Some(name) = self.direct_sound_name(command) { names.push(name.into()); }
        }
        // This frame's existing due operations follow the command. Prepare
        // them together so READY can consume the saved frame tail once.
        for (queued, left, fresh) in &self.se_delayed {
            if due(*left, *fresh, ms) && matches!(queued.kind, 1 | 4)
                && queued_play_needs_pcm(queued.looping, self.skip)
                && let Some(name) = &queued.name { names.push(name.clone()); }
        }
        self.prepare_sound_names(unique_names(names), Kind::Command(ms))
    }
    pub(super) fn prepare_delayed_sounds(&mut self, ms: f32) -> bool {
        if self.seek_audio.is_some() { return true; }
        let names = self.se_delayed.iter().filter(|(_, left, fresh)| due(*left, *fresh, ms))
            .filter(|(queued, _, _)| matches!(queued.kind, 1 | 4) && queued_play_needs_pcm(queued.looping, self.skip))
            .filter_map(|(queued, _, _)| queued.name.clone());
        self.prepare_sound_names(unique_names(names), Kind::Delayed(ms))
    }
    fn prepare_sound_names(&mut self, names: Vec<String>, kind: Kind) -> bool {
        if !knife_perf::optimizations::enabled(knife_perf::optimizations::ASYNC_REBAKE) || names.is_empty() { return true; }
        // At most 20 queued channels plus one direct sound. Overflow is not a
        // valid command cohort and cannot become an unbounded staging pool.
        assert!(names.len() <= 24);
        for name in &names {
            if matches!(self.assets.poll_sound_needed(name), SoundNeeded::Pending) {
                let pc = if matches!(kind, Kind::Command(_)) { self.pc.saturating_sub(1) } else { self.pc };
                self.sound_barrier = Some(Barrier { scene: self.scene_index, pc, names, kind, started: Instant::now() });
                self.resource_pending = true;
                self.resource_event_at("pending", "sound PCM preparation; play/queued timers not consumed", self.scene_index, pc);
                return false;
            }
        }
        true
    }
    pub(super) fn poll_sound_barrier(&mut self, renderer: &Renderer) {
        let Some(barrier) = self.sound_barrier.take() else { return; };
        if barrier.scene != self.scene_index || barrier.pc != self.pc {
            self.assets.reset_sound_needed_context();
            self.resource_pending = true;return;
        }
        if barrier.started.elapsed() >= Duration::from_secs(30) {
            self.assets.fail_sound_cohort(&barrier.names, "30 second sound cohort deadline");
        } else {
            for name in &barrier.names {
                if matches!(self.assets.poll_sound_needed(name), SoundNeeded::Pending) {
                    self.sound_barrier = Some(barrier);return;
                }
            }
        }
        if self.paused && matches!(barrier.kind, Kind::Command(_)) { self.sound_barrier = Some(barrier);return; }
        match barrier.kind {
            Kind::Command(ms) => {
                // Retry only this original instruction, without advancing the
                // world a second time. Its original delayed-sound tail is also
                // consumed exactly once before normal dispatch resumes.
                if let Some(command) = self.script.commands.get(barrier.pc).cloned() {
                    self.pc = barrier.pc + 1;
                    self.resource_pending = false;
                    self.exec_profiled(renderer, &command);
                    self.tick_se_delayed(ms);
                    self.assets.release_prepared_sounds();
                }
            }
            Kind::Delayed(ms) => {
                // The earlier frame completed its script once, but did not
                // decrement these timers or play. Consume that saved tail once.
                let ready = self.tick_se_delayed(ms);
                debug_assert!(ready);
            }
        }
        self.resource_event_at("ready", "sound PCM ready; original play point preserved", barrier.scene, barrier.pc);
        self.resource_pending = true;
    }
    pub(super) fn story_sound_effect(&mut self, name: &str) -> Option<Arc<knife_audio::Pcm>> {
        if knife_perf::optimizations::enabled(knife_perf::optimizations::ASYNC_REBAKE) {
            // All story entry points were prepared before any channel changes.
            // A missing pin remains an explicit failed effect, never a hidden
            // synchronous codec fallback on the frame thread.
            match self.assets.cached_sound_effect(name) {
                Some(pcm) => pcm,
                None => {
                    tracing::warn!(name, "story sound preparation invariant missing; effect omitted");
                    knife_perf::add(knife_perf::Metric::SoundDemandFailed, 0, 1);None
                }
            }
        } else { self.assets.sound_effect(name) }
    }
}

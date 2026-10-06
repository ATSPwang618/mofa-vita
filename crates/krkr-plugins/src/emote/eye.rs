//! Blink controllers preserve the reference's strict time boundaries, saved
//! base value, and eye-before-timeline ordering. C++ default_random_engine is
//! implementation-defined; use minstd_rand0 consistently on every platform.
use super::metadata::Eye;
use std::collections::BTreeMap;

#[derive(Clone)]
pub(super) struct Blink {
    started: bool,
    blinking: bool,
    wait: i32,
    last: f32,
    base: f32,
}
impl Default for Blink {
    fn default() -> Self {
        Self {
            started: false,
            blinking: false,
            wait: -1,
            last: 0.,
            base: 0.,
        }
    }
}
struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0 * 16807 % 2147483647;
        self.0 - 1
    }
    fn interval(&mut self, min: i32, max: i32) -> i32 {
        if max < min {
            return min;
        }
        let width = (i64::from(max) - i64::from(min) + 1) as u64;
        let base = 2147483646u64;
        let range = if width <= base { base } else { base * base };
        let scale = range / width;
        loop {
            let n = if width <= base {
                self.next()
            } else {
                self.next() * base + self.next()
            };
            if n < width * scale {
                return (i64::from(min) + (n / scale) as i64) as i32;
            }
        }
    }
}
pub(super) fn update(
    definitions: &[Eye],
    states: &mut [Blink],
    variables: &mut BTreeMap<String, f32>,
    tick: f32,
) {
    // The source constructs its generator inside updateEyeControl, resetting
    // it each call; only controllers needing a new interval consume samples.
    let mut random = Random(1);
    for (eye, state) in definitions.iter().zip(states) {
        if !state.started {
            state.started = true;
            state.last = tick;
        }
        if state.wait < 0 {
            state.wait = random.interval(eye.interval_min, eye.interval_max);
        }
        let elapsed = tick - state.last;
        if state.blinking {
            if elapsed > eye.blink_frames as f32 {
                state.blinking = false;
                state.wait = -1;
                state.last = tick;
                if let Some(value) = variables.get_mut(&eye.label) {
                    *value = state.base;
                }
            } else {
                let frames = eye.blink_frames as f32;
                let delta = if elapsed * 2. < frames {
                    elapsed
                } else {
                    frames - elapsed
                };
                let value = (eye.begin + delta * 2. * (eye.end - eye.begin) / frames)
                    .max(eye.begin)
                    .min(eye.end);
                if let Some(current) = variables.get_mut(&eye.label) {
                    *current = value;
                }
            }
        } else if elapsed > state.wait as f32 {
            state.blinking = true;
            state.last = tick;
            if let Some(value) = variables.get(&eye.label) {
                state.base = *value;
            }
        }
    }
}

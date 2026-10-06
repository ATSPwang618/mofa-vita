//! Physical key state is independent of the script event queue. Script-posted
//! events do not manufacture a hardware key press.
use std::sync::atomic::{AtomicU8, Ordering};

pub(crate) struct Keys([AtomicU8; 512]);
impl Default for Keys {
    fn default() -> Self {
        Self(std::array::from_fn(|_| AtomicU8::new(0)))
    }
}
impl Keys {
    pub fn update(&self, key: u32, pressed: bool) {
        if let Some(state) = self.0.get(key as usize) {
            if pressed {
                // Auto-repeat is not a new physical down transition.
                let _ = state.try_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                    Some(if old & 1 == 0 { old | 3 } else { old })
                });
            } else {
                state.fetch_and(!1, Ordering::Relaxed);
            }
        }
    }
    pub fn held(&self, key: u32) -> bool {
        self.0
            .get(key as usize)
            .is_some_and(|s| s.load(Ordering::Relaxed) & 1 != 0)
    }
    pub fn release(&self) {
        for state in &self.0 {
            state.fetch_and(!1, Ordering::Relaxed);
        }
    }
    pub fn get(&self, key: u32, current: bool) -> bool {
        self.0.get(key as usize).is_some_and(|state| {
            // GetAsyncKeyState consumes the press latch even for current reads.
            let bits = state.fetch_and(!2, Ordering::Relaxed);
            bits & if current { 1 } else { 2 } != 0
        })
    }
}

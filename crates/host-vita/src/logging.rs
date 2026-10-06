//! Opt-in script diagnostics collected by the host's console logger.
use krkr_engine::debug::LogOutput;
use std::time::Instant;

pub(crate) struct Logs {
    enabled: bool,
    since: Instant,
}
impl Logs {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            since: Instant::now(),
        }
    }
}
impl LogOutput for Logs {
    fn enabled(&self) -> bool {
        self.enabled || krkr_protocol::profile::active()
    }
    fn timestamp(&mut self) -> String {
        format!("+{:.3}", self.since.elapsed().as_secs_f64())
    }
    fn console(&mut self, line: &[u16]) {
        krkr_protocol::profile::marker("script", || String::from_utf16_lossy(line));
        if self.enabled {
            eprintln!("[VITA][SCRIPT] {}", String::from_utf16_lossy(line));
        }
    }
}

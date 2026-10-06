//! Console levels and optional timings. Performance recordings are independent.
use std::{
    fmt,
    sync::atomic::{AtomicU8, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Level {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}
impl std::str::FromStr for Level {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err("log level must be off, error, warn, info, debug or trace"),
        }
    }
}
static LEVEL: AtomicU8 = AtomicU8::new(Level::Warn as u8);
pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}
pub fn set_enabled(enabled: bool) {
    set_level(if enabled { Level::Debug } else { Level::Warn });
}
#[inline]
pub fn allows(level: Level) -> bool {
    level != Level::Off && level as u8 <= LEVEL.load(Ordering::Relaxed)
}
#[inline]
pub fn enabled() -> bool {
    allows(Level::Debug)
}
pub fn write(level: Level, message: fmt::Arguments<'_>) {
    if allows(level) {
        eprintln!("[KRKR][{level:?}] {message}");
    }
}

#[derive(Clone, Copy)]
pub struct Timer {
    console: Option<Instant>,
    #[cfg(feature = "profiling")]
    capture: Option<crate::profile::Stamp>,
}
impl Timer {
    pub fn start() -> Self {
        Self {
            console: allows(Level::Trace).then(Instant::now),
            #[cfg(feature = "profiling")]
            capture: crate::profile::stamp(),
        }
    }
    pub fn report(self, detail: impl FnOnce() -> String) {
        let elapsed = self.console.map(|start| start.elapsed());
        let console = elapsed.is_some_and(|elapsed| elapsed >= Duration::from_millis(20))
            && allows(Level::Trace);
        #[cfg(feature = "profiling")]
        let capture = self.capture.is_some();
        #[cfg(not(feature = "profiling"))]
        let capture = false;
        if !console && !capture {
            return;
        }
        let detail = detail();
        #[cfg(feature = "profiling")]
        if let Some(stamp) = self.capture {
            let stage = detail
                .strip_prefix("stage=")
                .and_then(|s| s.split_whitespace().next())
                .unwrap_or("work");
            crate::profile::complete(stamp, stage, || detail.clone());
        }
        if console {
            write(
                Level::Trace,
                format_args!(
                    "thread={} elapsed_ms={:.3} {detail}",
                    std::thread::current().name().unwrap_or("unnamed"),
                    elapsed.unwrap().as_secs_f64() * 1000.
                ),
            );
        }
    }
}

#[macro_export]
macro_rules! log {
    ($level:ident, $($arg:tt)*) => {
        if $crate::diagnostics::allows($crate::diagnostics::Level::$level) {
            $crate::diagnostics::write($crate::diagnostics::Level::$level, ::std::format_args!($($arg)*));
        }
    };
}
#[macro_export]
macro_rules! diagnostic {
    ($($arg:tt)*) => { $crate::log!(Debug, $($arg)*); };
}

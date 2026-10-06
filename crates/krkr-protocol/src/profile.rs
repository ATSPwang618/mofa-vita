//! Optional recording for the performance tools. Disabled builds contain no recorder.
#[cfg(feature = "profiling")]
mod recording;
#[cfg(feature = "profiling")]
pub use recording::*;
#[cfg(feature = "profiling")]
mod file;
#[cfg(feature = "profiling")]
pub use file::FileCapture;

#[cfg(not(feature = "profiling"))]
mod disabled {
    pub struct Span;
    #[inline]
    pub fn active() -> bool {
        false
    }
    #[inline]
    pub fn span(_: &'static str) -> Span {
        Span
    }
    #[inline]
    pub fn span_detail(_: &'static str, _: impl FnOnce() -> String) -> Span {
        Span
    }
    #[inline]
    pub fn counter(_: &'static str, _: u64) {}
    #[inline]
    pub fn marker(_: &'static str, _: impl FnOnce() -> String) {}
}
#[cfg(not(feature = "profiling"))]
pub use disabled::*;

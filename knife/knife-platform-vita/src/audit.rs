//! Native CPU copy/synchronisation probes; all counters are cumulative u64.
#[path = "console_worker.rs"]
mod console_worker;
pub use console_worker::Stats as ConsoleStats;
use std::{cell::RefCell, sync::OnceLock};
thread_local! { static BATCH: RefCell<Option<Vec<String>>> = const { RefCell::new(None) }; }
static CONSOLE: OnceLock<Result<console_worker::ConsoleQueue, String>> = OnceLock::new();
#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct NativeSample { pub calls: u64, pub us: u64, pub bytes: u64, pub aux: u64 }
include!(concat!(env!("OUT_DIR"), "/perf_names.rs"));
const _: () = assert!(std::mem::size_of::<NativeSample>() == 32);
unsafe extern "C" {
    fn kv_render_set_options(options: u32);
    fn kv_perf_set_enabled(enabled: u32);
    fn kv_perf_snapshot(out: *mut NativeSample, count: u32) -> i32;
    fn kv_perf_console(line: *const u8, length: u32) -> i32;
}
pub fn set_options(options: u32) { unsafe { kv_render_set_options(options); } }
pub fn set_enabled(enabled: bool) { unsafe { kv_perf_set_enabled(u32::from(enabled)); } }
pub fn snapshot() -> Result<[NativeSample; NATIVE_NAMES.len()], &'static str> {
    let mut out = [NativeSample::default(); NATIVE_NAMES.len()];
    if unsafe { kv_perf_snapshot(out.as_mut_ptr(), out.len() as u32) } < 0 {
        return Err("native performance ABI mismatch");
    }
    Ok(out)
}
fn write_batch(lines: &[String]) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQUENCE: AtomicU32 = AtomicU32::new(0);
    for line in lines {
        crate::console_codec::emit_lines(line, SEQUENCE.fetch_add(1, Ordering::Relaxed), |part| {
            if unsafe { kv_perf_console(part.as_ptr(), part.len() as u32) } < 0 {
                return Err(std::io::Error::other("sceClibPrintf performance output failed"));
            }
            Ok(())
        })?;
    }
    Ok(())
}
fn queue() -> std::io::Result<&'static console_worker::ConsoleQueue> {
    CONSOLE.get_or_init(|| console_worker::ConsoleQueue::new(write_batch).map_err(|e| e.to_string()))
        .as_ref().map_err(|e| std::io::Error::other(e.clone()))
}
/// An entire window is accepted or dropped together, never blocks rendering.
pub struct ConsoleBatch;
impl ConsoleBatch {
    pub fn begin() -> Self {
        BATCH.with(|b| { assert!(b.borrow().is_none(), "nested console batch"); *b.borrow_mut() = Some(Vec::new()); });
        Self
    }
    pub fn finish(self) -> std::io::Result<()> {
        let lines = BATCH.with(|b| b.borrow_mut().take().unwrap_or_default());
        queue()?.submit(lines);
        Ok(())
    }
}
impl Drop for ConsoleBatch {
    fn drop(&mut self) { BATCH.with(|b| { b.borrow_mut().take(); }); }
}
pub fn console_stats() -> ConsoleStats {
    CONSOLE.get().and_then(|q| q.as_ref().ok()).map_or_else(ConsoleStats::default, |q| q.stats())
}
/// Strings are owned before return; the worker is the only printf caller.
pub fn console(line: &str) -> std::io::Result<()> {
    if option_env!("KNIFE_VITA_QUIET") == Some("1") { return Ok(()); }
    let captured = BATCH.with(|batch| {
        let mut batch = batch.borrow_mut();
        if let Some(lines) = &mut *batch { lines.push(line.to_owned()); true } else { false }
    });
    if !captured { queue()?.submit(vec![line.to_owned()]); }
    Ok(())
}

//! Lock-free cumulative counters. Read only by the optional five-second perf log.
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

pub static SUBMITTED: AtomicU32 = AtomicU32::new(0);
pub static OUTPUT: AtomicU32 = AtomicU32::new(0);
pub static UNDERRUNS: AtomicU32 = AtomicU32::new(0);
pub static IDLE: AtomicU32 = AtomicU32::new(0);
pub static QUEUE_FULL: AtomicU32 = AtomicU32::new(0);
pub static PEAK_QUEUED: AtomicU32 = AtomicU32::new(0);
static MIX_BLOCKS: AtomicU32 = AtomicU32::new(0);
static MIX_MAX_US: AtomicU32 = AtomicU32::new(0);
static MIX_LATE: AtomicU32 = AtomicU32::new(0);
static CLIPPED: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub submitted: u32, pub output: u32, pub underruns: u32, pub idle: u32,
    pub queue_full: u32, pub peak_queued: u32, pub mix_blocks: u32,
    pub mix_max_us: u32, pub mix_late: u32, pub clipped_samples: u32,
}
pub fn record_mix(micros: u32, clipped: u32) {
    MIX_BLOCKS.fetch_add(1, Relaxed);
    MIX_MAX_US.fetch_max(micros, Relaxed);
    if micros > 20_000 { MIX_LATE.fetch_add(1, Relaxed); }
    CLIPPED.fetch_add(clipped, Relaxed);
}
pub fn snapshot() -> Snapshot {
    Snapshot {
        submitted: SUBMITTED.load(Relaxed), output: OUTPUT.load(Relaxed),
        underruns: UNDERRUNS.load(Relaxed), idle: IDLE.load(Relaxed),
        queue_full: QUEUE_FULL.load(Relaxed), peak_queued: PEAK_QUEUED.load(Relaxed),
        mix_blocks: MIX_BLOCKS.load(Relaxed), mix_max_us: MIX_MAX_US.load(Relaxed),
        mix_late: MIX_LATE.load(Relaxed), clipped_samples: CLIPPED.load(Relaxed),
    }
}

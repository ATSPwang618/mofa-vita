//! Cumulative I/O accounting for the Vita performance log.
//!
//! The card read is measured at the seek+read_exact pair, so NX inflation and
//! image decode stay separate numbers; nothing here is inferred from timing
//! differences of larger scopes.
use std::sync::atomic::{AtomicU64, Ordering};

static CARD_US: AtomicU64 = AtomicU64::new(0);
static CARD_CALLS: AtomicU64 = AtomicU64::new(0);
static CARD_BYTES: AtomicU64 = AtomicU64::new(0);
static CARD_MAX_US: AtomicU64 = AtomicU64::new(0);
static INFLATE_US: AtomicU64 = AtomicU64::new(0);
static INFLATE_CALLS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Default, Clone, Copy)]
pub struct IoStats {
    pub card_read_us: u64,
    pub card_read_calls: u64,
    pub card_read_bytes: u64,
    pub max_card_read_us: u64,
    pub inflate_us: u64,
    pub inflate_calls: u64,
}

pub fn record_card_read(us: u64, bytes: u64) {
    CARD_US.fetch_add(us, Ordering::Relaxed);
    CARD_CALLS.fetch_add(1, Ordering::Relaxed);
    CARD_BYTES.fetch_add(bytes, Ordering::Relaxed);
    CARD_MAX_US.fetch_max(us, Ordering::Relaxed);
}

pub fn record_inflate(us: u64) {
    INFLATE_US.fetch_add(us, Ordering::Relaxed);
    INFLATE_CALLS.fetch_add(1, Ordering::Relaxed);
}

pub fn snapshot() -> IoStats {
    IoStats {
        card_read_us: CARD_US.load(Ordering::Relaxed),
        card_read_calls: CARD_CALLS.load(Ordering::Relaxed),
        card_read_bytes: CARD_BYTES.load(Ordering::Relaxed),
        max_card_read_us: CARD_MAX_US.load(Ordering::Relaxed),
        inflate_us: INFLATE_US.load(Ordering::Relaxed),
        inflate_calls: INFLATE_CALLS.load(Ordering::Relaxed),
    }
}
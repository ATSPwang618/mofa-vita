//! Initial PC image budgets, based on 2560x1440 RGBA assets. These are limits,
//! not eager reservations or a cap on the driver/process's total memory.
// PC font files, prerendered indexes and evictable glyph masks use CPU RAM.
// The portable default remains separate from this desktop policy. Prerendered
// glyph payloads stay in storage, so a large font collection need not fit here.
pub const FONT_BYTES: usize = 64 * 1024 * 1024;
const FRAME_BYTES: usize = 2560 * 1440 * 4;
pub const RESIDENT_BYTES: usize = (FRAME_BYTES * 16).next_power_of_two();
pub const SCRATCH_BYTES: usize = (FRAME_BYTES * 8).next_power_of_two();
// Resident images may use unused scratch allowance, while both pools remain
// charged against their original combined texture ceiling. RESIDENT_BYTES is
// the cache eviction target; scratch keeps its own per-pool maximum as well.
pub const TEXTURE_BYTES: usize = RESIDENT_BYTES + SCRATCH_BYTES;
pub const STAGING_BYTES: usize = (FRAME_BYTES * 4).next_power_of_two();
pub const BITMAP_BYTES: usize = (FRAME_BYTES * 4).next_power_of_two();
// Cache aliases retain resident resources, not another GPU pool.
pub const CACHE_BYTES: usize = (FRAME_BYTES * 4).next_power_of_two();

//! Shared cache ceilings. Native admission also checks the kernel's free CDRAM;
//! these are limits, never eagerly allocated reservations.
pub const MIB: usize = 1024 * 1024;
pub const CPU_SOURCE_BYTES: usize = 64 * MIB;
pub const CPU_GRADED_BYTES: usize = 32 * MIB;
pub const CPU_SOUND_BYTES: usize = 16 * MIB;
pub const GPU_RESOURCE_BYTES: usize = 56 * MIB;
pub const GPU_SOURCE_BYTES: usize = 32 * MIB;
pub const GPU_TEXT_BYTES: usize = 12 * MIB;
pub const GPU_TEXT_ENTRIES: usize = 8;
pub const GPU_GROUP_BYTES: usize = 12 * MIB;
pub const GPU_GROUP_ENTRIES: usize = 6;
/// One optional native540p static prefix. Kernel headroom is checked before allocation.
pub const GPU_STAGE_PREFIX_BYTES: usize = 3 * MIB;
pub const GPU_PRESSURE_HEADROOM: usize = 8 * MIB;
pub const GPU_CACHE_SPARE_BYTES: usize = MIB;
pub const CPU_PRESSURE_HEADROOM: usize = 32 * MIB;
pub const CPU_TARGET_HEADROOM: usize = 48 * MIB;

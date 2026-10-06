//! MAIN heap reservations and shared texture admission for the Vita host.
pub const MIB: usize = 1024 * 1024;
pub const BITMAP_BYTES: usize = 64 * MIB;
pub const FONT_BYTES: usize = 8 * MIB;
pub const VM_STACK_BYTES: usize = MIB;
pub const AUDIO_STACK_BYTES: usize = 256 * 1024;
// Newlib reserves this MAIN block up front. Leave room for PVR's separate
// USER_NC allocations even when most of the Rust heap is unused.
pub const NEWLIB_HEAP_BYTES: usize = 160 * MIB;
// ATTRIBUTE2=12 gives the application 365 MiB MAIN. PVR textures can use both
// CDRAM and USER_NC: allow 96 MiB from each, with CDRAM space left for surfaces.
// This is a payload ceiling, not a reservation. The driver also needs MAIN for
// uploads, metadata and alignment; native allocation errors remain authoritative.
pub const GRAPHICS_BYTES: usize = 192 * MIB;
pub const RESIDENT_BYTES: usize = GRAPHICS_BYTES;
pub const SCRATCH_BYTES: usize = 48 * MIB;
pub const TILE_EDGE: u32 = 1024;
// Leave room for PVR backing copies and page-aligned driver allocations.
pub(crate) const GRAPHICS_HEADROOM_BYTES: usize = 8 * MIB;

/// Kernel memory available to PVR's USER_NC upload buffers.
/// General upload paths need UNC staging even when final storage fits in CDRAM.
/// Counting CDRAM here would hide that USER pressure.
/// Free space retained inside the UNC heap is excluded: this is a reclamation
/// hint, never a reason to reject an allocation.
pub(crate) fn graphics_free() -> Option<usize> {
    #[cfg(target_os = "vita")]
    {
        free_info().map(|info| info.size_user.max(0) as usize)
    }
    #[cfg(not(target_os = "vita"))]
    None
}

pub fn graphics_config(staging: krkr_protocol::budget::Budget) -> krkr_render_gles2::Config {
    // Resident and scratch allocations borrow from one shared payload budget.
    // Existing scratch/work surfaces still reduce resident.available(), and
    // neither allocation path can exceed the combined software budget. This
    // counts payloads, not physical CDRAM: PVR can also map USER_NC memory.
    // Driver surfaces, alignment and other GPU allocations remain outside
    // this admission limit; the host also reclaims under physical pressure.
    let graphics = krkr_protocol::budget::Budget::new(GRAPHICS_BYTES);
    graphics.set_profile_name("memory.graphics_bytes");
    krkr_render_gles2::Config {
        resident: graphics.child(RESIDENT_BYTES),
        scratch: graphics.child(SCRATCH_BYTES),
        staging,
        tile_edge: TILE_EDGE,
        render_target_cache_entries: 8,
        render_target_cache_bytes: 8 * MIB,
        ..Default::default()
    }
}

/// Kernel free blocks are separate from unused space inside newlib's fixed
/// heap, and from free allocations retained inside the PVR texture heaps.
#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
pub fn trace_free(stage: &str) {
    krkr_protocol::diagnostic!("[VITA][MEM] {stage}: {}", free_report());
}

#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
pub fn free_report() -> String {
    if let Some(info) = free_info() {
        format!(
            "kernel_free main={}KiB cdram={}KiB phycont={}KiB; newlib_heap={}MiB graphics_cap={}MiB resident_cap={}MiB scratch_cap={}MiB",
            info.size_user / 1024,
            info.size_cdram / 1024,
            info.size_phycont / 1024,
            NEWLIB_HEAP_BYTES / MIB,
            GRAPHICS_BYTES / MIB,
            RESIDENT_BYTES / MIB,
            SCRATCH_BYTES / MIB
        )
    } else {
        "kernel free-memory query failed".into()
    }
}

#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
fn free_info() -> Option<vitasdk_sys::SceKernelFreeMemorySizeInfo> {
    let mut info = vitasdk_sys::SceKernelFreeMemorySizeInfo {
        size: std::mem::size_of::<vitasdk_sys::SceKernelFreeMemorySizeInfo>() as _,
        size_user: 0,
        size_cdram: 0,
        size_phycont: 0,
    };
    (unsafe { vitasdk_sys::sceKernelGetFreeMemorySize(&mut info) } >= 0).then_some(info)
}

#[cfg(test)]
#[path = "../tests/memory/internal.rs"]
mod tests;

pub fn storage_limits() -> krkr_engine::assets::Limits {
    krkr_engine::assets::Limits {
        max_read_bytes: 32 * MIB,
        max_index_bytes: 8 * MIB,
        max_entries: 100_000,
        // Games with separate image, voice and music archives exceed eight
        // indexes. Keep them while the shared 16 MiB index budget allows it.
        max_cached_archives: 16,
        max_cached_index_bytes: 16 * MIB,
    }
}

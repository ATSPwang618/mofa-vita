#pragma once

#include "mofa/vita_memory_budget.hpp"

#include <cstddef>
#include <cstdint>
#include <limits>

namespace mofa {

inline constexpr std::size_t kVitaBitmapMemblockThreshold =
    1u * 1024u * 1024u;
inline constexpr std::size_t kVitaBitmapMemblockPage = 4096u;
inline constexpr std::size_t kVitaBitmapAllocationHeaderBytes = 32u;
inline constexpr std::size_t kVitaBitmapPayloadAlignment = 16u;

// The retry floor. By the time a reclaim runs the engine has already dropped its
// graphic cache and compressed its textures, the caller is about to abort the
// game with "Cannot allocate memory for Bitmap", and a bitmap is exactly the
// kind of allocation that can be given back later. Let that retry use the
// margin down to this floor instead of failing.
inline constexpr std::size_t kVitaBitmapMemblockEmergencyReserveBytes =
    4u * 1024u * 1024u;

constexpr bool vita_bitmap_uses_memblock(std::size_t requested) {
    return requested >= kVitaBitmapMemblockThreshold;
}

constexpr std::size_t vita_bitmap_memblock_bytes(std::size_t requested) {
    constexpr std::size_t overhead = kVitaBitmapAllocationHeaderBytes +
                                     kVitaBitmapPayloadAlignment - 1;
    if (requested > std::numeric_limits<std::size_t>::max() - overhead)
        return 0;
    const std::size_t total = requested + overhead;
    if (total > std::numeric_limits<std::size_t>::max() -
                    (kVitaBitmapMemblockPage - 1))
        return 0;
    return (total + kVitaBitmapMemblockPage - 1) &
           ~(kVitaBitmapMemblockPage - 1);
}

// Whether a large bitmap may take a USER_RW memblock right now.
//
// This used to compare against a fixed 64 MiB budget, which turned a guess
// into a hard ceiling: once a 1280x720 project had ~18 full-screen surfaces
// live, every further bitmap fell back to the fixed newlib heap and then
// failed outright, even with most of the console's memory unused. Ask the
// kernel instead, and keep a reserve so bitmaps cannot starve the subsystems
// whose allocations are not recoverable.
// The policy with an explicit reserve, so the post-reclaim retry can trade part
// of the safety margin for not aborting the title.
constexpr bool vita_bitmap_memblock_budget_allows_with_reserve(
    std::size_t free_user_bytes, std::size_t requested,
    std::size_t reserve_bytes) {
    const std::size_t mapped = vita_bitmap_memblock_bytes(requested);
    if (mapped == 0) return false;
    if (free_user_bytes <= reserve_bytes) return false;
    return mapped <= free_user_bytes - reserve_bytes;
}

constexpr bool vita_bitmap_memblock_budget_allows(std::size_t free_user_bytes,
                                                  std::size_t requested) {
    return vita_bitmap_memblock_budget_allows_with_reserve(
        free_user_bytes, requested, kVitaBitmapMemblockReserveBytes);
}

void* vita_bitmap_allocate(std::size_t size);

// Allocation attempt that may consume part of the reserve.  Only for the
// retry after mofa_yuri_reclaim_bitmap_memory() has already run.
void* vita_bitmap_allocate_after_reclaim(std::size_t size);

void vita_bitmap_deallocate(void* memory) noexcept;
std::uint64_t vita_bitmap_memblock_bytes_live() noexcept;

// One [mofa-mem] trace line describing what the allocator can see right now.
// Used on the pressure/failure path so the next run says whether an allocation
// was refused by policy or by the kernel.
void vita_bitmap_log_memory_state(const char* tag);

} // namespace mofa

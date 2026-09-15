#pragma once

#include <cstdint>

namespace mofa {

// Yuri's ARM backend replaces TvPgl's generated scalar kernels with
// hand-written NEON ones during TVPGL_ASM_Init(). On a Cortex-A9 those NEON
// routines are the fast path, but the replacement decision is made from the
// CPU feature bits alone: a layer that implements the ARM feature set without
// executing it natively (an emulator's JIT falling back to its interpreter for
// the vsubhn blend sequences) keeps the NEON kernels *correct* while making
// them far slower than the scalar code they replaced.
//
// The launcher therefore measures both implementations on the device it is
// actually running on and keeps the NEON kernel only when it is clearly
// faster. The measurement is a few thousand blended pixels, so it costs well
// under one frame and no longer needs a data-directory flag to describe the
// host it was started on.
inline constexpr unsigned kTvpglKernelMinAdvantagePercent = 15;

// True when the candidate kernel is at least `min_advantage_percent` faster
// than the reference. An unmeasurable comparison keeps the reference: a
// substitute has to prove itself, not merely tie.
constexpr bool tvpgl_kernel_candidate_wins(
    std::uint64_t reference_us, std::uint64_t candidate_us,
    unsigned min_advantage_percent) noexcept {
    if (reference_us == 0 || candidate_us == 0) return false;
    if (min_advantage_percent >= 100) return false;
    // Integer form of candidate <= reference * (100 - advantage) / 100.
    return candidate_us * 100u <= reference_us * (100u - min_advantage_percent);
}

// One measured kernel comparison, kept for the hardware log.
struct TvpglKernelChoice {
    const char* name;
    std::uint64_t reference_us;
    std::uint64_t candidate_us;
    bool candidate_selected;
};

// Applies the rule to one TvPgl function pointer.  `installed` already holds
// the candidate (NEON) implementation and is left untouched when that
// implementation wins, so a selection costs nothing when nothing changes.
template <typename Kernel>
TvpglKernelChoice select_tvpgl_kernel(
    const char* name, Kernel& installed, Kernel reference,
    std::uint64_t reference_us, std::uint64_t candidate_us,
    unsigned min_advantage_percent = kTvpglKernelMinAdvantagePercent) noexcept {
    const bool candidate_selected = tvpgl_kernel_candidate_wins(
        reference_us, candidate_us, min_advantage_percent);
    if (!candidate_selected) installed = reference;
    return TvpglKernelChoice{name, reference_us, candidate_us,
                             candidate_selected};
}

// Measures every blended kernel family that the software compositor uses
// heavily and keeps Yuri's NEON replacement only where it is faster.  Honors
// the explicit scalar-kernel request (ux0:data/mofa-vita/tvpgl-scalar) as a
// hard override: that flag means "never install NEON", not "NEON looks slow".
void apply_device_tvpgl_kernel_policy();

} // namespace mofa

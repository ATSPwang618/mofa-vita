#include "tjsCommHead.h"
#include "tvpgl.h"

#include "mofa/retail_bootstrap.hpp"
#include "mofa/tvpgl_kernel_benchmark.hpp"
#include "mofa/tvpgl_kernel_policy.hpp"

#include <psp2/kernel/processmgr.h>

#include <cstdint>
#include <cstdio>

// Yuri installs its hand-written NEON kernels whenever TVPGL_ASM_Init() runs,
// and that is the right answer on a Cortex-A9. It is not necessarily the right
// answer on the machine the launcher is started on: an emulator can implement
// the ARM feature set without executing it natively, which keeps the NEON
// kernels correct but slower than the scalar code they replaced.
//
// The runtime therefore times both implementations of every kernel family the
// software compositor uses heavily and keeps the NEON slot only where NEON
// actually wins. On hardware nothing changes (NEON is kept); where NEON is
// penalised, the same build silently stays on the generated scalar kernels.
// The explicit ux0:data/mofa-vita/tvpgl-scalar request stays a hard override.
namespace {

// One kilobyte of destination pixels per call is large enough for the kernel
// to dominate the timing syscall and small enough that a losing candidate is
// abandoned after a few milliseconds.
constexpr int kBlendPixels = 1024;
constexpr int kStretchPixels = 512;
constexpr int kTimingRounds = 3;
// A round that already costs this much has lost the comparison no matter how
// the remaining rounds would have gone. Bounding each round keeps a
// substitution that an emulator has to interpret from stretching startup.
constexpr std::uint64_t kRoundBudgetUs = 25000;
constexpr unsigned kMaxRepetitions = 16;

alignas(64) tjs_uint32 benchmark_dest[kBlendPixels];
alignas(64) tjs_uint32 benchmark_source[kBlendPixels];

using BlendKernel = void (*)(tjs_uint32*, const tjs_uint32*, tjs_int);
using BlendOpacityKernel = void (*)(tjs_uint32*, const tjs_uint32*, tjs_int,
                                   tjs_int);
using StretchKernel = void (*)(tjs_uint32*, tjs_int, const tjs_uint32*,
                              tjs_int, tjs_int);
using StretchOpacityKernel = void (*)(tjs_uint32*, tjs_int, const tjs_uint32*,
                                     tjs_int, tjs_int, tjs_int);

std::uint64_t now_us() {
    return static_cast<std::uint64_t>(sceKernelGetProcessTimeWide());
}

void fill_benchmark_pixels() {
    // Transparent, translucent and opaque sources in one buffer: the scalar
    // and NEON blend kernels branch on the source alpha, so the workload has
    // to contain every branch for the comparison to mean anything.
    for (int i = 0; i < kBlendPixels; ++i) {
        const tjs_uint32 index = static_cast<tjs_uint32>(i);
        benchmark_source[i] = ((index * 2654435761u) & 0x00ffffffu) |
                              (((index * 37u) % 256u) << 24);
        benchmark_dest[i] = ((index * 40503u) & 0x00ffffffu) |
                            (((index * 89u) % 256u) << 24);
    }
}

template <typename Invoke>
std::uint64_t measure_kernel(const Invoke& invoke, int repetitions) {
    std::uint64_t best = 0;
    for (int round = 0; round < kTimingRounds; ++round) {
        const std::uint64_t started = now_us();
        for (int i = 0; i < repetitions; ++i) invoke();
        const std::uint64_t elapsed = now_us() - started;
        if (best == 0 || elapsed < best) best = elapsed;
        if (elapsed > kRoundBudgetUs) break;
    }
    return best;
}

int repetitions_for(std::uint64_t single_call_us) {
    // Aim each timed round at a few hundred microseconds: long enough to
    // dwarf the syscall granularity, short enough that a slow candidate is
    // still detected quickly.
    if (single_call_us == 0) return kMaxRepetitions;
    if (single_call_us < 25) return kMaxRepetitions;
    if (single_call_us < 80) return 8;
    if (single_call_us < 250) return 2;
    return 1;
}

template <typename Kernel, typename Invoke>
mofa::TvpglKernelChoice compare_kernel(const char* name, Kernel& installed,
                                       Kernel reference, const Invoke& invoke) {
    // Warm both implementations first so caches and any translation state are
    // identical before the timed rounds.
    invoke(reference);
    invoke(installed);
    const int repetitions =
        repetitions_for(measure_kernel([&] { invoke(reference); }, 1));
    const std::uint64_t reference_us =
        measure_kernel([&] { invoke(reference); }, repetitions);
    const std::uint64_t candidate_us =
        measure_kernel([&] { invoke(installed); }, repetitions);
    return mofa::select_tvpgl_kernel(name, installed, reference, reference_us,
                                     candidate_us);
}

void trace_choice(const mofa::TvpglKernelChoice& choice) {
    // mofa_boot_trace does not copy the string, so the buffer has to outlive
    // the call; one shared static is enough because the trace is synchronous.
    static char line[160];
    std::snprintf(line, sizeof line, "[mofa-tvpgl] %s scalar=%lluus neon=%lluus pick=%s",
                  choice.name,
                  static_cast<unsigned long long>(choice.reference_us),
                  static_cast<unsigned long long>(choice.candidate_us),
                  choice.candidate_selected ? "neon" : "scalar");
    mofa_boot_trace(line);
}

} // namespace

namespace mofa {

void apply_device_tvpgl_kernel_policy() {
    if (scalar_tvpgl_kernels_requested()) {
        mofa_boot_trace("[mofa-tvpgl] scalar-requested-skip-benchmark");
        return;
    }
    fill_benchmark_pixels();

    // A KAG frame composites a normal layer blend, a blend of a scaled image
    // and the opacity variants of both. Measuring the destination-alpha
    // variants as well covers the layer types a layer tree actually uses;
    // the additive-alpha slots are excluded because the exact-alpha policy
    // already pins them to the generated scalar implementations.
    const auto blend = [](BlendKernel kernel) {
        kernel(benchmark_dest, benchmark_source, kBlendPixels);
    };
    const auto blend_opacity = [](BlendOpacityKernel kernel) {
        kernel(benchmark_dest, benchmark_source, kBlendPixels, 100);
    };
    const auto stretch = [](StretchKernel kernel) {
        kernel(benchmark_dest, kStretchPixels, benchmark_source, 0, 1 << 16);
    };
    const auto stretch_opacity = [](StretchOpacityKernel kernel) {
        kernel(benchmark_dest, kStretchPixels, benchmark_source, 0, 1 << 16,
               100);
    };

    trace_choice(compare_kernel("TVPAlphaBlend", TVPAlphaBlend,
                                TVPAlphaBlend_c, blend));
    trace_choice(compare_kernel("TVPAlphaBlend_d", TVPAlphaBlend_d,
                                TVPAlphaBlend_d_c, blend));
    trace_choice(compare_kernel("TVPAlphaBlend_o", TVPAlphaBlend_o,
                                TVPAlphaBlend_o_c, blend_opacity));
    trace_choice(compare_kernel("TVPAlphaBlend_do", TVPAlphaBlend_do,
                                TVPAlphaBlend_do_c, blend_opacity));
    trace_choice(compare_kernel("TVPStretchAlphaBlend", TVPStretchAlphaBlend,
                                TVPStretchAlphaBlend_c, stretch));
    trace_choice(compare_kernel("TVPStretchAlphaBlend_d",
                                TVPStretchAlphaBlend_d,
                                TVPStretchAlphaBlend_d_c, stretch));
    trace_choice(compare_kernel("TVPStretchAlphaBlend_o",
                                TVPStretchAlphaBlend_o,
                                TVPStretchAlphaBlend_o_c, stretch_opacity));
    trace_choice(compare_kernel("TVPStretchAlphaBlend_do",
                                TVPStretchAlphaBlend_do,
                                TVPStretchAlphaBlend_do_c, stretch_opacity));
    mofa_boot_trace("yuri-tvpgl-kernels-measured-on-device");
}

} // namespace mofa

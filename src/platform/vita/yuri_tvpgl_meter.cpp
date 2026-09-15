#include "tjsCommHead.h"
#include "tvpgl.h"

#include "mofa/retail_bootstrap.hpp"
#include "mofa/tvpgl_pixel_meter.hpp"

#include <psp2/kernel/processmgr.h>

#include <atomic>
#include <cstdint>
#include <cstdio>
#include <tuple>

// Per-family pixel accounting for the software compositor.
//
// The event loop can say how long the engine stage took, but not how much of
// it was script interpretation and how much was pixels. This unit answers that
// by counting destination pixels per blend family (a relaxed atomic add per
// scanline, which is far cheaper than the blend it describes) and multiplying
// them by the per-pixel cost of the kernel that is actually installed on this
// device. Both halves are measured here, so the estimate describes a Cortex-A9
// exactly as well as it describes an emulator.
namespace {

std::atomic<std::uint64_t>
    family_pixels[mofa::kTvpgPixelFamilyCount];
std::atomic<std::uint64_t> family_calls[mofa::kTvpgPixelFamilyCount];
mofa::TvpgPixelFamilyCosts family_costs{};
bool meter_installed = false;

std::uint64_t now_us() {
    return static_cast<std::uint64_t>(sceKernelGetProcessTimeWide());
}

void add_family_pixels(int family, std::uint64_t pixels) {
    // Row-split blends also run on the two render workers, so the counters
    // must be safe from every application thread.
    family_pixels[family].fetch_add(pixels, std::memory_order_relaxed);
    family_calls[family].fetch_add(1, std::memory_order_relaxed);
}

// One forwarding shell per kernel. The instantiation is keyed by a tag type so
// that kernels sharing a signature (there are many) each keep their own
// original pointer; keying by type alone would make the second installation
// forward both slots to the same kernel.
template <typename SlotTag, typename Kernel, int Family, int LengthIndex>
struct PixelCountingKernel;

template <typename SlotTag, typename R, typename... Args, int Family,
          int LengthIndex>
struct PixelCountingKernel<SlotTag, R (*)(Args...), Family, LengthIndex> {
    using Kernel = R (*)(Args...);
    static inline Kernel next = nullptr;

    static R thunk(Args... args) {
        const auto& packed = std::forward_as_tuple(args...);
        const std::int64_t length =
            static_cast<std::int64_t>(std::get<LengthIndex>(packed));
        if (length > 0)
            add_family_pixels(Family, static_cast<std::uint64_t>(length));
        next(args...);
    }
};

template <typename SlotTag, int Family, int LengthIndex, typename Kernel>
void install_counting_kernel(Kernel& slot) {
    if (!slot) return;
    using Counter = PixelCountingKernel<SlotTag, Kernel, Family, LengthIndex>;
    Counter::next = slot;
    slot = &Counter::thunk;
}

// LENGTH_INDEX is the position of the destination pixel count in that kernel's
// argument list. Kirikiri spells it last for the packed blend families, second
// for the stretch families, and second for fills.
#define MOFA_COUNT_PIXELS(SLOT, FAMILY, LENGTH_INDEX)                     \
    do {                                                                  \
        struct SlotTag_##SLOT {};                                         \
        install_counting_kernel<SlotTag_##SLOT, FAMILY, LENGTH_INDEX>(    \
            SLOT);                                                        \
    } while (0)

void install_shells() {
    MOFA_COUNT_PIXELS(TVPAlphaBlend, mofa::kTvpgPixelBlend, 2);
    MOFA_COUNT_PIXELS(TVPAlphaBlend_o, mofa::kTvpgPixelBlend, 2);
    MOFA_COUNT_PIXELS(TVPAlphaBlend_d, mofa::kTvpgPixelBlend, 2);
    MOFA_COUNT_PIXELS(TVPAlphaBlend_do, mofa::kTvpgPixelBlend, 2);
    MOFA_COUNT_PIXELS(TVPAlphaBlend_a, mofa::kTvpgPixelAdditiveDest, 2);
    MOFA_COUNT_PIXELS(TVPAlphaBlend_ao, mofa::kTvpgPixelAdditiveDest, 2);
    // Kirikiri draws masked glyph runs and constant-opacity layer content with
    // the constant-* family, so without these the text path is invisible.
    MOFA_COUNT_PIXELS(TVPConstAlphaBlend, mofa::kTvpgPixelBlend, 2);
    MOFA_COUNT_PIXELS(TVPConstAlphaBlend_d, mofa::kTvpgPixelBlend, 2);
    MOFA_COUNT_PIXELS(TVPConstAlphaBlend_SD, mofa::kTvpgPixelBlend, 3);
    MOFA_COUNT_PIXELS(TVPConstAlphaBlend_SD_d, mofa::kTvpgPixelBlend, 3);
    MOFA_COUNT_PIXELS(TVPConstAlphaBlend_a, mofa::kTvpgPixelAdditiveDest, 2);
    MOFA_COUNT_PIXELS(TVPConstAlphaBlend_SD_a, mofa::kTvpgPixelAdditiveDest, 3);
    MOFA_COUNT_PIXELS(TVPConstColorAlphaBlend_a, mofa::kTvpgPixelAdditiveDest, 1);
    MOFA_COUNT_PIXELS(TVPConstColorAlphaBlend, mofa::kTvpgPixelCopyFill, 1);
    MOFA_COUNT_PIXELS(TVPConstColorAlphaBlend_d, mofa::kTvpgPixelCopyFill, 1);
    MOFA_COUNT_PIXELS(TVPAdditiveAlphaBlend, mofa::kTvpgPixelAdditive, 2);
    MOFA_COUNT_PIXELS(TVPAdditiveAlphaBlend_o, mofa::kTvpgPixelAdditive, 2);
    MOFA_COUNT_PIXELS(TVPAdditiveAlphaBlend_a, mofa::kTvpgPixelAdditive, 2);
    MOFA_COUNT_PIXELS(TVPAdditiveAlphaBlend_ao, mofa::kTvpgPixelAdditive, 2);
    MOFA_COUNT_PIXELS(TVPStretchAlphaBlend, mofa::kTvpgPixelStretch, 1);
    MOFA_COUNT_PIXELS(TVPStretchAlphaBlend_o, mofa::kTvpgPixelStretch, 1);
    MOFA_COUNT_PIXELS(TVPStretchAlphaBlend_d, mofa::kTvpgPixelStretch, 1);
    MOFA_COUNT_PIXELS(TVPStretchAlphaBlend_do, mofa::kTvpgPixelStretch, 1);
    MOFA_COUNT_PIXELS(TVPStretchAlphaBlend_a, mofa::kTvpgPixelStretch, 1);
    MOFA_COUNT_PIXELS(TVPStretchAlphaBlend_ao, mofa::kTvpgPixelStretch, 1);
    MOFA_COUNT_PIXELS(TVPStretchAdditiveAlphaBlend,
                      mofa::kTvpgPixelStretchAdditive, 1);
    MOFA_COUNT_PIXELS(TVPStretchAdditiveAlphaBlend_o,
                      mofa::kTvpgPixelStretchAdditive, 1);
    MOFA_COUNT_PIXELS(TVPStretchAdditiveAlphaBlend_a,
                      mofa::kTvpgPixelStretchAdditive, 1);
    MOFA_COUNT_PIXELS(TVPStretchAdditiveAlphaBlend_ao,
                      mofa::kTvpgPixelStretchAdditive, 1);
    MOFA_COUNT_PIXELS(TVPCopyOpaqueImage, mofa::kTvpgPixelCopyFill, 2);
    MOFA_COUNT_PIXELS(TVPStretchCopyOpaqueImage, mofa::kTvpgPixelCopyFill, 1);
    MOFA_COUNT_PIXELS(TVPFillARGB, mofa::kTvpgPixelCopyFill, 1);
    MOFA_COUNT_PIXELS(TVPFillColor, mofa::kTvpgPixelCopyFill, 1);
    MOFA_COUNT_PIXELS(TVPApplyColorMap, mofa::kTvpgPixelColorMap, 2);
    MOFA_COUNT_PIXELS(TVPApplyColorMap_o, mofa::kTvpgPixelColorMap, 2);
    MOFA_COUNT_PIXELS(TVPApplyColorMap_a, mofa::kTvpgPixelColorMap, 2);
    MOFA_COUNT_PIXELS(TVPApplyColorMap_ao, mofa::kTvpgPixelColorMap, 2);
    MOFA_COUNT_PIXELS(TVPAlphaColorMat, mofa::kTvpgPixelColorMap, 2);
}

// The probe covers the pixel count of one 1024-pixel (or 512-pixel) scanline
// run, repeated enough times for the microsecond clock to resolve it. Every
// family is measured through the kernel that is installed on this device, so a
// host that has to interpret NEON reports the interpreter's cost and hardware
// reports NEON's.
constexpr int kProbeBlendPixels = 1024;
constexpr int kProbeStretchPixels = 512;
constexpr int kProbeRepetitions = 64;

alignas(64) tjs_uint32 probe_dest[kProbeBlendPixels];
alignas(64) tjs_uint32 probe_source[kProbeBlendPixels];
alignas(64) tjs_uint8 probe_palette[kProbeBlendPixels];

void fill_probe_data() {
    for (int i = 0; i < kProbeBlendPixels; ++i) {
        const tjs_uint32 index = static_cast<tjs_uint32>(i);
        probe_source[i] = ((index * 2654435761u) & 0x00ffffffu) |
                          (((index * 41u) % 256u) << 24);
        probe_dest[i] = ((index * 40503u) & 0x00ffffffu) |
                        (((index * 97u) % 256u) << 24);
        probe_palette[i] = static_cast<tjs_uint8>((index * 29u) % 256u);
    }
}

template <typename Invoke>
double measure_ns_per_pixel(const Invoke& invoke, int pixels) {
    invoke();
    std::uint64_t best_us = 0;
    for (int round = 0; round < 3; ++round) {
        const std::uint64_t started = now_us();
        for (int repeat = 0; repeat < kProbeRepetitions; ++repeat) invoke();
        const std::uint64_t elapsed = now_us() - started;
        if (best_us == 0 || elapsed < best_us) best_us = elapsed;
    }
    if (best_us == 0) return 0.0;
    const double measured_pixels = static_cast<double>(pixels) *
                                   static_cast<double>(kProbeRepetitions);
    return static_cast<double>(best_us) * 1000.0 / measured_pixels;
}

// A missing pointer means TVPInitTVPGL never installed that slot: report the
// family as unmeasured instead of calling through null.
template <typename Kernel, typename Invoke>
double probe_kernel_cost(Kernel kernel, const Invoke& invoke, int pixels) {
    if (!kernel) return 0.0;
    return measure_ns_per_pixel(invoke, pixels);
}

void probe_family_costs() {
    const int blend_family = mofa::kTvpgPixelBlend;
    const int additive_dest_family = mofa::kTvpgPixelAdditiveDest;
    const int additive_family = mofa::kTvpgPixelAdditive;
    const int stretch_family = mofa::kTvpgPixelStretch;
    const int stretch_additive_family = mofa::kTvpgPixelStretchAdditive;
    const int copy_fill_family = mofa::kTvpgPixelCopyFill;
    const int color_map_family = mofa::kTvpgPixelColorMap;

    family_costs.ns_per_pixel[blend_family] = probe_kernel_cost(
        TVPAlphaBlend,
        [] { TVPAlphaBlend(probe_dest, probe_source, kProbeBlendPixels); },
        kProbeBlendPixels);
    family_costs.ns_per_pixel[additive_dest_family] = probe_kernel_cost(
        TVPAlphaBlend_a,
        [] { TVPAlphaBlend_a(probe_dest, probe_source, kProbeBlendPixels); },
        kProbeBlendPixels);
    family_costs.ns_per_pixel[additive_family] = probe_kernel_cost(
        TVPAdditiveAlphaBlend,
        [] {
            TVPAdditiveAlphaBlend(probe_dest, probe_source,
                                  kProbeBlendPixels);
        },
        kProbeBlendPixels);
    family_costs.ns_per_pixel[stretch_family] = probe_kernel_cost(
        TVPStretchAlphaBlend,
        [] {
            TVPStretchAlphaBlend(probe_dest, kProbeStretchPixels, probe_source,
                                 0, 1 << 16);
        },
        kProbeStretchPixels);
    family_costs.ns_per_pixel[stretch_additive_family] = probe_kernel_cost(
        TVPStretchAdditiveAlphaBlend,
        [] {
            TVPStretchAdditiveAlphaBlend(probe_dest, kProbeStretchPixels,
                                         probe_source, 0, 1 << 16);
        },
        kProbeStretchPixels);
    // Opaque copy is the conservative representative of the copy/fill family:
    // a solid fill is cheaper per pixel, so a frame dominated by fills reports
    // a slightly high estimate instead of a low one.
    family_costs.ns_per_pixel[copy_fill_family] = probe_kernel_cost(
        TVPCopyOpaqueImage,
        [] { TVPCopyOpaqueImage(probe_dest, probe_source, kProbeBlendPixels); },
        kProbeBlendPixels);
    family_costs.ns_per_pixel[color_map_family] = probe_kernel_cost(
        TVPApplyColorMap,
        [] {
            TVPApplyColorMap(probe_dest, probe_palette, kProbeBlendPixels,
                             0x80c0d0e0u);
        },
        kProbeBlendPixels);
}

void trace_costs() {
    // mofa_boot_trace does not copy the string, so the buffer must outlive it.
    static char line[256];
    std::snprintf(
        line, sizeof line,
        "[mofa-meter] ns/px blend=%.2f adddest=%.2f add=%.2f stretch=%.2f "
        "sadd=%.2f copyfill=%.2f cmap=%.2f",
        family_costs.ns_per_pixel[mofa::kTvpgPixelBlend],
        family_costs.ns_per_pixel[mofa::kTvpgPixelAdditiveDest],
        family_costs.ns_per_pixel[mofa::kTvpgPixelAdditive],
        family_costs.ns_per_pixel[mofa::kTvpgPixelStretch],
        family_costs.ns_per_pixel[mofa::kTvpgPixelStretchAdditive],
        family_costs.ns_per_pixel[mofa::kTvpgPixelCopyFill],
        family_costs.ns_per_pixel[mofa::kTvpgPixelColorMap]);
    mofa_boot_trace(line);
}

} // namespace

namespace mofa {

void install_tvpgl_pixel_meter() {
    if (meter_installed) return;
    meter_installed = true;
    fill_probe_data();
    probe_family_costs();
    trace_costs();
    install_shells();
    mofa_boot_trace("yuri-pixel-meter-installed");
}

const TvpgPixelFamilyCosts& tvpgl_pixel_costs() {
    return family_costs;
}

void tvpgl_pixel_meter_take(std::uint64_t* pixels, std::uint64_t* calls) {
    if (!pixels || !calls) return;
    for (int family = 0; family < kTvpgPixelFamilyCount; ++family) {
        pixels[family] =
            family_pixels[family].exchange(0, std::memory_order_relaxed);
        calls[family] =
            family_calls[family].exchange(0, std::memory_order_relaxed);
    }
}

} // namespace mofa

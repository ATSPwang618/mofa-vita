#pragma once

#include <cstdint>

namespace mofa {

// The pixel families a KAG layer tree actually reaches. Counting them is what
// separates "script work" from "pixel work" inside the engine stage: on a
// 444 MHz Cortex-A9 the two cannot be planned together, and the existing
// [mofa-stage] line reports them as one "script" number.
//
// The split follows Kirikiri's blend suffixes: a normal layer blend, the
// destination-additive-alpha blend used when something is drawn *into* an
// ltAddAlpha layer (this is the full-screen message layer's text path), the
// source-additive blend used when such a layer is composited onto the surface,
// their scaled and opacity variants, opaque copies/fills, and the palette
// colour-map path.
enum TvpgPixelFamily : int {
    kTvpgPixelBlend = 0,
    kTvpgPixelAdditiveDest,
    kTvpgPixelAdditive,
    kTvpgPixelStretch,
    kTvpgPixelStretchAdditive,
    kTvpgPixelCopyFill,
    kTvpgPixelColorMap,
    kTvpgPixelFamilyCount
};

// Nanoseconds per destination pixel for the kernel that is actually installed
// on this device. Zero means the probe could not measure that family, and the
// estimator then reports no time for it rather than inventing one.
struct TvpgPixelFamilyCosts {
    double ns_per_pixel[kTvpgPixelFamilyCount];
};

// Microseconds a family spends on `pixels` destination pixels. Pure arithmetic
// so the host test pins the conversion and its rounding exactly.
constexpr std::uint64_t estimate_family_us(std::uint64_t pixels,
                                           double ns_per_pixel) noexcept {
    if (pixels == 0 || !(ns_per_pixel > 0.0)) return 0;
    const double microseconds =
        static_cast<double>(pixels) * ns_per_pixel / 1000.0;
    if (!(microseconds < 1.8e16)) return static_cast<std::uint64_t>(-1);
    return static_cast<std::uint64_t>(microseconds + 0.5);
}

// Installs the counting shells over the kernels TVPGL_ASM_Init selected and
// probes their per-pixel cost on this device. Call once, after the kernel
// policy has run, so the shells wrap the implementation in use. The probe runs
// before the shells are installed, so it measures the bare kernels.
void install_tvpgl_pixel_meter();

// Costs measured by install_tvpgl_pixel_meter(); zero-filled until then.
const TvpgPixelFamilyCosts& tvpgl_pixel_costs();

// Returns the pixels and shell calls accumulated since the previous call and
// resets them. Both arrays hold kTvpgPixelFamilyCount entries.
void tvpgl_pixel_meter_take(std::uint64_t* pixels, std::uint64_t* calls);

} // namespace mofa

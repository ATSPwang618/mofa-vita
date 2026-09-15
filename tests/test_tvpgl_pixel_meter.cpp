#include "mofa/tvpgl_pixel_meter.hpp"

#include <cassert>
#include <cstdint>
#include <limits>

int main() {
    using mofa::estimate_family_us;

    // The families the frame report distinguishes; adding one is a deliberate
    // change because every report line and every A/B comparison depends on it.
    static_assert(mofa::kTvpgPixelFamilyCount == 7);
    static_assert(mofa::kTvpgPixelBlend == 0);
    static_assert(mofa::kTvpgPixelColorMap ==
                  mofa::kTvpgPixelFamilyCount - 1);

    // Nothing blended and an unmeasured family both report no time rather than
    // inventing one from a default cost.
    static_assert(estimate_family_us(0, 2.5) == 0);
    static_assert(estimate_family_us(4096, 0.0) == 0);
    static_assert(estimate_family_us(4096, -1.0) == 0);

    // ns/px -> us conversion with rounding to the nearest microsecond.
    static_assert(estimate_family_us(1'000'000, 2.5) == 2500);
    static_assert(estimate_family_us(589'824, 2.5) == 1475);
    static_assert(estimate_family_us(3, 1000.0) == 3);
    static_assert(estimate_family_us(1000, 1.0) == 1);

    // A nonsensical product saturates instead of wrapping into a small number
    // that would understate a frame's pixel cost.
    static_assert(estimate_family_us(std::numeric_limits<std::uint64_t>::max(),
                                     1000.0) ==
                  std::numeric_limits<std::uint64_t>::max());

    // The device-side accessors live in the Vita translation unit (they read
    // the installed kernels), so this test pins the arithmetic the report
    // depends on and leaves installation to the device build.
    return 0;
}

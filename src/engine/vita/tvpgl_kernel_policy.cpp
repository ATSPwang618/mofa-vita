#include "mofa/tvpgl_kernel_policy.hpp"

#include <psp2/io/stat.h>

namespace mofa {

bool scalar_tvpgl_kernels_requested() {
    // A marker file rather than a per-game profile key: the choice describes
    // the host the build runs on (emulator versus hardware), not the title.
    SceIoStat status{};
    return sceIoGetstat("ux0:data/mofa-vita/tvpgl-scalar", &status) >= 0;
}

} // namespace mofa

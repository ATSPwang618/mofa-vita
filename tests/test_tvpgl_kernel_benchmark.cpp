#include "mofa/tvpgl_kernel_benchmark.hpp"

#include <cassert>
#include <cstdint>

namespace {

using Kernel = void (*)(int*, int);

void scalar_kernel(int*, int) {}
void neon_kernel(int*, int) {}

} // namespace

int main() {
    using mofa::kTvpglKernelMinAdvantagePercent;
    using mofa::select_tvpgl_kernel;
    using mofa::tvpgl_kernel_candidate_wins;

    // A candidate has to be clearly faster: a tie, a marginal win and an
    // unmeasurable comparison all keep the generated scalar reference.
    static_assert(!tvpgl_kernel_candidate_wins(100, 100, 15));
    static_assert(!tvpgl_kernel_candidate_wins(100, 86, 15));
    static_assert(tvpgl_kernel_candidate_wins(100, 84, 15));
    static_assert(tvpgl_kernel_candidate_wins(100, 20, 15));
    static_assert(!tvpgl_kernel_candidate_wins(0, 20, 15));
    static_assert(!tvpgl_kernel_candidate_wins(100, 0, 15));
    static_assert(!tvpgl_kernel_candidate_wins(100, 1, 100));
    static_assert(tvpgl_kernel_candidate_wins(1000, 999, 0));

    // The rule is the one the device driver applies, so a losing candidate
    // must leave the scalar reference installed and say so in its result.
    Kernel installed = neon_kernel;
    const mofa::TvpglKernelChoice lost =
        select_tvpgl_kernel("TVPAlphaBlend", installed, scalar_kernel,
                            100, 900, kTvpglKernelMinAdvantagePercent);
    assert(installed == scalar_kernel);
    assert(!lost.candidate_selected);
    assert(lost.reference_us == 100 && lost.candidate_us == 900);
    assert(lost.name != nullptr);

    Kernel kept = neon_kernel;
    const mofa::TvpglKernelChoice won =
        select_tvpgl_kernel("TVPAlphaBlend_o", kept, scalar_kernel, 800, 120);
    assert(kept == neon_kernel);
    assert(won.candidate_selected);
    assert(won.reference_us == 800 && won.candidate_us == 120);

    // An unmeasurable comparison must not downgrade a working slot either:
    // it can only decide to keep what is already installed.
    Kernel unmeasured = scalar_kernel;
    const mofa::TvpglKernelChoice skipped =
        select_tvpgl_kernel("TVPStretchAlphaBlend", unmeasured, scalar_kernel,
                            0, 0);
    assert(unmeasured == scalar_kernel);
    assert(!skipped.candidate_selected);
    return 0;
}

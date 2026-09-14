#pragma once

namespace mofa {

// Yuri installs a table of hand-written NEON kernels (tvpgl_arm.cpp) whenever
// the CPU reports ARM + NEON.  They are the fast path on real hardware, but an
// emulator's ARM JIT may not implement every instruction they use: Vita3K's
// Dynarmic, for instance, falls back to its interpreter for the `vsubhn`
// sequences inside the blend kernels, which turns every blended pixel into a
// JIT-to-interpreter transition plus four log lines.
//
// When the user asks for it, keep the portable C++ kernels instead: slower on
// hardware, but translated by the emulator and therefore usable there.  The
// request is a marker file so it survives a reboot and stays independent of
// the game profile:
//
//     ux0:data/mofa-vita/tvpgl-scalar
//
bool scalar_tvpgl_kernels_requested();

} // namespace mofa

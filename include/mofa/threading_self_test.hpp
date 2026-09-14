#pragma once

// Exercises the exact libstdc++ synchronization path used throughout Yuri.
// Vita static links must include all of libpthread or libstdc++ can decide
// that pthreads are inactive and leave std::mutex storage uninitialized.
bool mofa_vita_threading_self_test();

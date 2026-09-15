#pragma once

// Exercises the exact libstdc++ synchronization path used throughout Yuri.
// Vita static links must include all of libpthread or libstdc++ can decide
// that pthreads are inactive and leave std::mutex storage uninitialized.
bool mofa_vita_threading_self_test();

// Which check failed during the last run, or nullptr when it passed.  Kept
// separate so the verified entry point keeps its exact signature; the
// emulator's scheduling is not a contract, the failure reason is.
const char* mofa_vita_threading_self_test_reason();

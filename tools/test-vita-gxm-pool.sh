#!/usr/bin/env bash
# Actual native C allocator with SDK call doubles; no Vita/GPU execution.
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
vita_sdk=${VITASDK:-/opt/vitasdk}
build="$project/.work/vita/gxm-pool"
mkdir -p "$build"
cc -std=c11 -Wall -Wextra -Werror -g -O1 -fsanitize=address,undefined \
    -fno-omit-frame-pointer -ffunction-sections -fdata-sections \
    -c "$project/knife/vita/perf_audit.c" -o "$build/perf.o"
cc -std=c11 -Wall -Wextra -Werror -g -O1 -fsanitize=address,undefined \
    -fno-omit-frame-pointer -ffunction-sections -fdata-sections -Wl,--gc-sections \
    -idirafter "$vita_sdk/arm-vita-eabi/include" \
    "$project/knife/vita/tests/gxm_pool_test.c" "$build/perf.o" -lm -o "$build/test"
ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 "$build/test"

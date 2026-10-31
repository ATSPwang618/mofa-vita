#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$HERE/../../.." && pwd)
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/knife-vita-movie-agent}"
NETWORK_ARGS=()
if [[ "${KNIFE_VITA_OFFLINE:-0}" = 1 ]]; then NETWORK_ARGS=(--offline); fi
LOG=${MOVIE_PROBE_TEST_LOG:-"$ROOT/doc/logs/vita/movie-probe-host-tests.log"}
mkdir -p -- "$(dirname -- "$LOG")"
set +e
cargo +1.99.0 test --manifest-path "$HERE/Cargo.toml" ${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"} 2>&1 | tee "$LOG"
RESULT=${PIPESTATUS[0]}
set -e
printf 'ACTUAL_CARGO_EXIT=%s\n' "$RESULT" | tee -a "$LOG"
exit "$RESULT"

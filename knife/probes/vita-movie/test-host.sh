#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$HERE/../../.." && pwd)
export PATH=/opt/vitasdk/bin:/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/.cache/knife-vita-movie-agent}
LOG=${MOVIE_PROBE_TEST_LOG:-"$ROOT/doc/logs/vita/movie-probe-host-tests.log"}
mkdir -p -- "$(dirname -- "$LOG")"
set +e
cargo +1.99.0 test --manifest-path "$HERE/Cargo.toml" --offline 2>&1 | tee "$LOG"
RESULT=${PIPESTATUS[0]}
set -e
printf 'ACTUAL_CARGO_EXIT=%s\n' "$RESULT" | tee -a "$LOG"
exit "$RESULT"

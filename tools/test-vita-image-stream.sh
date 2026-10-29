#!/usr/bin/env bash
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
export PATH=/root/.cargo/bin:/usr/bin:/bin
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/.cache/knife-vita}
build="$project/.work/vita/image-stream-tests"
mkdir -p "$build" "$project/doc/logs/vita"
cd "$project/knife"
cargo +nightly-2026-05-28 build -p knife-core --offline --locked
deps="$CARGO_TARGET_DIR/debug/deps"
core=$(find "$deps" -maxdepth 1 -name 'libknife_core-*.rlib' -print -quit)
anyhow=$(find "$deps" -maxdepth 1 -name 'libanyhow-*.rlib' -print -quit)
cat > "$build/motion.rs" <<EOF
#![allow(dead_code)]
#[path = "$project/knife/knife/src/motion_loops.rs"] mod motion_loops;
#[path = "$project/knife/knife/src/motion.rs"] mod motion;
EOF
{
    rustc +nightly-2026-05-28 --edition=2024 --test "$build/motion.rs" \
        -L "dependency=$deps" --extern "knife_core=$core" --extern "anyhow=$anyhow" \
        -o "$build/motion-tests"
    "$build/motion-tests"
    rustc +nightly-2026-05-28 --edition=2024 --test \
        "$project/knife/knife-platform-vita/src/image_worker.rs" -o "$build/worker-tests"
    "$build/worker-tests"
    rustc +nightly-2026-05-28 --edition=2024 --test \
        "$project/knife/knife-platform-vita/tests/frame_timing.rs" -o "$build/frame-timing-tests"
    "$build/frame-timing-tests"
} 2>&1 | tee "$project/doc/logs/vita/image-stream-host.log"

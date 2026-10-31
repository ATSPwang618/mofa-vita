#!/usr/bin/env bash
set -euo pipefail
task_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
test_dir="${XDG_CACHE_HOME:-$HOME/.cache}/knife-vita-glyph-pages"
mkdir -p -- "$test_dir"
cat > "$test_dir/harness.rs" <<EOF
#[path = "$task_root/knife/knife/src/text/atlas.rs"]
mod atlas;
EOF
rustc +nightly-2026-05-28 --edition=2024 --test "$test_dir/harness.rs" -o "$test_dir/glyph-pages"
"$test_dir/glyph-pages"

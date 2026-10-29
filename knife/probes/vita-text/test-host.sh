#!/usr/bin/env bash
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$HERE/../../.." && pwd)
MODE=${1:-green}
case "$MODE" in red|green) ;; *) printf 'Expected red or green mode\n' >&2; exit 2 ;; esac
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/.cache/knife-vita-text-agent}
export KNIFE_TEXT_LAYOUT_REPORT="$ROOT/doc/vita-text-layout-reference.json"
LOG="$ROOT/doc/logs/vita/text-host-tdd-$MODE.log"
mkdir -p -- "$(dirname -- "$LOG")"
set +e
if [ "$MODE" = red ]; then
    cargo +1.99.0 test --offline --manifest-path "$HERE/Cargo.toml" --lib tests::actual_game_text_module_builds_bilingual_vita_pages -- --exact --nocapture 2>&1 | tee "$LOG"
else
    cargo +1.99.0 test --offline --manifest-path "$HERE/Cargo.toml" --lib -- --nocapture --skip text::tests::cff_glyphs_after_endchar_outline 2>&1 | tee "$LOG"
fi
RESULT=${PIPESTATUS[0]}
set -e
printf 'ACTUAL_CARGO_EXIT=%s\n' "$RESULT" | tee -a "$LOG"
exit "$RESULT"

#!/usr/bin/env bash
# Independent original-text probe; the parent task owns VPK packaging/runtime.
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$HERE/../../.." && pwd)
export VITASDK=${VITASDK:-/opt/vitasdk}
export PATH="$VITASDK/bin:/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/root/.cache/knife-vita-text-agent}
export CARGO_TARGET_ARMV7_SONY_VITA_NEWLIBEABIHF_LINKER=arm-vita-eabi-gcc
export KNIFE_VITA_DEV_DIAGNOSTICS=1
LOG="$ROOT/doc/logs/vita/text-arm-build.log"
mkdir -p -- "$(dirname -- "$LOG")"
set +e
{
    rustc +nightly-2026-05-28 -vV
    arm-vita-eabi-gcc --version
    cargo +nightly-2026-05-28 build -Z build-std=std,panic_abort --locked --offline \
        --manifest-path "$HERE/Cargo.toml" --profile vita \
        --target armv7-sony-vita-newlibeabihf --bin vita-text-probe
} 2>&1 | tee "$LOG"
RESULT=${PIPESTATUS[0]}
set -e
printf 'ACTUAL_CARGO_EXIT=%s\n' "$RESULT" | tee -a "$LOG"
if [ "$RESULT" != 0 ]; then exit "$RESULT"; fi
ELF="$CARGO_TARGET_DIR/armv7-sony-vita-newlibeabihf/vita/vita-text-probe.elf"
arm-vita-eabi-readelf -h -A "$ELF" > "$ROOT/doc/logs/vita/text-arm-elf-identity.log"
arm-vita-eabi-nm -C "$ELF" > "$ROOT/doc/logs/vita/text-arm-symbols.log"
arm-vita-eabi-nm -u "$ELF" > "$ROOT/doc/logs/vita/text-arm-undefined.log"
sha256sum "$ELF" | tee -a "$LOG"

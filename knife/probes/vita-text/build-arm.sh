#!/usr/bin/env bash
# Independent original-text probe; the parent task owns VPK packaging/runtime.
set -euo pipefail
HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$HERE/../../.." && pwd)
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
if [[ -z "${VITASDK:-}" ]]; then
    # Reuse the production launcher's SDK discovery without installing/building.
    VITASDK=$(python3 - "$ROOT/tools/build-vita.py" <<'SDK_PY'
import runpy, sys
launcher = runpy.run_path(sys.argv[1])
for candidate in launcher["sdk_candidates"](None):
    if not launcher["inspect_sdk"](candidate)["missing"]:
        print(candidate)
        break
else:
    raise SystemExit("No compatible VitaSDK found; set VITASDK or add its GCC to PATH.")
SDK_PY
    )
fi
export VITASDK
export PATH="$VITASDK/bin:$PATH"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/knife-vita-text-agent}"
NETWORK_ARGS=()
if [[ "${KNIFE_VITA_OFFLINE:-0}" = 1 ]]; then NETWORK_ARGS=(--offline); fi
export CARGO_TARGET_ARMV7_SONY_VITA_NEWLIBEABIHF_LINKER=arm-vita-eabi-gcc
export KNIFE_VITA_DEV_DIAGNOSTICS=1
LOG="$ROOT/doc/logs/vita/text-arm-build.log"
mkdir -p -- "$(dirname -- "$LOG")"
set +e
{
    rustc +nightly-2026-05-28 -vV
    arm-vita-eabi-gcc --version
    cargo +nightly-2026-05-28 build -Z build-std=std,panic_abort --locked ${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"} \
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
if command -v sha256sum >/dev/null; then sha256sum "$ELF"; else shasum -a 256 "$ELF"; fi | tee -a "$LOG"

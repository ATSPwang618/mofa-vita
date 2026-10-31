#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd -- "$here/../../.." && pwd)
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
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/knife-vita-audio-mixer}"
NETWORK_ARGS=()
if [[ "${KNIFE_VITA_OFFLINE:-0}" = 1 ]]; then NETWORK_ARGS=(--offline); fi
export CARGO_TARGET_ARMV7_SONY_VITA_NEWLIBEABIHF_LINKER=arm-vita-eabi-gcc
cargo +nightly-2026-05-28 build -Z build-std=std,panic_abort --locked ${NETWORK_ARGS[@]+"${NETWORK_ARGS[@]}"} \
  --manifest-path "$here/Cargo.toml" --profile vita \
  --target armv7-sony-vita-newlibeabihf --bin vita-audio-mixer-probe
elf="$CARGO_TARGET_DIR/armv7-sony-vita-newlibeabihf/vita/vita-audio-mixer-probe.elf"
arm-vita-eabi-readelf -h "$elf" | head -n 18
if command -v sha256sum >/dev/null; then sha256sum "$elf"; else shasum -a 256 "$elf"; fi

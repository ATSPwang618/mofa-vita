#!/usr/bin/env bash
set -euo pipefail
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
export VITASDK=${VITASDK:-/opt/vitasdk}
export PATH="$VITASDK/bin:/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$here/target}
export CARGO_TARGET_ARMV7_SONY_VITA_NEWLIBEABIHF_LINKER=arm-vita-eabi-gcc
cargo +nightly-2026-05-28 build -Z build-std=std,panic_abort --locked --offline \
  --manifest-path "$here/Cargo.toml" --profile vita \
  --target armv7-sony-vita-newlibeabihf --bin vita-audio-mixer-probe
elf="$CARGO_TARGET_DIR/armv7-sony-vita-newlibeabihf/vita/vita-audio-mixer-probe.elf"
arm-vita-eabi-readelf -h "$elf" | head -n 18
sha256sum "$elf"

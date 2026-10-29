#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
bin="$(mktemp)"
lib="$(mktemp)"
trap 'rm -f -- "$bin" "$lib"' EXIT
rustc +nightly-2026-05-28 --edition=2021 --crate-type=lib knife/knife-platform-vita/src/gxt.rs -o "$lib"
rustc +nightly-2026-05-28 --edition=2021 --test knife/knife-platform-vita/src/gxt.rs -o "$bin"
"$bin" --nocapture

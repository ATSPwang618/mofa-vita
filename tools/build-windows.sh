#!/usr/bin/env bash
# Run in Linux/WSL: bash tools/build-windows.sh [--debug-tools|--kdu]
set -euo pipefail

TASK_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_CACHE="${KNIFE_BUILD_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/knife-windows-x64}"
BUILD_JOBS="${KNIFE_BUILD_JOBS:-3}"
BUILD_NAME=knife
BUILD_PACKAGE=knife
BUILD_FEATURES=()
if [[ "${1:-}" == --debug-tools ]]; then
    BUILD_NAME=knife-qa
    BUILD_FEATURES=(--features debug)
elif [[ "${1:-}" == --kdu ]]; then
    BUILD_NAME=kdu
    BUILD_PACKAGE=kdu
elif [[ -n "${1:-}" ]]; then
    echo "Usage: $0 [--debug-tools|--kdu]" >&2
    exit 2
fi

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="$BUILD_CACHE"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
export CMAKE_GENERATOR=Ninja
DIST_OUTPUT="${KNIFE_DIST_DIR:-$TASK_ROOT/dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64}"
BUILD_LABEL="${KNIFE_BUILD_LABEL:-compact-$BUILD_NAME}"
mkdir -p "$TASK_ROOT/doc/logs" "$DIST_OUTPUT"
cd "$TASK_ROOT/knife"

{
    date -Iseconds
    if [[ -e .git ]]; then
        git rev-parse HEAD
        # This source checkout was created by Windows Git with CRLF conversion.
        git -c core.autocrlf=true -c core.safecrlf=false diff --stat
    else
        cat UPSTREAM.txt
    fi
    rustc +1.99.0 -Vv
    cargo +1.99.0 -V
    x86_64-w64-mingw32-gcc --version
    cmake --version
    ninja --version
    cargo +1.99.0 build --locked --release -p "$BUILD_PACKAGE" \
        --target x86_64-pc-windows-gnu -j "$BUILD_JOBS" "${BUILD_FEATURES[@]}"
} 2>&1 | tee "$TASK_ROOT/doc/logs/build-$BUILD_LABEL.log"

BUILD_OUTPUT="$CARGO_TARGET_DIR/x86_64-pc-windows-gnu/release/$BUILD_PACKAGE.exe"
if [[ "$BUILD_NAME" == knife-qa ]]; then
    mkdir -p "$TASK_ROOT/.work/qa"
    cp -- "$BUILD_OUTPUT" "${KNIFE_QA_PATH:-$TASK_ROOT/.work/qa/knife-540p-qa.exe}"
elif [[ "$BUILD_NAME" == kdu ]]; then
    cp -- "$BUILD_OUTPUT" "${KNIFE_KDU_PATH:-$TASK_ROOT/tools/kdu.exe}"
else
    cp -- "$BUILD_OUTPUT" "$DIST_OUTPUT/knife.exe"
fi
{
    x86_64-w64-mingw32-objdump -f "$BUILD_OUTPUT"
    x86_64-w64-mingw32-objdump -p "$BUILD_OUTPUT" | \
        awk '/The Export Tables/ || /The Function Table/ { stop=1 } !stop { print }'
} > "$TASK_ROOT/doc/logs/pe-$BUILD_LABEL.txt"
sha256sum "$BUILD_OUTPUT"

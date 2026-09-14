#!/usr/bin/env bash
set -euo pipefail

: "${VITASDK:?Set VITASDK to the VitaSDK installation directory}"
: "${JOBS:=12}"
export VITASDK

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
host_build="${MOFA_HOST_BUILD:-$repo_root/build-release-host}"
vita_build="${MOFA_VITA_BUILD:-$repo_root/build-release-vita}"

host_cmake_args=(
  -S "$repo_root"
  -B "$host_build"
  -DCMAKE_BUILD_TYPE=Release
  -DMOFA_BUILD_TESTS=ON
)
if [[ -n ${MOFA_RETAIL_TEST_GAME:-} ]]; then
  host_cmake_args+=(
    -DMOFA_RETAIL_TEST_GAME="$MOFA_RETAIL_TEST_GAME"
  )
fi
if [[ -n ${MOFA_RETAIL_TEST_PATCH_SNAPSHOT:-} ]]; then
  host_cmake_args+=(
    -DMOFA_RETAIL_TEST_PATCH_SNAPSHOT="$MOFA_RETAIL_TEST_PATCH_SNAPSHOT"
  )
fi
if [[ -n ${MOFA_RETAIL_TEST_FILTER:-} ]]; then
  host_cmake_args+=(
    -DMOFA_RETAIL_TEST_FILTER="$MOFA_RETAIL_TEST_FILTER"
  )
fi

cmake "${host_cmake_args[@]}"
cmake --build "$host_build" --parallel "$JOBS"
ctest --test-dir "$host_build" --output-on-failure --parallel 1

"$repo_root/scripts/build-vita-ffmpeg.sh"

cmake -S "$repo_root" -B "$vita_build" \
  -DCMAKE_TOOLCHAIN_FILE="$VITASDK/share/vita.toolchain.cmake" \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build "$vita_build" \
  --target mofa-vita-krkr.vpk-vpk --parallel "$JOBS"

sha256sum "$vita_build/mofa-vita-krkr.vpk"
echo "Validated VPK: $vita_build/mofa-vita-krkr.vpk"

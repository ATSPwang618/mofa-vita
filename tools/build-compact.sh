#!/usr/bin/env bash
# Reuse the WSL toolchain and build only the independent compact output.
set -euo pipefail
TASK_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
export KNIFE_DIST_DIR="$TASK_ROOT/dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64"
export KNIFE_QA_PATH="$TASK_ROOT/.work/qa/knife-compact-qa.exe"
export KNIFE_KDU_PATH="$TASK_ROOT/tools/kdu-compact.exe"
case "${1:-}" in
    '') export KNIFE_BUILD_LABEL=compact-production ;;
    --debug-tools) export KNIFE_BUILD_LABEL=compact-qa ;;
    --kdu) export KNIFE_BUILD_LABEL=compact-kdu ;;
    *) echo "Usage: $0 [--debug-tools|--kdu]" >&2; exit 2 ;;
esac
bash "$TASK_ROOT/tools/build-windows.sh" "$@"

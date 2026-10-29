#!/usr/bin/env bash
# Compile upstream tests for Windows, then execute those PE files on Windows.
set -euo pipefail
TASK_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_CACHE="${KNIFE_BUILD_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/knife-windows-x64}"
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR="$BUILD_CACHE"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
export CC_x86_64_pc_windows_gnu=x86_64-w64-mingw32-gcc
export CXX_x86_64_pc_windows_gnu=x86_64-w64-mingw32-g++
export AR_x86_64_pc_windows_gnu=x86_64-w64-mingw32-ar
export CMAKE_GENERATOR=Ninja
TEST_DIR="$TASK_ROOT/.work/qa/tests"
mkdir -p "$TEST_DIR" "$TASK_ROOT/doc/logs"
cd "$TASK_ROOT/knife"
cargo +1.99.0 test --locked --release --workspace --target x86_64-pc-windows-gnu \
    --no-run -j "${KNIFE_BUILD_JOBS:-3}" --message-format=json \
    > "$TASK_ROOT/doc/logs/test-artifacts.jsonl" \
    2> "$TASK_ROOT/doc/logs/build-tests.log"
python3 - "$TASK_ROOT" <<'PY'
import json, shutil, sys
from pathlib import Path
root = Path(sys.argv[1])
tests = []
for line in (root/'doc/logs/test-artifacts.jsonl').read_text().splitlines():
    item = json.loads(line)
    if item.get('reason') == 'compiler-artifact' and item.get('profile', {}).get('test') and item.get('executable'):
        src = Path(item['executable'])
        dst = root/'.work/qa/tests'/src.name
        shutil.copy2(src, dst)
        tests.append({'target': item['target']['name'], 'exe': dst.name})
(root/'.work/qa/tests/manifest.json').write_text(json.dumps(tests, indent=2)+'\n')
print(f'Windows test executables prepared: {len(tests)}')
PY

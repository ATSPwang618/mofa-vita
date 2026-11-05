#!/usr/bin/env bash
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
vita_sdk=${VITASDK:-/opt/vitasdk}
host_cc=${CC:-cc}
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
build="$project/.work/vita/runtime-shader-cache/$run_id"
mkdir -p "$build/backend/tests" "$build/backend/shaders/generated"
for source in runtime_shaders.c shader_sha256.h shader_module_exports.h shader_extensions.h diagnostic_log.h shaders/generated/runtime_source.h tests/runtime_shader_cache_stubs.h; do
    cp "$project/knife/vita/$source" "$build/backend/$source"
done
cp "$project/knife/vita/tests/runtime_shader_cache_test.c" "$build/test.c"
cp "${BASH_SOURCE[0]}" "$build/runner.sh"
command=("$host_cc" -std=c11 -D_DEFAULT_SOURCE -Wall -Wextra -Werror -Wno-unused-function -g -O2 -fstrict-aliasing
    -fsanitize=address,undefined -fno-omit-frame-pointer
    -idirafter "$vita_sdk/arm-vita-eabi/include"
    "-DKV_RUNTIME_SHADER_SOURCE=\"$build/backend/runtime_shaders.c\""
    "-DKV_SHADER_CACHE_ROOT=\"$build/cache\""
    -MD -MF "$build/test.d" "$build/test.c" -o "$build/test")
printf '%q ' "${command[@]}" > "$build/command.txt"
printf '\n' >> "$build/command.txt"
"${command[@]}"
set +e
ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 "$build/test" > "$build/output.log" 2>&1
test_status=$?
set -e
cat "$build/output.log"
python3 - "$build" "$test_status" "$host_cc" <<'PY'
from pathlib import Path
from datetime import datetime, timezone
import hashlib,json,re,subprocess,sys
build=Path(sys.argv[1]); status=int(sys.argv[2])
match=re.search(r'^RESULT (PASS|FAIL): (\d+) cases, (\d+) checks, (\d+) failures$',(build/'output.log').read_text(),re.M)
if not match: raise SystemExit('Test did not reach its result marker')
def artifact(p):
    b=p.read_bytes();return dict(path=str(p),bytes=len(b),sha256=hashlib.sha256(b).hexdigest())
receipt=dict(schema='knife-vita-runtime-shader-cache-host-v1',tested_at=datetime.now(timezone.utc).isoformat(),
    result=match[1],cases=int(match[2]),checks=int(match[3]),failures=int(match[4]),exit_code=status,
    compiler=subprocess.check_output([sys.argv[3],'--version'],text=True).splitlines()[0],
    command=(build/'command.txt').read_text().strip(),
    limitations=['Real production metadata/cache routines with real SDK declarations and a synthetic non-executable GXP fixture.',
                 'Host ProgramCheck returns a controlled status; no Cg compiler, ARM, GPU, rendered-pixel, or performance claim.'],
    files=[artifact(p) for p in sorted(build.rglob('*')) if p.is_file()])
(build/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print('RECEIPT',build/'receipt.json')
PY
exit "$test_status"

#!/usr/bin/env bash
# Compile and exercise actual scene setup on the host; no SDK runtime/GPU.
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
vita_sdk=${VITASDK:-/opt/vitasdk}
host_cc=${CC:-cc}
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
build="$project/.work/vita/gxm-viewport/$run_id"
mkdir -p "$build/backend/shaders/generated" "$project/doc/logs/vita"
for source in perf_audit.c perf_audit.h perf_counters.def gxm_backend.c gxm_backend.h jpeg_hw.h image_decode.h effects_backend.inc gpu_lifetime.inc movie_gxm.inc shaders/generated/shaders.h shaders/generated/effects.h; do
    cp "$project/knife/vita/$source" "$build/backend/$source"
done
cp "$project/knife/vita/tests/gxm_viewport_test.c" "$build/test.c"
cp "${BASH_SOURCE[0]}" "$build/runner.sh"
"$host_cc" -std=c11 -Wall -Wextra -Werror -g -O1 -fsanitize=address,undefined \
    -fno-omit-frame-pointer -ffunction-sections -fdata-sections \
    -c "$build/backend/perf_audit.c" -o "$build/perf_audit.o"
command=("$host_cc" -std=c11 -Wall -Wextra -Werror -g -O2 -fstrict-aliasing
    -fsanitize=address,undefined -fno-omit-frame-pointer
    -ffunction-sections -fdata-sections -Wl,--gc-sections
    -idirafter "$vita_sdk/arm-vita-eabi/include"
    "-DKV_GXM_BACKEND_SOURCE=\"$build/backend/gxm_backend.c\""
    -MD -MF "$build/test.d" "$build/test.c" "$build/perf_audit.o" -lm -o "$build/test")
printf '%q ' "${command[@]}" > "$build/command.txt"
printf '\n' >> "$build/command.txt"
"${command[@]}"
set +e
ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 "$build/test" > "$build/output.log" 2>&1
test_status=$?
set -e
cat "$build/output.log"
python3 - "$project" "$build" "$test_status" "$host_cc" <<'PY'
from pathlib import Path
from datetime import datetime, timezone
import hashlib, json, re, subprocess, sys
project, build = map(Path, sys.argv[1:3])
status = int(sys.argv[3])
text = (build / 'output.log').read_text()
match = re.search(r'^RESULT (PASS|FAIL): (\d+) cases, (\d+) checks, (\d+) failures$', text, re.M)
if not match:
    raise SystemExit('Test did not reach the result marker')
def artifact(path):
    data = path.read_bytes()
    return dict(path=str(path), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
receipt = dict(schema='knife-vita-gxm-viewport-host-v1', tested_at=datetime.now(timezone.utc).isoformat(),
    result=match[1], cases=int(match[2]), checks=int(match[3]), failures=int(match[4]), exit_code=status,
    build_directory=str(build), compiler=subprocess.check_output([sys.argv[4], '--version'], text=True).splitlines()[0],
    command=(build / 'command.txt').read_text().strip(),
    coverage=['Production effect_scene arguments: NDC corners span each of eight legal target sizes, including odd and minimum dimensions.',
              'BeginScene failure preserves closed scene and vertex arena, with no state setter calls.'],
    limitations=['Host test with real SDK declarations and mocked SDK scene/state calls; no GPU, SDK library, Vita3K or hardware pixel claim.',
                 'Uncalled backend functions are discarded by section garbage collection.'],
    files=[artifact(p) for p in sorted(build.rglob('*')) if p.is_file()])
for destination in [build / 'receipt.json', project / 'doc/vita-gxm-viewport-host-tests.json']:
    destination.write_text(json.dumps(receipt, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
print('RECEIPT', build / 'receipt.json')
PY
exit "$test_status"

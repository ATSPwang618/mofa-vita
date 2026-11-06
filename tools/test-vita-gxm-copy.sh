#!/usr/bin/env bash
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
vita_sdk=${VITASDK:-/opt/vitasdk}
host_cc=${CC:-cc}
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
build="$project/.work/vita/gxm-copy/$run_id"
mkdir -p "$build/backend/shaders/generated"
for source in perf_audit.c perf_audit.h perf_counters.def gxm_backend.c gxm_backend.h runtime_shaders.h jpeg_hw.h image_decode.h effects_backend.inc gpu_lifetime.inc movie_gxm.inc shaders/generated/shaders.h shaders/generated/effects.h; do
    cp "$project/knife/vita/$source" "$build/backend/$source"
done
cp "$project/knife/vita/tests/gxm_copy_test.c" "$build/test.c"
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
match = re.search(r'^RESULT (PASS|FAIL): (\d+) cases, (\d+) checks, (\d+) failures$', (build/'output.log').read_text(), re.M)
if not match: raise SystemExit('Test did not reach its result marker')
# Parse the real emitted JSON: enabled bits alone must never be called ready,
# and every bounded summary must remain under the console's admission limit.
records=[json.loads(line) for line in (build/'output.log').read_text().splitlines() if line.startswith('{')]
capabilities=[r for r in records if r.get('kind')=='shader_capabilities']
ready_keys=('full1_ready','full6_ready','full13_ready')
if not capabilities or any(any(key not in r for key in ready_keys) for r in capabilities):
    raise SystemExit('Missing actual full-material readiness in a startup capability summary')
states=[tuple(r[key] for key in ready_keys) for r in capabilities]
if (0,0,0) not in states or (1,0,1) not in states or (1,1,1) not in states:
    raise SystemExit(f'Missing absent/rejected/independently-patched readiness states: {states}')
if states[-2:]!=[(0,0,0),(0,0,0)]:
    raise SystemExit('Enabled-but-uncompiled or registration-rejected candidates reported ready')
routes=[r for r in records if r.get('kind')=='shader_fallback_routes']
if len(routes)!=len(capabilities): raise SystemExit('Startup fallback routes missing from a capability summary')
for route in routes:
    if not all(value in route.get('quad_fallback_routes','') for value in
            ('op1:30/64/54/17/14','op6:31/69/55/17/14','op13:21/76/56/17/14')):
        raise SystemExit('Quad fallback order does not preserve plain/full/generic/snapshot priority')
    if not all(value in route.get('group_fallback_routes','') for value in
            ('op1:64/54/17/14','op6:69/55/17/14','op13:76/56/17/14')):
        raise SystemExit('Group fallback order does not preserve full/generic/snapshot priority')
    if route.get('selection')!='first_ready_and_source_compatible;14=destination_snapshot':
        raise SystemExit('Fallback route log must describe readiness/source compatibility and snapshot route')
for line in (build/'output.log').read_text().splitlines():
    if line.startswith('{') and len((line+'\n').encode())>900:
        raise SystemExit('JSON log exceeds the native console admission limit')
def artifact(path):
    data=path.read_bytes()
    return dict(path=str(path),bytes=len(data),sha256=hashlib.sha256(data).hexdigest())
receipt=dict(schema='knife-vita-gxm-copy-host-v1',tested_at=datetime.now(timezone.utc).isoformat(),
    result=match[1],cases=int(match[2]),checks=int(match[3]),failures=int(match[4]),exit_code=status,
    build_directory=str(build),compiler=subprocess.check_output([sys.argv[4],'--version'],text=True).splitlines()[0],
    command=(build/'command.txt').read_text().strip(),
    coverage=['RT source copies submit an opaque full RGBA textured quad to a matching target, without CPU transfer.',
              'Encoded native RGBA8 uses 32-bit/UCHAR4 for offscreen and display; gamma source/target requests are rejected.',
              'All 17 legacy effects x 10 blends x one UCHAR4 output mode, destination switches, cache reuse and release; dedicated text CoverageOver keeps alpha ONE/ONE_MINUS_SRC_ALPHA while game Alpha/Over retain their original equations.',
              'Production 480x270 native target keeps RGBA8 at 512 KiB aligned storage; sRGB requests reject before allocation.',
              'Lazy target setup/reuse, odd/minimum/maximum dimensions and five reachable failing SDK stages.',
              '96 UI targets create no offscreen sync objects even when the driver rejects further sync allocation.',
              'Completed baked textures release RT attachments after Finish/TransferFinish, preserving pixel storage, descriptor and GPU copy routing.',
              'Attachment teardown failures retain a closing owner; active, foreign, movie and compressed textures are rejected.',
              'A draw failure still ends its scene; an EndScene failure retains a closing owner.',
              'CPU RGBA8 and R8 data retain their pitched transfer path.'],
    limitations=['Real backend compiled with SDK declarations and mocked SDK calls; pixels and alpha require the independent runtime oracle.',
                 'Uncalled backend functions are discarded by linker section garbage collection.'],
    files=[artifact(p) for p in sorted(build.rglob('*')) if p.is_file()])
destinations = [build/'receipt.json']
import os
if os.environ.get('KV_TEST_WORK_ONLY') != '1': destinations.append(project/'doc/vita-gxm-copy-host-tests.json')
for path in destinations:
    path.write_text(json.dumps(receipt,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
print('RECEIPT',build/'receipt.json')
PY
exit "$test_status"

#!/usr/bin/env bash
# Host-only compressed upload boundary tests. Never executes Vita or a GPU.
set -euo pipefail
mode=${1:---green}
if [[ $# -gt 1 || ( $mode != --green && $mode != --red-only ) ]]; then
    printf '%s\n' 'Usage: test-vita-compressed.sh [--red-only|--green]' >&2; exit 2
fi
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
vita_sdk=${VITASDK:-/opt/vitasdk}
host_cc=${CC:-cc}
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
build="$project/.work/vita/gxm-compressed/$run_id"
mkdir -p "$build/backend"

# Resolve and copy the quoted local include tree, including movie/effects
# implementation fragments. Both variants compile one immutable snapshot.
python3 - "$project" "$build" <<'PY'
from pathlib import Path
import re, shutil, sys
project, build = map(Path, sys.argv[1:])
root = project / 'knife/vita'
pending = [root / 'gxm_backend.c', root / 'gxm_backend.h', root / 'perf_audit.c']
seen = set()
while pending:
    source = pending.pop()
    if source in seen: continue
    seen.add(source)
    destination = build / 'backend' / source.relative_to(root)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)
    for include in re.findall(r'^\s*#\s*include\s*"([^"]+)"', source.read_text(), re.M):
        child = source.parent / include
        child.resolve().relative_to(root.resolve())
        if not child.is_file(): raise SystemExit(f'Missing local include: {child}')
        pending.append(child)
shutil.copyfile(root / 'tests/gxm_compressed_test.c', build / 'gxm_compressed_test.c')
shutil.copyfile(project / 'tools/test-vita-compressed.sh', build / 'test-vita-compressed.sh')
original = (build / 'backend/gxm_backend.c').read_text()
# RED is an explicit unsupported implementation in a private source copy;
# shared files and any concurrent production build are never modified.
# Rename the production symbol while preprocessing the complete include tree.
# Its implementation may live in an .inc file; leave those immutable bytes
# intact and let --gc-sections discard the uncalled renamed implementation.
red = '#define kv_gxm_texture_create_compressed kv_gxm_texture_create_compressed_red_discarded\n' + original
if not re.search(r'\bKV_TEXTURE_COMPRESSED\s*=', red):
    red = red.replace('struct KvGxmTexture {', 'enum { KV_TEXTURE_LINEAR=0, KV_TEXTURE_MOVIE=1, KV_TEXTURE_COMPRESSED=2 };\nstruct KvGxmTexture {', 1)
if not re.search(r'\bstorage\s*;', red[:red.index('typedef struct DisplayData')]):
    red = red.replace('struct KvGxmTexture {', 'struct KvGxmTexture {\n    uint32_t storage;', 1)
red += '''\n#undef kv_gxm_texture_create_compressed
int32_t kv_gxm_texture_create_compressed(KvGxm *g, const uint8_t *data,
    uint32_t length, uint32_t width, uint32_t height, uint32_t format,
    uint32_t srgb, uint32_t linear, KvGxmTexture **out) {
    (void)g; (void)data; (void)length; (void)width; (void)height;
    (void)format; (void)srgb; (void)linear;
    if (out) *out = NULL;
    return KV_BAD_ARGUMENT;
}\n'''
(build / 'backend/gxm_backend.red.c').write_text(red)
PY

{
    printf 'HOST-ONLY compressed texture boundary: mode=%s snapshot=%s\n' "$mode" "$build"
    "$host_cc" --version | head -n 1
    test -f "$vita_sdk/arm-vita-eabi/include/psp2/gxm.h"
    flags=(-std=c11 -Wall -Wextra -Werror -g -O1 -fsanitize=address,undefined
        -fno-omit-frame-pointer -ffunction-sections -fdata-sections -Wl,--gc-sections
        -idirafter "$vita_sdk/arm-vita-eabi/include")
    variants=(red)
    if [[ $mode == --green ]]; then variants+=(green); fi
    "$host_cc" -std=c11 -Wall -Wextra -Werror -g -O1 -fsanitize=address,undefined \
        -fno-omit-frame-pointer -ffunction-sections -fdata-sections \
        -c "$build/backend/perf_audit.c" -o "$build/perf_audit.o"
    for variant in "${variants[@]}"; do
        backend="$build/backend/gxm_backend.c"
        [[ $variant != red ]] || backend="$build/backend/gxm_backend.red.c"
        command=("$host_cc" "${flags[@]}" "-DKV_GXM_BACKEND_SOURCE=\"$backend\""
            -MD -MF "$build/$variant.d" "$build/gxm_compressed_test.c" "$build/perf_audit.o"
            "-Wl,-Map,$build/$variant.map" -o "$build/compressed-$variant")
        printf '%q ' "${command[@]}" > "$build/$variant.command"
        printf '\n' >> "$build/$variant.command"
        cat "$build/$variant.command"
        "${command[@]}"
        set +e
        ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
            "$build/compressed-$variant" > "$build/$variant.log" 2>&1
        status=$?
        set -e
        printf '%s\n' "$status" > "$build/$variant.exit"
        cat "$build/$variant.log"
        nm -u "$build/compressed-$variant" > "$build/$variant.undefined-symbols.txt"
        if grep -q 'sce[A-Z]' "$build/$variant.undefined-symbols.txt"; then
            printf '%s\n' 'ERROR: SDK imports escaped the host stubs.'; exit 1
        fi
        ldd "$build/compressed-$variant" > "$build/$variant.linked-libraries.txt"
        if [[ $variant == red ]]; then
            if [[ $status != 1 ]] || ! grep -q 'valid compressed upload must succeed' "$build/red.log" || \
                ! grep -q '^RESULT FAIL:' "$build/red.log"; then
                printf 'ERROR: unsupported RED did not fail the valid-upload assertion (exit=%s).\n' "$status"; exit 1
            fi
            printf '%s\n' 'RED VERIFIED: valid compressed uploads fail the unsupported implementation.'
        elif [[ $status != 0 ]] || ! grep -q '^RESULT PASS:' "$build/green.log"; then
            printf 'ERROR: production compressed tests failed (exit=%s).\n' "$status"; exit 1
        fi
    done
    python3 - "$build" "$mode" "$vita_sdk" <<'PY'
from datetime import datetime, timezone
from pathlib import Path
import hashlib, json, re, shlex, sys
build, mode, sdk = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
def identity(path):
    data = path.read_bytes()
    return {'path': str(path), 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
variants = ['red'] + (['green'] if mode == '--green' else [])
results, headers = {}, set()
for variant in variants:
    text = (build / f'{variant}.log').read_text()
    match = re.search(r'^RESULT (PASS|FAIL): (\d+) cases, (\d+) checks, (\d+) failures;', text, re.M)
    if not match: raise SystemExit(f'Missing summary for {variant}')
    results[variant] = dict(result=match[1], cases=int(match[2]), checks=int(match[3]),
        failures=int(match[4]), exit_code=int((build/f'{variant}.exit').read_text()))
    dependencies = shlex.split((build/f'{variant}.d').read_text().replace('\\\n', ' '))[1:]
    headers.update(Path(p) for p in dependencies if Path(p).is_relative_to(sdk))
if not any(p.name == 'gxm.h' for p in headers): raise SystemExit('Real SDK gxm.h was not compiled')
receipt = {'schema': 'knife-vita-compressed-host-v1',
    'created_utc': datetime.now(timezone.utc).isoformat(), 'mode': mode,
    'result': 'verified-red-only' if mode == '--red-only' else 'pass-host-red-and-green',
    'command': f'bash tools/test-vita-compressed.sh {mode}', 'build_directory': str(build),
    'results': results, 'sanitizers': ['AddressSanitizer','UndefinedBehaviorSanitizer','LeakSanitizer'],
    'coverage': ['Owner/scene/pointer/format/dimension/length/flags rejection before SDK allocation',
        'Hand-calculated min-block, irregular-axis, NX540 and maximum-dimension lengths',
        'Logical descriptor dimensions, clamp/filter, explicit gamma-request rejection, payload copy and complete zero padding',
        'Every retained allocation/descriptor SDK fault; rollback failure leaves reachable closing owner',
        '256KiB rounded CDRAM rejection and exact boundary; compressed CPU update/read/copy and movie rebind rejection'],
    'limitations': ['Host CPU tests with external SDK calls stubbed: no GPU, compressed texel decoding, Vita, or Vita3K execution.',
        'Full backend and real SDK headers compile; unused backend functions are removed with linker garbage collection.',
        'Host 64-bit ABI is tested; this does not verify ARM layout, driver validation, shader sampling, scanout, or native fences.',
        'RED is a private explicit unsupported API, with compatibility storage metadata only if absent from the snapshot.'],
    'sdk_headers': [identity(p) for p in sorted(headers)],
    # The outer tee still appends to host.log after this receipt is written.
    # Hash only the closed per-variant logs and immutable input/output files.
    'files': [identity(p) for p in sorted(build.rglob('*'))
        if p.is_file() and p.name not in ('receipt.json', 'host.log')]}
(build/'receipt.json').write_text(json.dumps(receipt,ensure_ascii=False,indent=2)+'\n')
print(f'RECEIPT {build / "receipt.json"}')
PY
    printf 'RESULT PASS: expected RED%s; immutable evidence is in %s\n' \
        "$([[ $mode == --green ]] && printf ' and actual production GREEN' || printf ' only')" "$build"
} 2>&1 | tee "$build/host.log"

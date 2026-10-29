#!/usr/bin/env bash
# Host fault injection for the real C present implementation; no Vita/GPU run.
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
vita_sdk=${VITASDK:-/opt/vitasdk}
host_cc=${CC:-cc}
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
build="$project/.work/vita/gxm-present/$run_id"
logs="$project/doc/logs/vita"
receipt="$project/doc/vita-gxm-present-host-tests.json"
mkdir -p "$build/backend/shaders/generated" "$logs"

# Keep one immutable input set for the red and green builds. Never roll back the
# shared production source while another task is building or exercising it.
cp "$project/knife/vita/gxm_backend.c" "$build/backend/gxm_backend.c"
cp "$project/knife/vita/gxm_backend.h" "$build/backend/gxm_backend.h"
cp "$project/knife/vita/jpeg_hw.h" "$build/backend/jpeg_hw.h"
cp "$project/knife/vita/image_decode.h" "$build/backend/image_decode.h"
cp "$project/knife/vita/effects_backend.inc" "$build/backend/effects_backend.inc"
cp "$project/knife/vita/movie_gxm.inc" "$build/backend/movie_gxm.inc"
cp "$project/knife/vita/shaders/generated/shaders.h" "$build/backend/shaders/generated/shaders.h"
cp "$project/knife/vita/shaders/generated/effects.h" "$build/backend/shaders/generated/effects.h"
cp "$project/knife/vita/tests/gxm_present_test.c" "$build/gxm_present_test.c"
cp "${BASH_SOURCE[0]}" "$build/test-vita-gxm-present.sh"

{
    printf 'HOST-ONLY GXM present regression; source snapshot: %s\n' "$build"
    "$host_cc" --version | head -n 1
    uname -m
    test -f "$vita_sdk/arm-vita-eabi/include/psp2/gxm.h"
    python3 - "$build" <<'PY'
from pathlib import Path
import sys

build = Path(sys.argv[1])
original = (build / "backend/gxm_backend.c").read_text(encoding="utf-8")
start = original.index("int32_t kv_gxm_present(KvGxm *g) {")
end = original.index("\nint32_t kv_gxm_readback(", start)
# Reproduce the pre-fix submission semantics only in a private test fixture.
# This deliberately lacks the closing/poison transition on submission failure.
old_present = """int32_t kv_gxm_present(KvGxm *g) {
    if (!active(g) || !g->scene_open) return KV_STATE;
    CHECK(g, sceGxmEndScene(g->context, NULL, NULL));
    g->scene_open = 0;
    CHECK(g, sceGxmPadHeartbeat(&g->surfaces[g->back], g->sync[g->back]));
    const DisplayData data = {g->display[g->back].base, &g->display_error};
    g->scanout_submitted = 1;
    CHECK(g, sceGxmDisplayQueueAddEntry(g->sync[g->front], g->sync[g->back], &data));
    sceGxmFinish(g->context);
    CHECK(g, sceGxmDisplayQueueFinish());
    if (g->display_error < 0) return fail(g, "display callback", g->display_error);
    g->front = g->back; g->back = (g->back + 1) % BUFFERS;
    ++g->stats.frames;
    return 0;
}"""
(build / "backend/gxm_backend.red.c").write_text(
    original[:start] + old_present + original[end:], encoding="utf-8")
PY
    flags=(-std=c11 -Wall -Wextra -Werror -g -O1
        -fsanitize=address,undefined -fno-omit-frame-pointer
        -ffunction-sections -fdata-sections -Wl,--gc-sections
        -idirafter "$vita_sdk/arm-vita-eabi/include")
    # -idirafter admits unmodified SDK declarations without replacing host libc
    # headers with the VitaSDK newlib headers. Do not enable SDK ARM ABI asserts
    # on this 64-bit host; the ARM ABI is outside this state-test's authority.
    for variant in red green; do
        backend="$build/backend/gxm_backend.c"
        if [[ $variant == red ]]; then backend="$build/backend/gxm_backend.red.c"; fi
        command=("$host_cc" "${flags[@]}" "-DKV_GXM_BACKEND_SOURCE=\"$backend\""
            -MD -MF "$build/$variant.d" "$build/gxm_present_test.c"
            "-Wl,-Map,$build/$variant.map" -o "$build/present-$variant")
        printf '%q ' "${command[@]}" > "$build/$variant.command"
        printf '\n' >> "$build/$variant.command"
        cat "$build/$variant.command"
        "${command[@]}"
    done

    set +e
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
        "$build/present-red" > "$build/red.log" 2>&1
    red_status=$?
    set -e
    cat "$build/red.log"
    if [[ $red_status -ne 1 ]] || ! grep -q 'g.closing == 1' "$build/red.log" || \
        ! grep -q '^RESULT FAIL: 18 cases,' "$build/red.log"; then
        printf 'ERROR: pre-fix fixture did not fail on the expected closing transition (exit %s)\n' "$red_status"
        exit 1
    fi
    printf '%s\n' 'RED VERIFIED: pre-fix fixture fails the submission poison assertions.'
    ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
        "$build/present-green" 2>&1 | tee "$build/green.log"
    grep -q '^RESULT PASS: 18 cases,' "$build/green.log"
    nm -u "$build/present-green" > "$build/green.undefined-symbols.txt"
    if grep -q 'sce[A-Z]' "$build/green.undefined-symbols.txt"; then
        printf '%s\n' 'ERROR: a VitaSDK import escaped the host stubs.'
        exit 1
    fi
    ldd "$build/present-green" > "$build/green.linked-libraries.txt"
    cp "$build/red.log" "$logs/gxm-present-red.log"
    cp "$build/green.log" "$logs/gxm-present-green.log"

    python3 - "$project" "$build" "$vita_sdk" "$host_cc" "$receipt" <<'PY'
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import re
import shlex
import struct
import subprocess
import sys

project, build, sdk, compiler, destination = sys.argv[1:]
project, build, sdk = Path(project), Path(build), Path(sdk)
def record(path):
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
def result(log):
    text = log.read_text(encoding="utf-8")
    match = re.search(r"^RESULT (PASS|FAIL): (\d+) cases, (\d+) checks, (\d+) failures;", text, re.M)
    if not match:
        raise SystemExit(f"Missing test result: {log}")
    return {"result": match[1], "cases": int(match[2]), "checks": int(match[3]),
            "failures": int(match[4])}
dependencies = shlex.split((build / "green.d").read_text().replace("\\\n", " "))[1:]
sdk_headers = sorted({Path(p) for p in dependencies if Path(p).is_relative_to(sdk)})
if not any(p.name == "gxm.h" for p in sdk_headers):
    raise SystemExit("The compiler did not consume the real VitaSDK gxm.h")
files = [build / p for p in (
    "backend/gxm_backend.c", "backend/gxm_backend.red.c", "backend/gxm_backend.h",
    "backend/jpeg_hw.h", "backend/image_decode.h",
    "backend/effects_backend.inc", "backend/movie_gxm.inc", "backend/shaders/generated/shaders.h",
    "backend/shaders/generated/effects.h", "gxm_present_test.c", "test-vita-gxm-present.sh",
    "present-red", "present-green", "red.command", "green.command", "red.log", "green.log",
    "green.undefined-symbols.txt", "green.linked-libraries.txt")]
receipt = {
    "schema": "knife-vita-gxm-present-host-v1",
    "created_utc": datetime.now(timezone.utc).isoformat(),
    "result": "pass-host-fault-injection-red-and-green",
    "command": "./tools/test-vita-gxm-present.ps1",
    "source": "knife/vita/gxm_backend.c; immutable private snapshot",
    "build_directory": str(build),
    "host": {"architecture": subprocess.check_output(["uname", "-m"], text=True).strip(),
             "pointer_bits": struct.calcsize("P") * 8,
             "compiler": subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
             "sanitizers": ["AddressSanitizer", "UndefinedBehaviorSanitizer", "LeakSanitizer"]},
    "red": {**result(build / "red.log"), "exit_code": 1,
            "expected_failure": "Pre-fix private present fixture leaves closing=0 and active=true after failure."},
    "green": {**result(build / "green.log"), "exit_code": 0},
    "sdk_stubs": ["sceKernelGetThreadId", "sceGxmEndScene", "sceGxmPadHeartbeat",
                  "sceGxmDisplayQueueAddEntry", "sceGxmFinish", "sceGxmDisplayQueueFinish",
                  "sceDisplaySetFrameBuf", "sceDisplayWaitVblankStart"],
    "coverage": [
        "Null owner, no scene, wrong thread, and closing preconditions have no SDK calls or owner changes.",
        "Six injected submission/callback failures at frame counts 0 and 17 preserve front/back/frame count, close the owner, and reject further present calls.",
        "EndScene failure retains scene_open; later failures close the scene; queued scanout is retained for teardown.",
        "Successful first and later presents advance buffers/frame count exactly once; a second present without a new scene is rejected.",
        "The real display callback receives copied queue data and propagates SetFrameBuf/WaitVblankStart errors."
    ],
    "limitations": [
        "CPU host state regression only: all eight retained SDK boundaries are stubs; no GPU, Vita, Vita3K, or SDK library execution.",
        "The complete unmodified C backend and actual VitaSDK headers are compiled, but unused backend functions are discarded with function/data sections and linker garbage collection.",
        "VitaSDK ABI assertions are disabled by their default SDK configuration on this 64-bit host; ARM ABI/layout correctness is not claimed.",
        "Queue callbacks execute synchronously in the host stub; real callback scheduling, GPU/display ownership, teardown, and hardware driver behavior require separate runtime tests.",
        "The red fixture reproduces only the pre-fix present function in a private copy; it is not a separately checked-out historical binary."
    ],
    "sdk_headers": [record(p) for p in sdk_headers],
    "files": [record(p) for p in files],
}
Path(destination).write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"RECEIPT {destination}")
PY
    printf '%s\n' 'RESULT PASS: host fault injection catches the old failure and accepts the production present state transitions.'
} 2>&1 | tee "$logs/gxm-present-host.log"

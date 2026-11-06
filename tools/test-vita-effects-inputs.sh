#!/usr/bin/env bash
# CPU-only contract and mutation tests of the real GXM effect input validator.
set -euo pipefail
project=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
vita_sdk=${VITASDK:-/opt/vitasdk}
host_cc=${CC:-cc}
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
build="$project/.work/vita/effects-inputs/$run_id"
logs="$project/doc/logs/vita"
receipt="$project/doc/vita-effects-inputs-host-tests.json"
if [[ ${KV_TEST_WORK_ONLY:-0} == 1 ]]; then logs="$build/logs"; receipt="$build/receipt.json"; fi
mkdir -p "$build/green/shaders/generated" "$logs"

# Read the same immutable inputs for every variant, retaining the real include
# chain rather than extracting effect_inputs into an alternate implementation.
for source in material_variants.h perf_audit.c perf_audit.h perf_counters.def gxm_backend.c gxm_backend.h effects_backend.inc gpu_lifetime.inc movie_gxm.inc jpeg_hw.h image_decode.h \
    shaders/generated/shaders.h shaders/generated/effects.h; do
    cp "$project/knife/vita/$source" "$build/green/$source"
done
cp "$project/knife/vita/tests/effects_inputs_test.c" "$build/effects_inputs_test.c"
cp "${BASH_SOURCE[0]}" "$build/test-vita-effects-inputs.sh"

{
    printf 'HOST-ONLY effects input validation; immutable snapshot: %s\n' "$build"
    "$host_cc" --version | head -n 1
    uname -m
    test -f "$vita_sdk/arm-vita-eabi/include/psp2/gxm.h"
    python3 - "$build" <<'PY'
from pathlib import Path
import json
import shutil
import sys

build = Path(sys.argv[1])
original = (build / "green/effects_backend.inc").read_text(encoding="utf-8")
# Independent, deliberately broken private copies demonstrate that specific
# negative assertions detect missing boundaries. These are synthetic mutants,
# not historical versions or edits to shared production code.
mutations = {
    "red-enums": [("shader >= EFFECT_PUBLIC_SHADERS || blend >= EFFECT_BLENDS",
                   "shader > EFFECT_PUBLIC_SHADERS || blend > EFFECT_BLENDS")],
    "red-finite": [("!isfinite(params[i])", "isnan(params[i])"),
                   ("!isfinite(floats[j])", "isnan(floats[j])")],
    "red-packed": [("truncf(value) == value", "isfinite(value)")],
    "red-soft": [("packed_integer(v[i].soft[2][1],0,17)",
                  "packed_integer(v[i].soft[2][1],0,18)"),
                 ("packed_integer(v[i].soft[2][2],1,256)",
                  "packed_integer(v[i].soft[2][2],1,257)")],
    "red-group": [("params[7] == 1 && (params[4] >= 0 || params[6] != 0)",
                   "0 && (params[4] >= 0 || params[6] != 0)")],
}
for variant, replacements in mutations.items():
    mutated = original
    for old, new in replacements:
        if mutated.count(old) != 1:
            raise SystemExit(f"Mutation target must match exactly once: {variant}: {old}")
        mutated = mutated.replace(old, new, 1)
    shutil.copytree(build / "green", build / variant)
    (build / variant / "effects_backend.inc").write_text(mutated, encoding="utf-8")
(build / "mutations.json").write_text(json.dumps(mutations, indent=2) + "\n", encoding="utf-8")
PY
    flags=(-std=c11 -Wall -Wextra -Werror -g -O2 -fstrict-aliasing
        -fsanitize=address,undefined -fno-omit-frame-pointer
        -ffunction-sections -fdata-sections -Wl,--gc-sections
        -idirafter "$vita_sdk/arm-vita-eabi/include")
    # Real SDK declarations, host libc, no SDK stubs/libraries. ARM ABI assertions
    # retain their SDK default (off) on this x86_64 host.
    for variant in red-enums red-finite red-packed red-soft red-group green; do
        backend="$build/$variant/gxm_backend.c"
        command=("$host_cc" "${flags[@]}" "-DKV_GXM_BACKEND_SOURCE=\"$backend\""
            -MD -MF "$build/$variant.d" "$build/effects_inputs_test.c" -lm
            "-Wl,-Map,$build/$variant.map" -o "$build/effects-$variant")
        printf '%q ' "${command[@]}" > "$build/$variant.command"
        printf '\n' >> "$build/$variant.command"
        cat "$build/$variant.command"
        "${command[@]}"
        set +e
        ASAN_OPTIONS=detect_leaks=1:halt_on_error=1 UBSAN_OPTIONS=halt_on_error=1 \
            "$build/effects-$variant" > "$build/$variant.log" 2>&1
        status=$?
        set -e
        printf '%s\n' "$status" > "$build/$variant.exit-code"
        # Full case output is preserved; keep the console readable.
        grep '^FAIL .*: ' "$build/$variant.log" | head -n 5 || true
        tail -n 1 "$build/$variant.log"
        if [[ $variant == green ]]; then
            if [[ $status -ne 0 ]] || ! grep -q '^RESULT PASS: ' "$build/$variant.log"; then
                printf 'ERROR: actual production validator failed (exit %s)\n' "$status"
                exit 1
            fi
        else
            if [[ $status -ne 1 ]] || ! grep -q '^RESULT FAIL: ' "$build/$variant.log"; then
                printf 'ERROR: expected mutant assertion failure was not observed for %s (exit %s)\n' "$variant" "$status"
                exit 1
            fi
        fi
        cp "$build/$variant.log" "$logs/effects-inputs-$variant.log"
    done
    grep -qF 'FAIL precondition.shader.108: expected status -1, got 0' "$build/red-enums.log"
    grep -qF 'FAIL precondition.blend.10: expected status -1, got 0' "$build/red-enums.log"
    grep -qF 'FAIL finite.params.0.infinity: expected status -1, got 0' "$build/red-finite.log"
    grep -qF 'FAIL rule.flags.shader.9.value.4: expected status -1, got 0' "$build/red-packed.log"
    grep -qF 'FAIL blur.phase.3: expected status -1, got 0' "$build/red-packed.log"
    grep -qF 'FAIL soft.row.vertex.0.value.4: expected status -1, got 0' "$build/red-soft.log"
    grep -qF 'FAIL invalid.ps.group-mask-with-rule: expected status -1, got 0' "$build/red-group.log"
    grep -qF 'FAIL invalid.ps.group-mask-with-no-alpha: expected status -1, got 0' "$build/red-group.log"
    nm -u "$build/effects-green" > "$build/green.undefined-symbols.txt"
    if grep -q 'sce[A-Z]' "$build/green.undefined-symbols.txt"; then
        printf '%s\n' 'ERROR: a retained VitaSDK import escaped linker garbage collection.'
        exit 1
    fi
    ldd "$build/effects-green" > "$build/green.linked-libraries.txt"
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
variants = ["red-enums", "red-finite", "red-packed", "red-soft", "red-group", "green"]
def record(path):
    data = path.read_bytes()
    return {"path": str(path), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
def result(variant):
    path = build / f"{variant}.log"
    text = path.read_text(encoding="utf-8")
    match = re.search(r"^RESULT (PASS|FAIL): (\d+) cases, (\d+) checks, (\d+) failures; (\d+) valid, (\d+) rejected inputs$", text, re.M)
    if not match:
        raise SystemExit(f"Missing test result: {path}")
    cases = int(match[2])
    if len(re.findall(r"^(?:PASS|FAIL) [^:\n]+$", text, re.M)) != cases:
        raise SystemExit(f"Case log/result count disagree: {variant}")
    return {"result": match[1], "cases": cases, "checks": int(match[3]),
            "failures": int(match[4]), "expected_valid_inputs": int(match[5]),
            "expected_rejected_inputs": int(match[6]),
            "exit_code": int((build / f"{variant}.exit-code").read_text()),
            "command": (build / f"{variant}.command").read_text().strip(),
            "failed_assertions": re.findall(r"^FAIL .+: .+$", text, re.M)}
results = {variant: result(variant) for variant in variants}
green = results["green"]
if green["result"] != "PASS" or green["failures"] or green["exit_code"]:
    raise SystemExit("Actual validator did not pass")
for variant in variants[:-1]:
    item = results[variant]
    if item["result"] != "FAIL" or not item["failures"] or item["exit_code"] != 1 or item["cases"] != green["cases"]:
        raise SystemExit(f"Mutant did not fail comparable cases: {variant}")
dependencies = shlex.split((build / "green.d").read_text().replace("\\\n", " "))[1:]
dependency_paths = {Path(p) for p in dependencies}
sdk_headers = sorted(p for p in dependency_paths if p.is_relative_to(sdk))
if not any(p.name == "gxm.h" for p in sdk_headers):
    raise SystemExit("The compiler did not consume the real VitaSDK gxm.h")
sources = ["gxm_backend.c", "gxm_backend.h", "effects_backend.inc", "movie_gxm.inc",
           "shaders/generated/shaders.h", "shaders/generated/effects.h"]
if not all(build / "green" / source in dependency_paths for source in sources):
    raise SystemExit("The compiler did not consume the complete production include chain")
files = [build / "effects_inputs_test.c", build / "test-vita-effects-inputs.sh", build / "mutations.json",
         build / "green.undefined-symbols.txt", build / "green.linked-libraries.txt"]
for variant in variants:
    files.extend(build / variant / source for source in sources)
    files.extend(build / f"{variant}{suffix}" for suffix in (".command", ".d", ".map", ".log", ".exit-code"))
    files.append(build / f"effects-{variant}")
receipt = {
    "schema": "knife-vita-effects-inputs-host-v1",
    "created_utc": datetime.now(timezone.utc).isoformat(),
    "result": "pass-host-production-inputs-and-five-mutation-controls",
    "command": "bash tools/test-vita-effects-inputs.sh",
    "build_directory": str(build),
    "source": "Actual effect_inputs in an immutable snapshot of knife/vita/effects_backend.inc, included through the complete production gxm_backend.c",
    "host": {"architecture": subprocess.check_output(["uname", "-m"], text=True).strip(),
             "pointer_bits": struct.calcsize("P") * 8,
             "compiler": subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
             "optimization": "-O2 -fstrict-aliasing",
             "sanitizers": ["AddressSanitizer", "UndefinedBehaviorSanitizer", "LeakSanitizer"]},
    "variants": results,
    "mutations": json.loads((build / "mutations.json").read_text()),
    "sdk_stubs": [],
    "coverage": [
        "Null pointers, triangle count and maximum count; invalid enum upper boundary and UINT32_MAX; all legal legacy and optional shader/blend combinations.",
        "NaN, positive infinity and negative infinity in every parameter and every float of all three vertices; finite extrema outside packed fields.",
        "Maximum vertex allocation and a non-finite field in its last vertex; vertex and parameter byte immutability checked for every case.",
        "Unequal haze on either later vertex in every haze field; valid shared haze; RimText levels and 24-bit packed colors.",
        "Soft radius sign and zero/minimum positive activation; every vertex's LUT row 0..17 and resolution 1..256 including fractional and immediate outside values.",
        "Rule/Transition packed flags 8..2047 and strict activation at mode >0.5; Rule positive dimensions.",
        "PS operations 1..24 with and without fused group masks; reject fractional/outside group modes, rule/no-alpha conflicts and signed-zero rule activation; positive dimensions and Blur integer bounds.",
        "Five private mutation controls independently weaken enum, finite-only, integer-only, soft LUT bounds and group-mask exclusion; each must fail its named assertions."
    ],
    "limitations": [
        "CPU host validator only: no Vita, GPU, Vita3K, SDK library, shader execution, texture sampling, or rendered-pixel claim.",
        "The complete C backend and real VitaSDK declarations are compiled; uncalled runtime functions and their imports are discarded by linker garbage collection.",
        "SDK ARM ABI assertions retain their default off setting on x86_64; this result does not establish ARM ABI or hardware behavior.",
        "The private input validator cannot verify caller buffer lengths, texture/LUT dimensions or ownership; tests provide valid allocations and verify only its declared value/count contract.",
        "Mutation controls are deliberately broken synthetic private fixtures, not historical builds, and do not modify shared production sources."
    ],
    "sdk_headers": [record(path) for path in sdk_headers],
    "files": [record(path) for path in files],
}
Path(destination).write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(f"RECEIPT {destination}")
PY
    printf '%s\n' 'RESULT PASS: actual effect input validation and all five negative mutation controls verified.'
} 2>&1 | tee "$logs/effects-inputs-host.log"

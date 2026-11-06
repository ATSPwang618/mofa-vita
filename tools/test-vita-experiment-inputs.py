#!/usr/bin/env python3
"""Sanitized host tests of production PS/blur public inputs and weight upload.

Snapshots the actual backend/include chain, runs positive fixtures, then proves
the public-ID assertions detect a deliberately removed internal-ID guard.
"""
from datetime import datetime, timezone
from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
SOURCES = ["material_variants.h", "gxm_backend.c", "gxm_backend.h", "effects_backend.inc", "perf_audit.h",
    "perf_counters.def", "jpeg_hw.h", "image_decode.h", "runtime_shaders.h", "gpu_lifetime.inc",
    "movie_gxm.inc", "shaders/generated/shaders.h", "shaders/generated/effects.h"]


def main():
    destination = ROOT / ".work/vita/experiment-inputs"
    destination.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=destination))
    green = run / "green"
    for source in SOURCES:
        target = green / source
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / "knife/vita" / source, target)
    tests = ["plain_ps_inputs_test.c", "blur_weights_upload_test.c", "full_ps_inputs_test.c"]
    for test in tests:
        shutil.copyfile(ROOT / "knife/vita/tests" / test, run / test)
    red = run / "red-internal-id-bypass"
    shutil.copytree(green, red)
    source = (red / "effects_backend.inc").read_text()
    guard = "        || (shader >= 32 && shader < 48)\n"
    if source.count(guard) != 1:
        raise SystemExit("Public internal-ID guard mutation target is not unique")
    (red / "effects_backend.inc").write_text(source.replace(guard, "", 1))
    compiler = os.environ.get("CC", "cc")
    sdk = Path(os.environ.get("VITASDK", "/opt/vitasdk"))
    flags = ["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-fstrict-aliasing",
        "-fsanitize=address,undefined", "-fno-omit-frame-pointer", "-ffunction-sections",
        "-fdata-sections", "-Wl,--gc-sections", "-idirafter", str(sdk / "arm-vita-eabi/include")]
    results = {}
    for name, test, backend, wanted in [
        ("public-inputs", tests[0], green, 0),
        ("blur-upload", tests[1], green, 0),
        ("full-ps-inputs", tests[2], green, 0),
        ("red-internal-id-bypass", tests[0], red, 1),
    ]:
        executable = run / f"{name}.test"
        command = [compiler, *flags, f'-DKV_GXM_BACKEND_SOURCE="{backend / "gxm_backend.c"}"',
            str(run / test), "-lm", "-o", str(executable)]
        subprocess.run(command, check=True)
        environment = dict(os.environ, ASAN_OPTIONS="detect_leaks=1:halt_on_error=1",
            UBSAN_OPTIONS="halt_on_error=1")
        result = subprocess.run([str(executable)], env=environment, text=True, capture_output=True)
        output = result.stdout + result.stderr
        (run / f"{name}.log").write_text(output)
        results[name] = {"exit_code": result.returncode, "command": command, "output": output}
        if result.returncode != wanted or (wanted == 0 and "RESULT PASS" not in output) or (wanted == 1 and "16 failures" not in output):
            raise SystemExit(f"Unexpected {name} result: {output}")
        print(name, result.stdout.strip())
    files = []
    for path in sorted(run.rglob("*")):
        if path.is_file():
            data = path.read_bytes()
            files.append({"path": str(path.relative_to(run)), "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
    receipt = {"schema": "knife-vita-experiment-inputs-host-v1", "time_utc": datetime.now(timezone.utc).isoformat(),
        "compiler": subprocess.check_output([compiler, "--version"], text=True).splitlines()[0],
        "results": results, "files": files, "limitations": [
            "Host-only actual C input/copy/match/upload routines; no Cg/GXP, ARM or GPU execution.",
            "Input validator executable links no retained SDK calls. Upload test records only the native uniform-call boundary and creator thread.",
            "Red control is a private synthetic mutation of the public internal-ID guard, not a historical build.",
            "This does not establish RGBA8 pixel equality or performance of an experiment."]}
    (run / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print("RECEIPT", run / "receipt.json")


if __name__ == "__main__":
    main()

"""Run the Rust renderer regression and retain source-bound local evidence.

The Vita renderer uses a host SDK double in these tests. This runner does not
execute the ARM FFI, GPU, emulator, or physical console.
"""
from __future__ import annotations

import hashlib
import json
import re
import shutil
import subprocess
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def identity(path: Path):
    data = path.read_bytes()
    return {"path": path.relative_to(ROOT).as_posix(), "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest()}


def main():
    run_id = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    run = ROOT / ".work/vita/render-host" / run_id
    run.mkdir(parents=True, exist_ok=False)
    sources = {ROOT / "knife/Cargo.toml", ROOT / "knife/Cargo.lock", Path(__file__).resolve()}
    for package in ("knife-render", "knife-render-api", "knife-platform-vita"):
        base = ROOT / "knife" / package
        sources.add(base / "Cargo.toml")
        sources.update(p for p in (base / "src").rglob("*")
                       if p.is_file() and p.suffix in {".rs", ".wgsl"})
    records = [identity(p) for p in sorted(sources)]
    for p in sorted(sources):
        dest = run / "sources" / p.relative_to(ROOT)
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(p, dest)
    wsl_root = subprocess.check_output(["wsl.exe", "-d", "pocket-ubuntu", "--", "wslpath",
                                        "-a", ROOT.as_posix()], text=True).strip()
    command = ["wsl.exe", "-d", "pocket-ubuntu", "--", "env",
               "PATH=/root/.cargo/bin:/usr/bin:/bin", "CARGO_TARGET_DIR=/root/.cache/knife-vita",
               "cargo", "+nightly-2026-05-28", "test", "--manifest-path",
               wsl_root + "/knife/Cargo.toml", "--offline", "--locked", "-p", "knife-render"]
    result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    log = run / "output.log"
    log.write_bytes(result.stdout)
    public_log = ROOT / "doc/logs/vita/270p-render-host.log"
    public_log.write_bytes(result.stdout)
    output = result.stdout.decode("utf-8", errors="replace")
    print(output)
    assert records == [identity(p) for p in sorted(sources)], "Test sources changed during execution"
    matches = re.findall(r"test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; "
                         r"(\d+) measured; (\d+) filtered out;", output)
    assert result.returncode == 0 and matches, "Renderer test failed or returned no results"
    assert all(m[0] == "ok" and int(m[2]) == int(m[3]) == int(m[4]) == int(m[5]) == 0
               for m in matches), "Failed, ignored or filtered tests"
    receipt = {"schema": "knife-vita-render-host-v1", "tested_at": datetime.now(timezone.utc).isoformat(),
               "command": command, "result": "PASS", "exit_code": result.returncode,
               "passed": sum(int(m[1]) for m in matches), "failures": 0, "ignored": 0,
               "run_directory": run.relative_to(ROOT).as_posix(),
               "compiler": subprocess.check_output(["wsl.exe", "-d", "pocket-ubuntu", "--",
                   "/root/.cargo/bin/rustc", "+nightly-2026-05-28", "--version"], text=True).strip(),
               "source_files": records, "source_files_unchanged_during_test": True,
               "source_snapshots": [identity(run / "sources" / p.relative_to(ROOT)) for p in sorted(sources)],
               "log": identity(public_log), "retained_log": identity(log),
               "coverage": ["Full knife-render host test suite without filters",
                   "96 baked UI textures with only one spare render target: attachments released after each bake",
                   "Failed attachment release does not publish a resident texture or a cache charge",
                   "Existing GPU copy, texture ownership, pinning, GXT metadata and draw routing regressions"],
               "limitations": ["Vita renderer uses a host SDK double; no native driver pool, GPU pixels or timing.",
                   "Tests compile the workspace sources; before/after hashes match the retained source snapshots.",
                   "ARM FFI compilation and hardware behavior require separate verification."]}
    data = json.dumps(receipt, ensure_ascii=False, indent=2) + "\n"
    (run / "receipt.json").write_text(data, encoding="utf-8")
    (ROOT / "doc/vita-render-host-tests.json").write_text(data, encoding="utf-8")
    print(f"RECEIPT {run / 'receipt.json'}")


if __name__ == "__main__":
    main()

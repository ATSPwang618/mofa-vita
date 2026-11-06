#!/usr/bin/env python3
"""Run the production-parser script census and validate its bounded report.

The Rust worker is intentionally the only default parser.  This controller does
not reimplement MZP/MZX, copy assets, or synthesize GPU observations.  If Cargo
is unavailable it emits an explicit blocked evidence report instead of silently
falling back to a second parser.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ASSET = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets/allscr.mrg"
DEFAULT_OUT = ROOT / ".work/reports/script-census.json"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def blocked(asset: Path, reason: str) -> dict:
    return {
        "schema": "knife-script-census-v1",
        "execution": {"status": "blocked", "reason": reason, "required": "cargo run --manifest-path tools/script-census/Cargo.toml"},
        "source": {"path": str(asset), "allscr_sha256": sha256(asset) if asset.exists() else None,
                    "parser": "knife-core::format::script::SceneArchive + knife-core::vm::parse::Scene"},
        "scene_count": None, "scenes": [],
        "fixture": {"scene_index": 38, "pc_range": [1221, 1246],
                     "fixed_phases_ms": [1500, 2000, 2300, 2500, 3000, 3500, 4000, 4500, 5000],
                     "status": "not_extracted", "runtime_integration": {"status": "not_run"}},
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--asset", type=Path, default=DEFAULT_ASSET)
    ap.add_argument("--output", type=Path, default=DEFAULT_OUT)
    ap.add_argument("--self-test", action="store_true")
    ns = ap.parse_args()
    if ns.self_test:
        assert (ROOT / "tools/script-census/src/main.rs").exists()
        assert "SceneArchive::parse" in (ROOT / "tools/script-census/src/main.rs").read_text()
        print("script-census controller self-test: ok")
        return 0
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    cargo = shutil.which("cargo")
    if not cargo:
        report = blocked(ns.asset, "cargo executable is not installed on this host")
        ns.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        print(json.dumps({"status": "blocked", "output": str(ns.output), "reason": report["execution"]["reason"]}))
        return 2
    proc = subprocess.run([cargo, "run", "--manifest-path", str(ROOT / "tools/script-census/Cargo.toml"), "--", str(ns.asset), str(ns.output)], cwd=ROOT)
    if proc.returncode:
        return proc.returncode
    report = json.loads(ns.output.read_text(encoding="utf-8"))
    assert report["schema"] == "knife-script-census-v1"
    assert report["execution"]["status"] == "completed"
    assert report["execution"]["scene_count"] == report["scene_count"]
    assert report["execution"]["asset_sha256"] == report["source"]["allscr_sha256"]
    assert report["source"]["parser"].startswith("knife-core::")
    assert report["scene_count"] == len(report["scenes"])
    assert report["fixture"]["pc_range"] == [1221, 1246]
    print(json.dumps({"status": "ok", "scenes": report["scene_count"], "output": str(ns.output)}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

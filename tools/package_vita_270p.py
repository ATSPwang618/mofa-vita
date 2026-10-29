"""Package the local 270p source tree without caches or external game assets."""
from __future__ import annotations

import hashlib
import json
import os
import subprocess
import zipfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-270p-project.zip"
PRUNE = {".git", ".work", ".superpowers", "target", "node_modules", "__pycache__"}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def files():
    # Project documents live under doc/; the overview doubles as the archive README.
    selected = {"README.md": ROOT / "doc/01-项目总览/项目总览与当前交付.md"}
    for base, dirs, names in os.walk(ROOT / "knife", followlinks=False):
        dirs[:] = sorted(d for d in dirs if d not in PRUNE)
        for d in dirs:
            p = Path(base) / d
            assert not p.is_symlink() and not p.is_junction(), str(p)
        for name in sorted(names):
            p = Path(base) / name
            relative = p.relative_to(ROOT).as_posix()
            if relative in {"knife/screenshot.png", "knife/screenshot_woh.png"}:
                continue
            selected[relative] = p
    for p in (ROOT / "tools").iterdir():
        if p.is_file() and p.suffix in {".py", ".mjs", ".sh", ".ps1", ".txt", ".cpp", ".c", ".h"}:
            selected[p.relative_to(ROOT).as_posix()] = p
    for name in ["03-交付与实机/270p交付报告.md", "vita-270p-delivery.json", "vita-270p-assets-validation.json",
                 "vita-game-release-build.json", "vita-game-build.json",
                 "vita-270p-vpk-validation.json", "vita-270p-jpeg-admission.json",
                 "vita-audio-ui-clarity-20261028.json", "vita-ui-clarity-assets-20261028.json",
                 "vita-gxm-startup-crash-fix-20261028.json",
                 "vita-gxm-sync-crash-fix-1793187197.json",
                 "vita-gxm-copy-host-tests.json", "vita-gxm-viewport-host-tests.json",
                 "vita-effects-inputs-host-tests.json", "vita-gxm-present-host-tests.json",
                 "vita-render-host-tests.json",
                 "vita-game-backlog-inputs-staged-1793140499461-6de6da-vulkan-disabled-runtime.json",
                 "vita-game-save-inputs-staged-1793140745791-9ceb68-vulkan-disabled-runtime.json"]:
        selected["doc/" + name] = ROOT / "doc" / name
    delivery = json.loads((ROOT / "doc/vita-270p-delivery.json").read_text(encoding="utf-8"))
    for run in delivery["finite_release_observations"] + delivery.get("historical_release_observations", []):
        for record in (run["receipt"], run.get("performance_log")):
            if record:
                p = Path(record["path"])
                p = p if p.is_absolute() else ROOT / p
                assert p.resolve().is_relative_to((ROOT / "doc").resolve()), str(p)
                data = p.read_bytes()
                assert len(data) == record["bytes"] and digest(data) == record["sha256"], str(p)
                selected[p.relative_to(ROOT).as_posix()] = p
    for p in (ROOT / "doc").rglob("*.md"):
        selected[p.relative_to(ROOT).as_posix()] = p
    for pattern in ["270p-*.log", "gxm-present-*.log", "game-release-build.log", "image-decode-*.log",
                    "image-rust-host.log", "image-stream-host.log", "performance-reader-tests.log"]:
        for p in (ROOT / "doc/logs/vita").glob(pattern):
            selected[p.relative_to(ROOT).as_posix()] = p
    selected["INSTALL-270p.md"] = ROOT / "doc/03-交付与实机/安装与更新说明-270p.md"
    return dict(sorted(selected.items()))


def main():
    if OUTPUT.exists() or OUTPUT.with_suffix(".zip.tmp").exists():
        raise FileExistsError("Refusing to replace an existing source archive or pending archive")
    selected = files()
    assert 169 < len(selected) < 3000
    build = json.loads((ROOT / "doc/vita-game-release-build.json").read_text(encoding="utf-8"))
    for f in build["source_files"]:
        assert f["path"] in selected, f["path"]
    records = []
    total = 0
    for name, p in selected.items():
        assert not p.is_symlink() and not p.is_junction() and p.is_file(), name
        assert p.resolve().is_relative_to(ROOT.resolve()), name
        assert not (set(Path(name).parts) & PRUNE), name
        data = p.read_bytes()
        assert len(data) < 20 * 1024 ** 2, name
        total += len(data)
        records.append({"path": name, "bytes": len(data), "sha256": digest(data)})
    assert total < 40 * 1024 ** 2
    by_name = {f["path"]: f for f in records}
    for f in build["source_files"]:
        assert by_name[f["path"]] == f, f["path"]
    vc = lambda *args: subprocess.check_output(["git", "-C", str(ROOT / "knife"), *args], text=True).strip()
    snapshot = {"schema": "knife-vita-270p-source-snapshot-v1", "created_at": datetime.now(timezone.utc).isoformat(),
                "files": records, "source_bytes": total, "branch": vc("branch", "--show-current"),
                "head": vc("rev-parse", "HEAD"), "working_tree_snapshot": True,
                "selected_release_build_sources_matched": len(build["source_files"]),
                "excluded": ["external game assets/ROMs", "personal saves", "SDK and Cargo caches", "emulator binaries",
                             ".git", "all .work and target directories", "screenshots of commercial game content"],
                "prerequisites": ["VitaSDK /opt/vitasdk", "Rust nightly-2026-05-28 and rust-src",
                                  "offline Cargo dependencies and required Vita PNG/JPEG/Opus libraries"],
                "scope": "Source snapshot and recorded finite local observations; not a hardware performance certificate."}
    pending = OUTPUT.with_suffix(".zip.tmp")
    with zipfile.ZipFile(pending, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for f in records:
            data = selected[f["path"]].read_bytes()
            assert digest(data) == f["sha256"] and len(data) == f["bytes"], f["path"]
            archive.writestr(f["path"], data)
        archive.writestr("SNAPSHOT.json", json.dumps(snapshot, ensure_ascii=False, indent=2) + "\n")
    with zipfile.ZipFile(pending) as archive:
        assert archive.testzip() is None
        assert len(archive.namelist()) == len(records) + 1
        for f in records:
            data = archive.read(f["path"])
            assert len(data) == f["bytes"] and digest(data) == f["sha256"], f["path"]
        assert json.loads(archive.read("SNAPSHOT.json"))["files"] == records
    pending.rename(OUTPUT)
    result = {"schema": "knife-vita-270p-source-package-v1", "verified_at": datetime.now(timezone.utc).isoformat(),
              "path": OUTPUT.relative_to(ROOT).as_posix(), "bytes": OUTPUT.stat().st_size,
              "sha256": digest(OUTPUT.read_bytes()), "files": len(records) + 1,
              "uncompressed_bytes_without_snapshot": total, "all_crc_checked": True, "all_file_hashes_checked": True,
              "selected_build_sources_matched": len(build["source_files"]), "commercial_asset_directories_included": False,
              "branch": snapshot["branch"], "head": snapshot["head"], "git_metadata_included": False,
              "pushed": False, "hardware_verified": False}
    (ROOT / "doc/vita-270p-project-package.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()

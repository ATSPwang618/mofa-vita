"""Bind the local audio/UI release to its build, regression logs and assets.

This does not launch or upload the application. Older runtime observations are
retained with their original identities, separately from this untested release.
"""
from __future__ import annotations

import copy
import hashlib
import json
import shutil
import subprocess
import sys
import zipfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita"
FROZEN = ROOT / ".work/vita/pre-audio-clarity-20261028"
WORK = ROOT / ".work/vita/audio-ui-clarity-20261028"


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8"))


def write(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def identity(path):
    path = Path(path).resolve()
    try:
        name = path.relative_to(ROOT).as_posix()
    except ValueError:
        name = path.as_posix()
    with path.open("rb") as file:
        sha = hashlib.file_digest(file, "sha256").hexdigest()
    return {"path": name, "bytes": path.stat().st_size, "sha256": sha}


def checked(record):
    actual = identity(ROOT / record["path"])
    assert (actual["bytes"], actual["sha256"]) == (record["bytes"], record["sha256"]), record["path"]
    return actual


def log_test(name, expected, scope):
    path = ROOT / "doc/logs/vita" / name
    text = path.read_text(encoding="utf-8")
    for marker in expected:
        assert marker in text, (name, marker)
    return {"log": identity(path), "status": "pass", "expected_result_markers": expected, "scope": scope}


def main():
    now = datetime.now(timezone.utc).isoformat()
    old_path = WORK / "previous-delivery.json"
    if not old_path.exists():
        previous_bytes = (ROOT / "doc/vita-270p-delivery.json").read_bytes()
        previous = json.loads(previous_bytes)
        assert previous["schema"] == "knife-vita-270p-delivery-v3"
        assert previous["engine"]["release"]["sha256"] == "55ae023fc25d833a98bc209872266e8513318b42fbda303e83e13bdbce26658a"
        with old_path.open("xb") as file:
            file.write(previous_bytes)
    old = read(old_path)
    old_vpk = FROZEN / old["engine"]["release"]["path"]
    assert identity(old_vpk)["sha256"] == old["engine"]["release"]["sha256"]
    build_path = ROOT / "doc/vita-game-release-build.json"
    build = read(build_path)
    assert build["build_kind"] == "release" and build["title_id"] == "KNIF00270"
    assert build["feature"] == "vita-gxm" and build["save_namespace"] == "production"
    assert build["frame_limit"] is None and not build["diagnostics_enabled"]
    assert build["scene_pixels"] == [480, 270] and build["composite_pixels"] == [960, 540]
    assert not build["hardware_verified"] and not build["runtime_verified"]
    for record in build["source_files"] + build["artifacts"]:
        checked(record)
    artifacts = {Path(f["path"]).name: f for f in build["artifacts"]}
    built_vpk = ROOT / artifacts["knife-vita-game-270p.vpk"]["path"]
    with zipfile.ZipFile(built_vpk) as archive:
        assert archive.testzip() is None
        assert hashlib.sha256(archive.read("eboot.bin")).hexdigest() == artifacts["eboot.bin"]["sha256"]
    render_path = ROOT / "doc/vita-render-host-tests.json"
    render = read(render_path)
    assert render["result"] == "PASS" and render["passed"] == 65 and render["failures"] == 0
    assert render["source_files_unchanged_during_test"]
    for record in render["source_files"] + render["source_snapshots"] + [render["log"], render["retained_log"]]:
        checked(record)
    tests = [
        log_test("270p-audio-pool-tests.log", ["5 passed; 0 failed", "drained=true released=true"],
                 "Five AudioOut ownership/queue/rebuffer/edge tests; retaining-pointer C mock with ASan/UBSan; Opus output probe and ARM ABI link. No physical AudioOut timing."),
        log_test("270p-audio-platform-tests.log", ["16 passed; 0 failed", "61 passed; 0 failed"],
                 "knife-audio and knife-platform-vita host library tests, including perf flag selection. No physical audio quality claim."),
        log_test("270p-glyph-pages-tests.log", ["4 passed; 0 failed"],
                 "Bounded two-page glyph atlas and queued-texture ownership tests with mock renderer."),
        {"receipt": identity(render_path), "status": "pass", "passed": 65, "failed": 0,
         "scope": "Complete knife-render host suite, source-bound; cross-resolution composition, alias rejection and capture sizes use mock GXM."},
        log_test("270p-ui-clarity-tests.log", ["Ran 6 tests", "OK"],
                 "Selective UI restoration, palette/alpha handling, format, logical dimensions and manifest policy tests."),
        log_test("270p-assets-validator-tests.log", ["pass 9", "fail 0"],
                 "V1/V2 manifest, scene/UI limits and optional perf flag admission tests."),
        log_test("performance-reader-tests.log", ["Ran 9 tests", "OK"],
                 "Performance parser including audio counters, wrapping/reset detection and old-log compatibility."),
    ]
    restore = read(WORK / "restore-report.json")
    assert restore["applied"] and restore["full_reopen_pixel_verification"] and not restore["other_archives_changed"]
    for name, record in restore["after"].items():
        actual = identity(DIST / "assets" / name)
        assert (actual["bytes"], actual["sha256"]) == (record["bytes"], record["sha256"]), name
    manifest_path = DIST / "assets/vita-270p-assets.json"
    manifest = read(manifest_path)
    validation_path = ROOT / "doc/vita-270p-assets-validation.json"
    validation = read(validation_path)
    assert manifest["schema"] == "knife-270p-assets-v2"
    assert validation["sha256"] == identity(manifest_path)["sha256"]
    assert validation["complete_file_hashes_checked"] and validation["private_archive_hashes_checked"]
    assert validation["verification"]["all_pass"] and validation["textures_verified"] == 34847
    jpeg_audit_path = ROOT / "doc/vita-270p-jpeg-admission.json"
    jpeg_audit = read(jpeg_audit_path)
    assert jpeg_audit["asset_manifest_sha256"] == identity(manifest_path)["sha256"]
    assert jpeg_audit["native_source_sha256"] == identity(ROOT / "knife/vita/jpeg_hw.c")["sha256"]
    assert jpeg_audit["textures"] == 34847
    assert jpeg_audit["formats"] == {"2": 31338, "1": 3480, "7": 29}
    assert jpeg_audit["jpeg_candidates"] + jpeg_audit["jpeg_fallback"] == jpeg_audit["formats"]["1"]
    previous_jpeg_audit_path = WORK / "previous-jpeg-admission.json"
    assert identity(previous_jpeg_audit_path)["sha256"] == old["image_loading"]["admission_audit"]["sha256"]
    assert sum(f["bytes"] for f in manifest["files"]) == manifest["total_bytes"]
    core_files = len(manifest["files"]) + 1
    core_bytes = manifest["total_bytes"] + manifest_path.stat().st_size
    asset_files = [p for p in (DIST / "assets").rglob("*") if p.is_file()]
    optional = [p for p in asset_files if p.name == "perf-enable.txt"]
    assert len(asset_files) == core_files + len(optional)
    assert sum(p.stat().st_size for p in asset_files) == core_bytes + sum(p.stat().st_size for p in optional)
    provided_flag = Path("C:/Users/26241/Desktop/perf-enable.txt")
    provided_flag_bytes = provided_flag.read_bytes()
    assert len(provided_flag_bytes) <= 512 and provided_flag_bytes.decode("utf-8-sig").strip() == "1"
    assets_receipt_path = ROOT / "doc/vita-ui-clarity-assets-20261028.json"
    write(assets_receipt_path, {"schema": "knife-vita-ui-clarity-assets-v1", "verified_at": now,
          "generator": identity(__file__), "restoration": restore, "source_report": identity(WORK / "restore-report.json"),
          "validation": identity(validation_path), "manifest": identity(manifest_path),
          "changed_files": [identity(DIST / "assets" / n) for n in restore["changed_files"]],
          "core_files": core_files, "core_bytes": core_bytes,
          "added_bytes": core_bytes - old["assets"]["bytes"],
          "optional_files": [identity(p) for p in optional], "hardware_verified": False})
    destination = DIST / "knife-vita-270p.vpk"
    if destination.exists():
        assert identity(destination)["sha256"] in (identity(old_vpk)["sha256"], identity(built_vpk)["sha256"])
    shutil.copyfile(built_vpk, destination)
    vpk = identity(destination)
    assert vpk["sha256"] == artifacts["knife-vita-game-270p.vpk"]["sha256"]
    checks = []
    for tool, suffix, extra in [(ROOT / "tools/check-vita-package.py", "check", ["KNIF00270", "--release"]),
                                (Path("C:/Users/26241/.codex/skills/vita-port/scripts/check_vpk.py"), "independent-check", [])]:
        command = [sys.executable, "-X", "utf8", str(tool), str(destination), *extra]
        result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log = ROOT / f"doc/logs/vita/270p-vpk-{suffix}.log"
        log.write_bytes(result.stdout)
        print(result.stdout.decode("utf-8", errors="replace").strip())
        assert result.returncode == 0, command
        checks.append({"checker": identity(tool), "command": command, "exit_code": 0, "log": identity(log)})
    vpk_validation_path = ROOT / "doc/vita-270p-vpk-validation.json"
    write(vpk_validation_path, {"schema": "knife-vita-release-package-check-v1", "verified_at": now,
          "vpk": vpk, "checks": checks, "hardware_verified": False})
    fix_path = ROOT / "doc/vita-audio-ui-clarity-20261028.json"
    write(fix_path, {"schema": "knife-vita-audio-ui-clarity-v1", "created_at": now, "generator": identity(__file__),
          "previous_vpk": identity(old_vpk), "previous_delivery": identity(old_path),
          "previous_jpeg_admission": identity(previous_jpeg_audit_path), "jpeg_admission": identity(jpeg_audit_path),
          "user_report": "Previous local release runs on physical Vita but has light crackling and blurred text/UI; not a source-bound runtime measurement.",
          "audio": {"sample_rate": 48000, "channels": 2, "sample_format": "S16",
              "block_frames": 960, "block_ms": 20, "queue_blocks": 4, "queue_capacity_ms": 80,
              "pool_blocks": 6, "pcm_pool_bytes": 23040, "gap_buffers_bytes": 7680,
              "prime_blocks": 2, "prime_timeout_ms": 40, "starvation_edge_frames": 128,
              "lifetime": "Retain current and prior submitted PCM until subsequent blocking output settles; drain/release before final free; retain ownership on driver failure.",
              "red_model": identity(FROZEN / "audio-buffer-red.log"),
              "red_scope": "Retaining-pointer ASan mock reproduces an old lifetime defect; not proof that this caused all crackles on hardware.",
              "global_gain_changed": False, "clipping_policy_changed": False,
              "counters": ["submitted", "output", "underruns", "idle", "queue_full", "peak_queued", "mix_blocks", "mix_max_us", "mix_late", "clipped_samples"]},
          "composition": {"scene_pixels": [480, 270], "text_ui_pixels": [960, 540],
              "scene_target_aligned_bytes": 524288, "composite_target_aligned_bytes": 2097152,
              "extra_bytes_per_promoted_canvas": 1572864, "freeze_canvas": "Same 960x540 size, created on demand",
              "glyph_page_pixels": [1024, 1024], "glyph_page_format": "R8", "glyph_cache_pages_per_atlas": 2,
              "glyph_cache_bytes_per_atlas": 2097152, "glyph_queued_textures_pinned": True,
              "glyph_scope": "Cache-held current/previous pages only; queued/in-flight older pages can remain alive transiently. Separate fonts have separate atlases; not a global font memory peak.",
              "display_buffers": 2, "display_queue_max_pending": 1,
              "native_memory_pool_budgets_increased": False},
          "performance_switch": {"provided_flag": identity(provided_flag),
              "provided_flag_valid": True, "locations_in_precedence_order": ["ux0:data/knife-woh/perf-enable.txt", "ux0:data/knife-woh/assets/perf-enable.txt"],
              "accepted": "Trimmed UTF-8/ASCII value 1, optional UTF-8 BOM, at most 512 bytes; no UTF-16",
              "first_existing_flag_wins": True, "startup_header_flushed": True,
              "profile": "270p-scene-540p-ui-audio-pool-v1", "path": "ux0:data/knife-woh/performance.log",
              "interval_ms": 5000, "max_bytes": 262144, "truncated_each_launch": True},
          "assets": identity(assets_receipt_path), "tests": tests, "build": identity(build_path), "vpk": vpk,
          "elf": artifacts["knife.elf"], "eboot": artifacts["eboot.bin"],
          "runtime_verified": False, "hardware_verified": False,
          "limitations": ["Audio audibility, frame pacing, UI appearance and the larger composite working set require physical retest.",
              "Scene art remains 270p; RGBA8 does not recover lost spatial detail.",
              "Current assets are PNG/JPEG/gray PNG, not DXT. Existing codec8 GXT support retains compressed GPU data.",
              "Pool telemetry is not a whole-process RAM/VRAM measurement."]})
    delivery = copy.deepcopy(old)
    historical_tests = delivery.get("historical_tests", [])
    for test in old["tests"]:
        item = copy.deepcopy(test)
        record = item.pop("receipt")
        item["original_receipt"] = record
        frozen = FROZEN / record["path"]
        current = ROOT / record["path"]
        match = next((p for p in [frozen, current] if p.is_file() and identity(p)["sha256"] == record["sha256"]), None)
        if match:
            item["receipt"] = identity(match)
        else:
            item["limitation"] = "Recorded identity retained; prior mutable receipt no longer available at its original path."
        item["current_build_observation"] = False
        historical_tests.append(item)
    delivery.update({"schema": "knife-vita-270p-delivery-v4", "generated_at": now, "generator": identity(__file__),
        "previous_delivery": identity(old_path), "audio_ui_clarity_fix": identity(fix_path),
        "tests": tests, "historical_tests": historical_tests, "vpk_validation": identity(vpk_validation_path),
        "vpk_checks": [c["log"] for c in checks], "finite_release_observations": [],
        "runtime_verified": False, "hardware_verified": False,
        "scope": "Bounded AudioOut lifetime/queue fixes, 270p scene with 540p text/UI, selective UI assets and early opt-in performance logging. Host regressions and ARM release verified; physical retest pending."})
    delivery["engine"].update({"release": vpk, "build_receipt": identity(build_path), "composite_pixels": [960, 540],
        "selected_build_source_identities_rechecked": len(build["source_files"]), "build_artifacts_rechecked": len(build["artifacts"]),
        "fix": "audio-pool-540p-ui-perf-switch-20261028"})
    delivery["assets"].update({"manifest": identity(manifest_path), "validation": identity(validation_path),
        "files": core_files, "bytes": core_bytes, "statistics": manifest["statistics"], "archives": manifest["archives"],
        "storage_reduction_percent": 100 * (1 - core_bytes / old["assets"]["baseline_bytes"]),
        "pixel_reduction_percent": 100 * (1 - manifest["statistics"]["pixels_after"] / manifest["statistics"]["pixels_before"]),
        "modified_this_fix": True, "ui_policy": manifest["ui_policy"], "ui_restore_receipt": identity(assets_receipt_path),
        "optional_files": [identity(p) for p in optional], "core_count_excludes_optional_files": True})
    delivery["image_loading"].update({"admission_audit": identity(jpeg_audit_path),
        "jpeg_header_candidates": jpeg_audit["jpeg_candidates"], "jpeg_header_fallback": jpeg_audit["jpeg_fallback"],
        "all_candidates_hardware_verified": False})
    for key in ["fullscreen_RGBA8_aligned_bytes_before", "fullscreen_RGBA8_aligned_bytes_after", "fullscreen_target_reduction_percent"]:
        delivery["memory"].pop(key, None)
    delivery["memory"].update({"scene_RGBA8_aligned_bytes": 524288, "composite_RGBA8_aligned_bytes": 2097152,
        "extra_bytes_per_promoted_canvas": 1572864, "freeze_promoted_canvas_allocated_on_demand": True,
        "audio_pcm_pool_bytes": 23040, "audio_gap_buffer_bytes": 7680,
        "glyph_R8_page_bytes": 1048576, "glyph_cache_pages_per_font_atlas": 2,
        "glyph_scope": "Cache-held pages only; queued/in-flight older pages can be retained transiently; each font has its own atlas.",
        "display_buffers": 2, "display_queue_max_pending": 1})
    delivery["limits"][0] = "User reports previous release runs on physical Vita. Current audio/UI release has no source-bound physical run or audio quality measurement."
    delivery["limits"].extend(["540p text/UI composition costs an additional 1.5 MiB per promoted canvas; fixed native budgets remain unchanged.",
        "Audio lifetime and underrun defects are reproduced with controlled host mocks; the cause of every audible crackle remains unverified."])
    write(ROOT / "doc/vita-270p-delivery.json", delivery)
    print(json.dumps({"vpk": vpk, "sources_rechecked": len(build["source_files"]), "artifacts_rechecked": len(build["artifacts"]),
        "core_asset_files": core_files, "core_asset_bytes": core_bytes, "asset_added_bytes": core_bytes - old["assets"]["bytes"],
        "test_groups": len(tests), "hardware_verified": False}, ensure_ascii=False))


if __name__ == "__main__":
    main()

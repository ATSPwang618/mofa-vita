"""Verify local 270p artifact identities and write the delivery record.

This consumes existing build/runtime observations; it never starts a game and
never turns a finite emulator observation into a hardware performance claim.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import subprocess
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita"


def read(p):
    return json.loads(Path(p).read_text(encoding="utf-8-sig"))


def identity(p):
    p = Path(p)
    with p.open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": p.relative_to(ROOT).as_posix(), "bytes": p.stat().st_size, "sha256": digest}


def checked(record):
    p = Path(record["path"])
    p = p if p.is_absolute() else ROOT / p
    actual = identity(p)
    assert (actual["bytes"], actual["sha256"]) == (record["bytes"], record["sha256"]), str(p)
    return actual


def release_run(filename, manifest_sha, vpk_sha):
    receipt = read(ROOT / filename)
    assert receipt["runtime_verified"] and receipt["control_sequence_completed"]
    assert receipt["image_check"]["required_views_nonblack"]
    assert not receipt["hardware_verified"] and receipt["exit"]["code"] == 0
    assert receipt["after_stop"]["phase"] == "stopped" and receipt["shutdown"]["accepted"]
    assert not receipt["unexpected_diagnostic_files"]
    assert receipt["vpk"]["sha256"] == vpk_sha and receipt["asset_manifest"]["sha256"] == manifest_sha
    for key in ("vpk", "build", "asset_manifest", "emulator", "debugger", "host_log",
                "observation_tool", "image_check_tool"):
        checked(receipt[key])
    captures = [{"label": c["label"], **checked(c["artifact"])} for c in receipt["captures"]]
    assert len(captures) == 4
    performance = None
    if receipt["performance_enabled"]:
        checked(receipt["performance_log"])
        spec = importlib.util.spec_from_file_location("performance_reader", ROOT / "tools/read-vita-performance.py")
        reader = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(reader)
        performance = reader.analyze((ROOT / receipt["performance_log"]["path"]).read_bytes(), "emulator")
        assert performance == receipt["performance_summary"]
        assert performance["windows"] > 0 and receipt["performance_verified"]
        assert not performance["queue_and_jpeg_bound_violations"]
        assert not performance["counter_discontinuities"]
    return {"receipt": identity(ROOT / filename), "language": receipt["language"],
            "vpk": receipt["vpk"], "runtime_verified": True, "hardware_verified": False,
            "host_exit": receipt["exit"], "captures": captures,
            "scope": receipt["scope"], "emulator": receipt["emulator"],
            "required_views_nonblack": True, "visual_correctness_automatically_verified": False,
            "performance_log": receipt.get("performance_log"), "performance_summary": performance,
            "emulator_fps_used_as_performance_evidence": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-receipt", action="append", required=True,
                        help="Exact current-build runtime receipt, relative to the project root; supply en and zc")
    args = parser.parse_args()
    manifest_path = DIST / "assets/vita-270p-assets.json"
    manifest = read(manifest_path)
    validation = read(ROOT / "doc/vita-270p-assets-validation.json")
    manifest_id = identity(manifest_path)
    assert validation["sha256"] == manifest_id["sha256"]
    assert validation["complete_file_hashes_checked"] and validation["private_archive_hashes_checked"]
    assert manifest["converter_sha256"] == identity(ROOT / "tools/create_270p.py")["sha256"]
    assert manifest["verification"]["all_pass"] and manifest["verification"]["hd_fallback"] is False
    assets = DIST / "assets"
    actual = [p for p in assets.rglob("*") if p.is_file()]
    assert len(actual) == len(manifest["files"]) + 1
    asset_bytes = sum(p.stat().st_size for p in actual)
    assert asset_bytes == manifest["total_bytes"] + manifest_id["bytes"]
    baseline = Path(manifest["source_assets"])
    old_bytes = sum(p.stat().st_size for p in baseline.rglob("*") if p.is_file())

    build_path = ROOT / "doc/vita-game-release-build.json"
    build = read(build_path)
    for item in build["source_files"] + build["artifacts"]:
        checked(item)
    assert build["build_kind"] == "release" and build["frame_limit"] is None
    assert build["diagnostics_enabled"] is False and build["save_namespace"] == "production"
    assert build["title_id"] == "KNIF00270" and build["physical_canvas"] == [480, 270]
    vpk = identity(DIST / "knife-vita-270p.vpk")
    assert vpk["sha256"] == next(a["sha256"] for a in build["artifacts"] if a["path"].endswith(".vpk"))

    tests = []
    for name, count, marker in [
        ("270p-conversion-tests.log", 6, "Ran 6 tests"),
        ("270p-staging-tests.log", 18, "ℹ pass 18"),
        ("270p-render-tests.log", 62, "test result: ok. 62 passed; 0 failed"),
    ]:
        p = ROOT / "doc/logs/vita" / name
        text = p.read_text(encoding="utf-8-sig")
        assert marker in text and ("\nOK" in text if count == 6 else "ℹ fail 0" in text if count == 18 else True), name
        tests.append({"evidence": identity(p), "passed": count, "failed": 0,
                      "scope": "retained 270p baseline host correctness; no hardware timing"})
    for name, markers, scope in [
        ("image-decode-host.log", ["RESULT PASS: failure injection at every implementation allocation point",
                                  "PASS hardware routing, pitch crop, 420/422, limits, failures, busy serialization",
                                  "PASS hardware init failure is bounded and remembered"],
         "Current C decoder, ASan/UBSan, PNG PLTE/tRNS/Adam7/gray16 and JPEG driver double; not console execution"),
        ("image-decode-arm.log", ["RESULT PASS: Vita ARM C11 -Wall -Wextra -Werror compilation and static library link"],
         "Current Vita C decoder ABI compilation and static link; not executed by this check"),
        ("image-rust-host.log", ["test result: ok. 11 passed; 0 failed"],
         "Current C/Rust image ownership and metadata contracts, 11 host tests"),
        ("image-stream-host.log", ["test result: ok. 1 passed; 0 failed", "test result: ok. 5 passed; 0 failed",
                                  "test result: ok. 2 passed; 0 failed"],
         "Current motion prediction (1), one-worker bounded queues (5), frame histogram (2) host tests"),
        ("performance-reader-tests.log", ["Ran 6 tests", "\nOK"],
         "Current offline performance reader, 6 host tests"),
    ]:
        p = ROOT / "doc/logs/vita" / name
        text = p.read_text(encoding="utf-8-sig")
        assert all(marker in text for marker in markers), name
        tests.append({"evidence": identity(p), "status": "pass", "scope": scope})
    package_checks = []
    for filename, marker in [("270p-vpk-check.log", "PASS: KNIF00270"),
                             ("270p-vpk-independent-check.log", "0 error(s), 0 warning(s)")]:
        p = ROOT / "doc/logs/vita" / filename
        assert marker in p.read_text(encoding="utf-8-sig")
        package_checks.append(identity(p))
    package_validation_path = ROOT / "doc/vita-270p-vpk-validation.json"
    package_validation = read(package_validation_path)
    assert checked(package_validation["vpk"]) == vpk
    assert all(check["exit_code"] == 0 for check in package_validation["checks"])
    assert len(package_validation["checks"]) == 2
    for check in package_validation["checks"]:
        checked(check["log"])
    admission_path = ROOT / "doc/vita-270p-jpeg-admission.json"
    admission = read(admission_path)
    assert admission["native_source_sha256"] == identity(ROOT / "knife/vita/jpeg_hw.c")["sha256"]
    assert admission["asset_manifest_sha256"] == manifest_id["sha256"]
    assert admission["textures"] == 34847 and admission["jpeg_candidates"] + admission["jpeg_fallback"] == 3480

    spec = importlib.util.spec_from_file_location("retained_performance", ROOT / "tools/summarize-vita-artemis-performance.py")
    perf = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(perf)
    runs = {"backlog": perf.retained_run("270p-backlog-7200"),
            "save_load": perf.retained_run("270p-save-load-7200-host-exit-error")}
    for run in runs.values():
        assert run["guest_pass"] and run["input_valid"] and run["probe_verified"] and run["panic"] is None
        assert run["natural_stop"] and run["last_state"]["frame"] == 7200
        assert run["asset_stage"]["sha256"] == manifest_id["sha256"]
        pool = run["memory"]["pools"]["CDRAM"]
        assert pool["peak_bytes"] == 26 * 1024 ** 2 and pool["end_blocks"] == 0
        assert pool["allocations"] == pool["frees"]
    assert runs["backlog"]["runtime_verified"] and runs["backlog"]["host_exit"]["code"] == 0
    assert runs["save_load"]["host_exit"]["hex"] == "0xC0000374" and not runs["save_load"]["runtime_verified"]
    releases = [release_run(name, manifest_id["sha256"], vpk["sha256"]) for name in args.release_receipt]
    assert len(releases) == 2 and {run["language"] for run in releases} == {"en", "zc"}
    assert any(run["performance_summary"] and
               run["performance_summary"]["last_cumulative_counters"]["jpeg_hw_ok"] > 0 for run in releases)
    vc = lambda *args: subprocess.check_output(["git", "-C", str(ROOT / "knife"), *args], text=True).strip()
    result = {"schema": "knife-vita-270p-delivery-v2", "generated_at": datetime.now(timezone.utc).isoformat(),
              "generator": identity(__file__), "engine": {"upstream": "knife 0.9.4", "renderer": "direct GXM",
                  "internal_pixels": [480, 270], "display_pixels": [960, 540], "screen_pixels": [960, 544],
                  "script_coordinates": [1920, 1080], "release": vpk, "build_receipt": identity(build_path),
                  "selected_build_source_identities_rechecked": len(build["source_files"]),
                  "build_artifacts_rechecked": len(build["artifacts"]), "title_id": "KNIF00270",
                  "frame_limit": None, "diagnostics_enabled": False, "save_namespace": "production",
                  "source_package": "dist/Witch-on-the-Holy-Night-Chengkong-270p-project.zip"},
              "assets": {"manifest": manifest_id, "validation": identity(ROOT / "doc/vita-270p-assets-validation.json"),
                  "files": len(actual), "bytes": asset_bytes, "baseline_bytes": old_bytes,
                  "storage_reduction_percent": (1 - asset_bytes / old_bytes) * 100,
                  "pixel_reduction_percent": (1 - manifest["statistics"]["pixels_after"] / manifest["statistics"]["pixels_before"]) * 100,
                  "statistics": manifest["statistics"], "archives": manifest["archives"],
                  "loose_images": manifest["loose_images"], "languages": manifest["languages"],
                  "formats": {"1": "JPEG", "2": "PNG", "7": "grayscale PNG mask"},
                  "webp": False, "prebaked_gxt": False, "hd_fallback": False,
                  "unchanged": ["540p MP4 videos", "Ogg Opus/BFSAR audio", "font files"],
                  "excluded_from_half_image_size": ["runtime glyph cache pages", "platform LiveArea icons"],
                  "old_540p_baseline_retained": True},
              "memory": {"fullscreen_RGBA8_aligned_bytes_before": 2097152,
                  "fullscreen_RGBA8_aligned_bytes_after": 524288,
                  "fullscreen_target_reduction_percent": 75,
                  "newlib_heap_bytes": 67108864, "sceLibcHeapSize_bytes": 4194304, "main_stack_bytes": 2097152,
                  "native_CDRAM_policy_bytes": 67108864, "parameter_reserve_bytes": 16777216,
                  "jpeg_work_CDRAM_reserve_bytes": 3145728,
                  "native_CDRAM_explicit_budget_bytes": 47185920, "mapped_MAIN_budget_bytes": 16777216,
                  "deferred_resident_budget_bytes": 33554432, "deferred_encoded_budget_bytes": 33554432,
                  "ATTRIBUTE2": 0, "budget_enlarged_for_270p": False,
                  "scope": "Current performance log separates GXM mappings/reservation and JPEG CDRAM work; excludes ordinary Rust/newlib heap, stacks, codec heap, audio and other uninstrumented memory. No whole-game RAM peak claim."},
              "image_loading": {"hardware_jpeg": "SceJpeg GetOutputInfo -> DecodeMJpegYCbCr -> MJpegCsc; crop to owned RGBA then main-thread GXM upload",
                  "jpeg_profile": "Baseline 8-bit YCbCr 420/422, 64..480 wide, 64..540 high, <=512 KiB encoded",
                  "fallback": "Bounded libjpeg-turbo software decode on unsupported profile, busy, allocation, driver or contract failure",
                  "png": "Bounded libpng software decode with PLTE/tRNS; no dedicated PNG hardware decoder claim",
                  "admission_audit": identity(admission_path), "jpeg_header_candidates": admission["jpeg_candidates"],
                  "jpeg_header_fallback": admission["jpeg_fallback"], "all_candidates_hardware_verified": False,
                  "worker_threads": 1, "pending_limit": 8, "decoded_ready_limit": 2, "lookahead_limit": 32,
                  "speculative_encoded_limit_bytes": 2097152, "speculative_rgba_limit_bytes": 1048576,
                  "speculative_uploads_per_frame": 1, "archive": "Persistent HED/MRG handle and bounded indexed reads; existing KATL containers",
                  "new_animation_container_implemented": False, "zero_copy": False},
              "tests": tests, "vpk_checks": package_checks, "vpk_validation": identity(package_validation_path),
              "historical_DEV_observations": runs,
              "finite_release_observations": releases,
              "windows_preview": {"present": (DIST / "knife.exe").exists(),
                  "artifact": identity(DIST / "knife.exe") if (DIST / "knife.exe").exists() else None,
                  "rebuilt_this_change": False, "physical_image_assets": [480, 270], "offscreen_pixels": [960, 540]},
              "limits": ["No real PS Vita run; no hardware FPS, CPU-time or full-game OOM guarantee.",
                  "Historical DEV memory figures are earlier-source finite 7200-frame observations, not current-build regressions or all scenes.",
                  "Historical save/load guest checks pass; that run's host Qt shutdown heap exception remains unresolved.",
                  "JPEG hardware API success is observed only in the emulator; PNG uses bounded software decode, no console timing measured.",
                  "Header admission of 3454 JPEGs does not prove all images decoded successfully by physical hardware.",
                  "Release nonblack capture checks do not verify complete menu layout, all colors or animation correctness.",
                  "Videos, transitions, palette/alpha behavior and late-game scenes need broader hardware acceptance.",
                  "Emulator has a documented private memory-bookkeeping fix; it does not enlarge guest budgets."],
              "version_control": {"branch": vc("branch", "--show-current"), "head": vc("rev-parse", "HEAD"),
                  "committed_this_change": False, "pushed": False, "remote_refs_queried": False}}
    output = ROOT / "doc/vita-270p-delivery.json"
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(output), "asset_bytes": asset_bytes, "baseline_bytes": old_bytes,
                      "vpk_sha256": vpk["sha256"], "tests": [t.get("passed", t.get("status")) for t in tests],
                      "source_files_rechecked": len(build["source_files"]), "hardware_verified": False}, ensure_ascii=False))


if __name__ == "__main__":
    main()

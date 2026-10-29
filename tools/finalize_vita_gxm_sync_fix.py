"""Verify and deliver the fix for the physical Vita dump 1793187197.

All native observations remain local. This verifies build/test identities and
the package; it does not launch the release or certify physical GPU behavior.
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

from finalize_vita_gxm_crash_fix import ROOT, DIST, checked, identity, read, write

FROZEN = ROOT / ".work/vita/hardware-crash-1793187197"
OLD_VPK_SHA = "bb0a3b5bd507c2e7146dbf5c40704a455a1cba107a7e53d2e1638cf574eb1ae9"
OLD_ELF_SHA = "d0ed97767585597bbdfb14fe1c099d5472449c94649c355831c05890768bff62"
PANIC = ("submitting Vita game frame: allocating deferred alteration conf_stxt01_en: "
         "GXM: effects render sync: 0x805b0017 (0x805b0017)")


def verify_c_test(name, selection=None):
    path = ROOT / "doc" / name
    receipt = read(path)
    result = (receipt["variants"]["green"] if selection == "variants"
              else receipt["green"] if selection == "green" else receipt)
    assert result["result"] == "PASS" and result["exit_code"] == result["failures"] == 0
    for record in receipt["files"]:
        checked(record)
    for source in ("gxm_backend.c", "effects_backend.inc", "gxm_backend.h"):
        prefix = "/green/" if selection == "variants" else "/backend/"
        snapshot = next(f for f in receipt["files"] if f["path"].endswith(prefix + source))
        assert snapshot["sha256"] == identity(ROOT / "knife/vita" / source)["sha256"], source
    if selection == "variants":
        controls = [v for k, v in receipt["variants"].items() if k != "green"]
        assert len(controls) == 5 and all(v["exit_code"] and v["failures"] > 0 for v in controls)
    elif selection == "green":
        assert receipt["red"]["exit_code"] and receipt["red"]["failures"] > 0
    return {"receipt": identity(path), "status": "pass", "cases": result["cases"],
            "checks": result["checks"], "failed": 0,
            "scope": "Production C backend with host SDK mocks and ASan/UBSan; no hardware GPU execution or timing"}


def historical_test_records(old):
    """Keep older evidence distinct from today's receipts at the same path."""
    history = copy.deepcopy(old.get("historical_tests", []))
    candidates = []
    for directory in ("gxm-copy", "gxm-viewport", "effects-inputs"):
        candidates.extend((ROOT / ".work/vita" / directory).rglob("*.json"))
    for test in old["tests"]:
        previous = copy.deepcopy(test)
        record = previous.pop("receipt")
        previous["original_receipt"] = record
        match = next((p for p in candidates if identity(p)["sha256"] == record["sha256"]), None)
        previous["evidence_file_available"] = match is not None
        if match:
            previous["receipt"] = identity(match)
        else:
            previous["limitation"] = "Original mutable receipt was replaced; its recorded identity remains in the frozen previous delivery."
        previous["current_build_observation"] = False
        history.append(previous)
    return history


def main():
    old = read(FROZEN / "vita-270p-delivery.json")
    assert old["engine"]["release"]["sha256"] == OLD_VPK_SHA
    assert identity(FROZEN / "knife-vita-270p.vpk")["sha256"] == OLD_VPK_SHA
    assert identity(FROZEN / "release/knife.elf")["sha256"] == OLD_ELF_SHA
    for record in read(FROZEN / "frozen-files.json"):
        checked(record)

    build_path = ROOT / "doc/vita-game-release-build.json"
    build = read(build_path)
    assert build["build_kind"] == "release" and build["title_id"] == "KNIF00270"
    assert build["frame_limit"] is None and not build["diagnostics_enabled"]
    assert build["save_namespace"] == "production" and build["physical_canvas"] == [480, 270]
    assert build["feature"] == "vita-gxm" and not build["hardware_verified"] and not build["runtime_verified"]
    for record in build["source_files"] + build["artifacts"]:
        checked(record)
    artifacts = {Path(r["path"]).name: r for r in build["artifacts"]}
    built_vpk = ROOT / artifacts["knife-vita-game-270p.vpk"]["path"]
    assert identity(built_vpk)["sha256"] != OLD_VPK_SHA
    with zipfile.ZipFile(built_vpk) as archive:
        assert archive.testzip() is None
        assert hashlib.sha256(archive.read("eboot.bin")).hexdigest() == artifacts["eboot.bin"]["sha256"]
        assert hashlib.sha256(archive.read("sce_sys/param.sfo")).hexdigest() == artifacts["param.sfo"]["sha256"]

    tests = [verify_c_test(name, variant) for name, variant in [
        ("vita-gxm-copy-host-tests.json", None), ("vita-gxm-viewport-host-tests.json", None),
        ("vita-effects-inputs-host-tests.json", "variants"), ("vita-gxm-present-host-tests.json", "green")]]
    render_path = ROOT / "doc/vita-render-host-tests.json"
    render = read(render_path)
    assert render["result"] == "PASS" and render["exit_code"] == render["failures"] == 0
    assert render["passed"] == 64 and render["ignored"] == 0 and render["source_files_unchanged_during_test"]
    for record in render["source_files"] + render["source_snapshots"] + [render["log"], render["retained_log"]]:
        checked(record)
    tests.append({"receipt": identity(render_path), "status": "pass", "passed": render["passed"],
                  "failed": 0, "scope": "Full knife-render host suite with mock GXM, source identities unchanged; no ARM/GPU runtime claim"})

    red_path = ROOT / ".work/vita/gxm-copy/20261028T114841Z-291/receipt.json"
    red = read(red_path)
    assert red["result"] == "FAIL" and red["cases"] == 21 and red["failures"] == 193
    for record in red["files"]:
        checked(record)

    manifest = DIST / "assets/vita-270p-assets.json"
    assert checked(old["assets"]["manifest"]) == identity(manifest)
    asset_files = [p for p in (DIST / "assets").rglob("*") if p.is_file()]
    assert len(asset_files) == old["assets"]["files"]
    assert sum(p.stat().st_size for p in asset_files) == old["assets"]["bytes"]
    assert read(ROOT / "doc/vita-270p-assets-validation.json")["sha256"] == identity(manifest)["sha256"]

    dump = read(FROZEN / "dump-inspection.json")
    compressed = FROZEN / "psp2core-1793187197-0x00017129f9-eboot.bin.psp2dmp"
    assert identity(compressed)["sha256"] == dump["compressed_sha256"]
    assert identity(FROZEN / "crash.core")["sha256"] == dump["core_sha256"]
    assert PANIC.encode() in (FROZEN / "crash.core").read_bytes()
    main_thread = next(t for t in dump["threads"] if t["name"] == "KNIF00270")
    assert main_thread["pc"] == "0x811f6866" and main_thread["regs"]["r2"] == "0x6"
    assert "_kill_r" in (FROZEN / "knife-abort-symbols.txt").read_text()
    trap = (FROZEN / "knife-abort-trap.asm").read_text()
    assert "811eb866:" in trap and "udf" in trap

    destination = DIST / "knife-vita-270p.vpk"
    shutil.copyfile(built_vpk, destination)
    vpk = identity(destination)
    assert (vpk["bytes"], vpk["sha256"]) == (artifacts["knife-vita-game-270p.vpk"]["bytes"],
                                               artifacts["knife-vita-game-270p.vpk"]["sha256"])
    checks = []
    for tool, suffix, extra in [(ROOT / "tools/check-vita-package.py", "check", ["KNIF00270", "--release"]),
                                (Path("C:/Users/26241/.codex/skills/vita-port/scripts/check_vpk.py"), "independent-check", [])]:
        command = [sys.executable, "-X", "utf8", str(tool), str(destination), *extra]
        result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log = ROOT / f"doc/logs/vita/270p-vpk-{suffix}.log"
        log.write_bytes(result.stdout)
        print(result.stdout.decode("utf-8", errors="replace").strip())
        assert result.returncode == 0, command
        checks.append({"checker": identity(tool), "command": command, "exit_code": result.returncode, "log": identity(log)})
    validation_path = ROOT / "doc/vita-270p-vpk-validation.json"
    write(validation_path, {"schema": "knife-vita-release-package-check-v1",
          "verified_at": datetime.now(timezone.utc).isoformat(), "vpk": vpk, "checks": checks, "hardware_verified": False})

    fix_path = ROOT / "doc/vita-gxm-sync-crash-fix-1793187197.json"
    write(fix_path, {
        "schema": "knife-vita-gxm-sync-crash-fix-v1", "created_at": datetime.now(timezone.utc).isoformat(),
        "generator": identity(__file__), "old_vpk": identity(FROZEN / "knife-vita-270p.vpk"),
        "old_elf": identity(FROZEN / "release/knife.elf"), "compressed_dump": identity(compressed),
        "core": identity(FROZEN / "crash.core"), "dump_analysis": identity(FROZEN / "dump-inspection.json"),
        "panic": PANIC, "scene": 1, "script_pc": 131,
        "failing_api": "sceGxmSyncObjectCreate", "error": "SCE_GXM_ERROR_DRIVER", "error_hex": "0x805b0017",
        "pc": {"runtime": "0x811f6866", "rx_runtime_base": "0x8100b000", "rx_elf_base": "0x81000000",
               "elf_address": "0x811eb866", "symbol": "_kill_r", "instruction": "udf #255",
               "meaning": "Intentional abort trap after Rust panic, not evidence of unsupported ARM instructions"},
        "native_static_evidence": {
            "origin": "User-local firmware module, static inspection; not executed or distributed in the package",
            "elf": identity(ROOT / ".work/vita/hardware-crash-20261028/libgxm_es4.elf"),
            "sync_create_nid": "0x6a6013e1", "sync_create_entry": "0x81019620",
            "driver_failure_branch": "0x81019690", "sync_destroy_entry": "0x810196b4",
            "disassemblies": [identity(FROZEN / name) for name in ["gxm-sync-create.asm", "gxm-sync-helper.asm",
                                                                   "knife-abort-trap.asm", "knife-abort-symbols.txt"]]},
        "established_defects": ["Every ordinary offscreen image allocated a display-style GXM sync object",
                                "Cached baked images retained temporary render targets for their entire pixel-cache lifetime"],
        "uncertainty": "The driver error does not identify ordinary heap exhaustion or a precise native sync-object limit. Driver RW state was not captured.",
        "correction": {"ordinary_offscreen_sync_objects": 0, "display_buffer_sync_preserved": True,
                       "dev_scanout_sync": "created only when explicitly scanning a diagnostic texture out",
                       "baked_image_attachments": "Finish -> TransferFinish -> DestroyRenderTarget before cache residency commit",
                       "pixel_storage_preserved": True, "gpu_written_origin_preserved_for_copy": True,
                       "failure_teardown_retains_owner_and_closes_backend": True,
                       "previous_srgb_register_correction_preserved": True, "memory_budgets_increased": False},
        "pre_fix_regression": {"receipt": identity(red_path), "expected_failure": True,
                               "cases": red["cases"], "checks": red["checks"], "failures": red["failures"]},
        "tests": tests, "build": identity(build_path), "vpk": vpk, "elf": artifacts["knife.elf"],
        "eboot": artifacts["eboot.bin"], "assets_unchanged": True, "assets_manifest": identity(manifest),
        "runtime_verified": False, "hardware_verified": False,
        "limitations": ["Host SDK mocks verify resource and error lifetimes; they do not execute real GXM or verify visual output.",
                        "No execution of the corrected VPK on physical Vita; menu stability, pixels and timing require retest."]})

    delivery = copy.deepcopy(old)
    delivery.update({"generated_at": datetime.now(timezone.utc).isoformat(), "generator": identity(__file__),
                     "previous_delivery": identity(FROZEN / "vita-270p-delivery.json"), "crash_fix": identity(fix_path),
                     "tests": tests, "historical_tests": historical_test_records(old),
                     "vpk_checks": [c["log"] for c in checks], "vpk_validation": identity(validation_path),
                     "finite_release_observations": [], "runtime_verified": False, "hardware_verified": False,
                     "scope": "Physical Vita sync-create failure addressed with bounded offscreen object lifetimes; host tests and ARM package verified, hardware retest pending"})
    delivery["prior_crash_fixes"] = copy.deepcopy(old.get("prior_crash_fixes", [])) + [old["crash_fix"]]
    delivery["failed_hardware_observations"] = copy.deepcopy(old.get("failed_hardware_observations", [])) + [{
        "vpk": identity(FROZEN / "knife-vita-270p.vpk"), "dump": identity(compressed),
        "analysis": identity(fix_path), "outcome": "panic", "error_hex": "0x805b0017",
        "current_build_observation": False}]
    delivery["engine"].update({"release": vpk, "build_receipt": identity(build_path),
        "selected_build_source_identities_rechecked": len(build["source_files"]),
        "build_artifacts_rechecked": len(build["artifacts"]), "fix": "offscreen-sync-and-baked-target-lifetime-1793187197"})
    delivery["assets"]["modified_this_fix"] = False
    delivery["limits"][0] = "The previous release failed on physical Vita while allocating an offscreen sync object. The corrected release awaits physical hardware retest."
    delivery["limits"].append("0x805b0017 is a driver error; no exact native object limit or general heap-OOM diagnosis is claimed.")
    write(ROOT / "doc/vita-270p-delivery.json", delivery)
    print(json.dumps({"vpk": vpk, "test_suites": len(tests), "source_files_rechecked": len(build["source_files"]),
                      "hardware_verified": False}, ensure_ascii=False))


if __name__ == "__main__":
    main()

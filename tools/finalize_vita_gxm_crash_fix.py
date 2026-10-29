"""Deliver the 2026-10-28 GXM startup correction with exact local evidence.

This runs package checkers and copies a verified build. It does not launch the
game or certify hardware behavior. Older runtime observations remain historical.
"""
from __future__ import annotations

import copy
import hashlib
import json
import shutil
import struct
import subprocess
import sys
import zipfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FROZEN = ROOT / ".work/vita/hardware-crash-20261028"
DIST = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita"
OLD_VPK_SHA = "2df3c5ee6837d93a26a92f5cf359ae4d5b827332910694a0c436beeb629b1fbb"
OLD_ELF_SHA = "95743fc5ae2d52c85cad6e7389e02e4a31c8a835d1c72d7a2df765eb6a7dabbb"


def read(path):
    return json.loads(Path(path).read_text(encoding="utf-8-sig"))


def write(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def identity(path):
    path = Path(path).resolve()
    label = path.relative_to(ROOT).as_posix() if path.is_relative_to(ROOT) else str(path)
    with path.open("rb") as stream:
        sha = hashlib.file_digest(stream, "sha256").hexdigest()
    return {"path": label, "bytes": path.stat().st_size, "sha256": sha}


def checked(record):
    path = Path(record["path"])
    if record["path"].startswith("/mnt/d/"):
        path = Path("D:/" + record["path"][7:])
    elif not path.is_absolute():
        path = ROOT / path
    actual = identity(path)
    assert (actual["bytes"], actual["sha256"]) == (record["bytes"], record["sha256"]), path
    return actual


def main():
    old = read(FROZEN / "vita-270p-delivery.json")
    assert old["engine"]["release"]["sha256"] == OLD_VPK_SHA
    assert identity(FROZEN / "knife-vita-game-270p.vpk")["sha256"] == OLD_VPK_SHA
    assert identity(FROZEN / "knife.elf")["sha256"] == OLD_ELF_SHA
    build_path = ROOT / "doc/vita-game-release-build.json"
    build = read(build_path)
    assert build["build_kind"] == "release" and build["title_id"] == "KNIF00270"
    assert build["frame_limit"] is None and not build["diagnostics_enabled"]
    assert build["save_namespace"] == "production" and build["physical_canvas"] == [480, 270]
    assert not build["hardware_verified"] and not build["runtime_verified"]
    for record in build["source_files"] + build["artifacts"]:
        checked(record)
    artifacts = {Path(a["path"]).name: a for a in build["artifacts"]}
    built_vpk = ROOT / artifacts["knife-vita-game-270p.vpk"]["path"]
    assert identity(built_vpk)["sha256"] != OLD_VPK_SHA
    with zipfile.ZipFile(built_vpk) as archive:
        assert archive.testzip() is None
        assert hashlib.sha256(archive.read("eboot.bin")).hexdigest() == artifacts["eboot.bin"]["sha256"]
        assert hashlib.sha256(archive.read("sce_sys/param.sfo")).hexdigest() == artifacts["param.sfo"]["sha256"]

    tests = []
    for name, variant in [("vita-gxm-copy-host-tests.json", None),
                          ("vita-gxm-viewport-host-tests.json", None),
                          ("vita-effects-inputs-host-tests.json", "green")]:
        path = ROOT / "doc" / name
        receipt = read(path)
        result = receipt["variants"][variant] if variant else receipt
        assert result["result"] == "PASS" and result["exit_code"] == result["failures"] == 0
        for record in receipt["files"]:
            checked(record)
        for source in ("gxm_backend.c", "effects_backend.inc"):
            snapshot = next(f for f in receipt["files"] if f["path"].endswith(
                ("/green/" if variant else "/backend/") + source))
            assert snapshot["sha256"] == identity(ROOT / "knife/vita" / source)["sha256"]
        if variant:
            controls = [v for k, v in receipt["variants"].items() if k != "green"]
            assert len(controls) == 5 and all(c["exit_code"] != 0 and c["failures"] > 0 for c in controls)
        tests.append({"receipt": identity(path), "status": "pass", "cases": result["cases"],
                      "checks": result["checks"], "failed": 0,
                      "scope": "Production C backend, host ASan/UBSan with mocked SDK; no hardware pixels or timing"})

    manifest = DIST / "assets/vita-270p-assets.json"
    assert checked(old["assets"]["manifest"]) == identity(manifest)
    assets = list((DIST / "assets").rglob("*"))
    assert sum(p.is_file() for p in assets) == old["assets"]["files"]
    assert sum(p.stat().st_size for p in assets if p.is_file()) == old["assets"]["bytes"]
    assert read(ROOT / "doc/vita-270p-assets-validation.json")["sha256"] == identity(manifest)["sha256"]

    destination = DIST / "knife-vita-270p.vpk"
    shutil.copyfile(built_vpk, destination)
    vpk = identity(destination)
    assert (vpk["bytes"], vpk["sha256"]) == (artifacts["knife-vita-game-270p.vpk"]["bytes"],
                                               artifacts["knife-vita-game-270p.vpk"]["sha256"])
    checks = []
    for tool, suffix, extra in [(ROOT / "tools/check-vita-package.py", "check", ["KNIF00270", "--release"]),
                                (Path("C:/Users/26241/.codex/skills/vita-port/scripts/check_vpk.py"),
                                 "independent-check", [])]:
        command = [sys.executable, "-X", "utf8", str(tool), str(destination), *extra]
        result = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        log_path = ROOT / f"doc/logs/vita/270p-vpk-{suffix}.log"
        log_path.write_bytes(result.stdout)
        print(result.stdout.decode("utf-8", errors="replace").strip())
        assert result.returncode == 0, command
        checks.append({"checker": identity(tool), "command": command, "exit_code": result.returncode,
                       "log": identity(log_path)})
    validation_path = ROOT / "doc/vita-270p-vpk-validation.json"
    write(validation_path, {"schema": "knife-vita-release-package-check-v1",
                           "verified_at": datetime.now(timezone.utc).isoformat(), "vpk": vpk,
                           "checks": checks, "hardware_verified": False})

    dump = read(FROZEN / "dump-inspection.json")
    expected_panic = ("creating Vita scene target: creating render target stage\n\nCaused by:\n"
                      "    GXM: effects render target setup: 0x805b0003 (0x805b0003)")
    assert dump["possible_error"] == expected_panic
    core = (FROZEN / "crash.core").read_bytes()
    assert expected_panic.encode() in core
    native = (FROZEN / "libgxm-text.bin").read_bytes()
    table = [struct.unpack_from("<I", native, 0x21AD0 + i * 4)[0] for i in (0, 1)]
    assert table == [0x5, 0x2A] and not (table[0] & 32) and table[1] & 32
    assert "udf" in (FROZEN / "knife-abort-trap.asm").read_text()
    fix_path = ROOT / "doc/vita-gxm-startup-crash-fix-20261028.json"
    write(fix_path, {
        "schema": "knife-vita-gxm-startup-crash-fix-v1", "created_at": datetime.now(timezone.utc).isoformat(),
        "generator": identity(__file__), "old_vpk": identity(FROZEN / "knife-vita-game-270p.vpk"),
        "old_elf": identity(FROZEN / "knife.elf"),
        "core": identity(FROZEN / "crash.core"), "dump_analysis": identity(FROZEN / "dump-inspection.json"),
        "panic": expected_panic, "error": "SCE_GXM_ERROR_INVALID_VALUE", "error_hex": "0x805b0003",
        "pc": {"runtime": "0x811f5266", "rx_runtime_base": "0x8100a000", "rx_elf_base": "0x81000000",
               "elf_address": "0x811eb266", "symbol": "_kill_r", "instruction": "udf #255",
               "meaning": "Intentional newlib abort trap after Rust panic, not evidence of unsupported ARM ISA"},
        "native_static_evidence": {"elf": identity(FROZEN / "libgxm_es4.elf"),
            "gamma_mode_address": "0x810194a4", "format_support_helper": "0x8100bc4c",
            "format_table": "0x81021ad0", "RGBA8_32bit_word": "0x5", "RGBA8_64bit_word": "0x2a",
            "gamma_support_bit": 5, "observed_from": "User-local decrypted SceGxm module, static inspection only",
            "disassemblies": [identity(FROZEN / n) for n in ["gxm-gamma-mode.asm", "gxm-gamma-format-helper.asm",
                                                               "gxm-color-surface.asm", "knife-abort-trap.asm"]]},
        "root_cause": "sRGB RGBA8 offscreen targets used 32-bit output registers, rejected by native gamma setup",
        "correction": {"srgb": {"register_size": "64BIT", "fragment_output": "HALF4"},
                       "linear_display": {"register_size": "32BIT", "fragment_output": "UCHAR4"},
                       "program_cache_key_includes_destination_mode": True,
                       "preserves_specific_target_setup_error": True,
                       "pixel_storage": "RGBA8", "aligned_480x270_bytes": 524288},
        "tests": tests, "build": identity(build_path), "vpk": vpk, "elf": artifacts["knife.elf"],
        "eboot": artifacts["eboot.bin"], "assets_unchanged": True, "assets_manifest": identity(manifest),
        "runtime_verified": False, "hardware_verified": False,
        "limitations": ["Native rule reproduced in host SDK doubles and supported by static native disassembly.",
                        "No execution of the fixed build on a physical Vita; startup, pixels and performance await retest."]})

    delivery = copy.deepcopy(old)
    delivery.update({"schema": "knife-vita-270p-delivery-v3", "generated_at": datetime.now(timezone.utc).isoformat(),
                     "generator": identity(__file__), "previous_delivery": identity(FROZEN / "vita-270p-delivery.json"),
                     "crash_fix": identity(fix_path), "tests": tests, "historical_tests": old["tests"],
                     "vpk_checks": [c["log"] for c in checks], "vpk_validation": identity(validation_path),
                     "finite_release_observations": [], "historical_release_observations": old["finite_release_observations"],
                     "runtime_verified": False, "hardware_verified": False,
                     "scope": "GXM startup correction built and host-tested; current VPK awaits hardware retest"})
    for run in delivery["historical_release_observations"]:
        checked(run["receipt"])
        assert run["vpk"]["sha256"] == OLD_VPK_SHA
        run["vpk"] = identity(FROZEN / "knife-vita-game-270p.vpk")
        run["current_build_observation"] = False
    delivery["engine"].update({"release": vpk, "build_receipt": identity(build_path),
        "selected_build_source_identities_rechecked": len(build["source_files"]),
        "build_artifacts_rechecked": len(build["artifacts"]), "fix": "native-sRGB-output-registers-20261028"})
    delivery["assets"]["modified_this_fix"] = False
    delivery["limits"][0] = "The previous release crashed on real hardware. This corrected build has no hardware execution or FPS validation yet."
    delivery["limits"].append("All earlier English/Chengkong emulator runs apply to the frozen old VPK, not this corrected VPK.")
    write(ROOT / "doc/vita-270p-delivery.json", delivery)
    print(json.dumps({"vpk": vpk, "tests": tests, "source_files_rechecked": len(build["source_files"]),
                      "hardware_verified": False}, ensure_ascii=False))


if __name__ == "__main__":
    main()

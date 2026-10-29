#!/usr/bin/env python3
"""Record already executed original-text tests and the independently built ARM ELF."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from datetime import datetime, timezone

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
REPO = ROOT / "knife"
LOGS = ROOT / "doc/logs/vita"
CACHE = Path(os.environ.get("CARGO_TARGET_DIR", "/root/.cache/knife-vita-text-agent"))
ELF = CACHE / "armv7-sony-vita-newlibeabihf/vita/vita-text-probe.elf"
ASSETS = Path(os.environ.get("KNIFE_TEXT_ASSETS", str(ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets")))
FONT_NAMES = [
    "en/HelveticaNeueLTGEO-55Roman.otf", "zc/arshanghaisonggbpro_lt.otf",
    "nintendo_udsg-r_std_003.bfttf", "nintendo_udsg-r_org_zh-cn_003.bfttf",
    "nintendo_udsg-r_ext_zh-cn_003.bfttf", "exfont_nx01.bffnt",
]


def artifact(path: Path, relative: bool = True) -> dict:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for data in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(data)
    return {"path": str(path.relative_to(ROOT)) if relative else str(path),
            "bytes": path.stat().st_size, "sha256": digest.hexdigest()}


def command(*args: str) -> str:
    result = subprocess.run(args, cwd=REPO, text=True, capture_output=True, check=True)
    return result.stdout.strip()


def readlog(name: str) -> str:
    return (LOGS / name).read_text(encoding="utf-8")


def exit_code(log: str) -> int:
    matches = re.findall(r"ACTUAL_CARGO_EXIT=(\d+)", log)
    assert len(matches) == 1, "each log must retain exactly one actual cargo exit code"
    return int(matches[0])


green = readlog("text-host-tdd-green.log")
red = readlog("text-host-tdd-red.log")
arm = readlog("text-arm-build.log")
assert exit_code(red) == 101 and "1 failed" in red
assert "TDD: actual original text.rs page construction is not connected yet" in red
assert exit_code(green) == 0 and "10 passed; 0 failed" in green and "1 filtered out" in green
assert exit_code(arm) == 0
assert artifact(ELF, False)["sha256"] in arm, "ELF must match the captured successful build"
identity = readlog("text-arm-elf-identity.log")
assert all(value in identity for value in ["ELF32", "ARM", "hard-float ABI", "v7"])
symbols = readlog("text-arm-symbols.log").splitlines()
required = ["knife_vita_text_probe::text::layout", "FontAtlas>::new", "FontAtlas>::load_exfont",
            "FontAtlas>::placed_quad", "FontAtlas>::rimmed_quad_edge", "FontAtlas>::add_fallback",
            "kv_gxm_texture_create_ex", "kv_gxm_texture_update", "kv_gxm_draw_effect", "kv_gxm_present_canvas"]
linked_symbols = {}
for name in required:
    linked_symbols[name] = [line for line in symbols if name in line]
    assert linked_symbols[name], f"required original-text/native symbol absent: {name}"
undefined = readlog("text-arm-undefined.log").splitlines()
assert all(re.match(r"\s+w\s+", line) for line in undefined), "unexpected strong undefined symbol"

# Verify the actual host facade snapshot against the current source rather than
# claiming a copied substitute implements the original facade.
snapshots = sorted(CACHE.glob("debug/build/knife-vita-text-probe-*/out/vita_facade_snapshot.rs"),
                   key=lambda path: path.stat().st_mtime)
snapshot = snapshots[-1]
original = (REPO / "knife-render/src/vita.rs").read_text(encoding="utf-8")
registration = '#[cfg(all(test, not(target_os = "vita")))]\n#[path = "vita_tests.rs"]\nmod tests;'
assert original.count(registration) == 1
expected = original.replace(registration, "// Upstream facade tests are registered in the root workspace.")
backend = str(REPO / "knife-render/src/vita_test_backend.rs")
expected = expected.replace('#[path = "vita_test_backend.rs"]',
                            f"#[path = {json.dumps(backend, ensure_ascii=False)}]")
assert snapshot.read_text(encoding="utf-8").startswith(expected), "host snapshot source changed after testing"

# Windows checkout CRLF and WSL Git defaults differ. Preserve raw input hashes
# below, but compare the original text module with HEAD after CRLF-to-LF only.
head_text = subprocess.check_output(["git", "show", "HEAD:knife/src/text.rs"], cwd=REPO).replace(b"\r\n", b"\n")
current_text = (REPO / "knife/src/text.rs").read_bytes().replace(b"\r\n", b"\n")
assert current_text == head_text, "production text module changed beyond checkout line endings"

source_paths = [
    REPO / "knife/src/text.rs", REPO / "knife/src/assets.rs", REPO / "knife/src/game.rs", REPO / "knife/src/ui.rs",
    REPO / "knife-render/src/vita.rs", REPO / "knife-render/src/vita_test_backend.rs",
    REPO / "knife-render-api/src/lib.rs", REPO / "knife-core/src/format/bffnt.rs",
    REPO / "knife-core/src/format/bfttf.rs", REPO / "knife-core/src/format/cff.rs",
    REPO / "knife-core/src/format/sfnt.rs", REPO / "knife-core/src/linebreak.rs",
    REPO / "knife/data/exe_tables_witch.bin",
]
probe_files = sorted(path for path in HERE.rglob("*") if path.is_file())
log_names = ["text-host-tdd-red.log", "text-host-tdd-green.log", "text-metrics-regression.log",
             "text-arm-build.log", "text-arm-elf-identity.log", "text-arm-symbols.log", "text-arm-undefined.log"]
layout_path = ROOT / "doc/vita-text-layout-reference.json"
layout = json.loads(layout_path.read_text(encoding="utf-8"))
assert len(layout["pages"]) == 2
pages = []
for page in layout["pages"]:
    page_summary = {key: page[key] for key in ["language", "placed", "quads", "text_draws", "rim_text_draws",
                                             "message_metrics", "system_metrics", "format", "atlas_bytes"]}
    page_summary["atlas_partial_updates"] = len(page["atlas_updates"])
    page_summary["uploaded_message_glyph_bytes"] = sum(u["size"][0] * u["size"][1] for u in page["atlas_updates"])
    pages.append(page_summary)

receipt = {
    "schema": "knife-vita-original-text-verification-v1",
    "recorded_at_utc": datetime.now(timezone.utc).isoformat(),
    "status": "host-tests-and-arm-link-pass; visible-text-runtime-unverified",
    "source_reuse": {
        "text_module": "knife/knife/src/text.rs",
        "mechanism": "src/lib.rs path includes the original file directly, without a copied text engine",
        "host_facade": "exact current vita.rs functional body; module paths/test registration and recorder introspection only",
        "snapshot": artifact(snapshot, False),
        "production_text_diff": command("git", "diff", "--ignore-space-at-eol", "--", "knife/src/text.rs"),
        "production_text_head_comparison": {
            "normalization": "CRLF to LF only",
            "equal_to_head": True,
            "normalized_sha256": hashlib.sha256(current_text).hexdigest(),
        },
        "sources": [artifact(path) for path in source_paths],
    },
    "inputs": [artifact(ASSETS / name) for name in FONT_NAMES],
    "input_bound": {"per_font_minimum_bytes": 1, "per_font_maximum_bytes": 8 * 1024 * 1024,
                    "read_length_and_metadata_rechecked": True, "missing_input_is_failure": True},
    "tdd": {"red": {"tests": 1, "passed": 0, "failed": 1, "actual_cargo_exit": exit_code(red),
                      "cause": "the test required real original FontAtlas/layout while page construction was not connected"},
            "green": {"tests": 10, "passed": 10, "failed": 0, "filtered": 1, "actual_cargo_exit": exit_code(green)},
            "additional_regression": "first-shared-font metric selection test passed before the source-selection alignment; no failing red is claimed for that check",
            "filtered_test": {"name": "text::tests::cff_glyphs_after_endchar_outline",
                              "reason": "upstream test depends on an old romfs path and silently skips absent inputs; mandatory real-compact CFF coverage replaces it here"},
            "negative_control": {"test": "r8_glyph_partial_upload_matches_real_coverage_and_reuses_cache",
                                 "mutation": "one actual nonzero glyph atlas texel XOR 1",
                                 "expected": "coverage comparison returns error", "observed": "passing explicit rejection assertion"}},
    "host_pages": pages,
    "layout_reference": artifact(layout_path),
    "canvas": {"logical": [1920, 1080], "physical": [960, 540],
               "glyph_atlas": [2048, 2048], "glyph_format": "R8 non-sRGB, linear sampler"},
    "arm": {"actual_cargo_exit": exit_code(arm), "elf": artifact(ELF, False),
            "target": "armv7-sony-vita-newlibeabihf", "toolchain": "nightly-2026-05-28",
            "rustc": "1.98.0-nightly 57d06900fd7d9ee06d3a7f323bb77f17ab3cfaf8 2026-05-27",
            "vita_gcc": "15.2.0", "identity": "ELF32 ARM v7 little-endian EABI5 hard-float",
            "required_linked_symbols": linked_symbols, "weak_undefined_symbols": undefined,
            "strong_undefined_symbols": [], "cache": str(CACHE)},
    "commands": {
        "host": "env PATH=/opt/vitasdk/bin:/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin bash knife/probes/vita-text/test-host.sh green",
        "host_cargo": "cargo +1.99.0 test --offline --manifest-path knife/probes/vita-text/Cargo.toml --lib -- --nocapture --skip text::tests::cff_glyphs_after_endchar_outline",
        "arm": "env PATH=/opt/vitasdk/bin:/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin bash knife/probes/vita-text/build-arm.sh",
        "arm_cargo": "cargo +nightly-2026-05-28 build -Z build-std=std,panic_abort --locked --offline --manifest-path knife/probes/vita-text/Cargo.toml --profile vita --target armv7-sony-vita-newlibeabihf --bin vita-text-probe",
    },
    "dev_runtime_contract": {"title_id": "KNIF00008", "binary": "vita-text-probe",
                             "font_root": "ux0:data/knife-woh-dev/text-assets",
                             "log": "ux0:data/knife-woh-dev/text-probe.log",
                             "stages": ["EN", "ZC"], "frame_ready_hold_seconds_per_language": 4,
                             "markers": ["STAGE {EN|ZC} BEGIN", "STAGE {EN|ZC} FRAME READY", "STAGE {EN|ZC} HOLD COMPLETE"],
                             "result_pass_scope": "mandatory input/font/layout/upload/native submission; screenshot visibility remains independent"},
    "evidence_logs": [artifact(LOGS / name) for name in log_names],
    "probe_files": [artifact(path) for path in probe_files],
    "git": {"branch": command("git", "branch", "--show-current"), "head": command("git", "rev-parse", "HEAD"),
            "working_tree_state": command("git", "-c", "core.autocrlf=true", "status", "--short", "--branch"),
            "working_tree_eol_policy": "core.autocrlf=true for the shared Windows checkout",
            "shared_lock_current": artifact(REPO / "Cargo.lock"),
            "task_changes": "only isolated knife/probes/vita-text and doc text receipts/logs; production text.rs unchanged",
            "commit_or_remote_write": "none by this task; remote state was not queried"},
    "limitations": [
        "Host strict recorder checks texture bytes and native draw arguments; it never executes Text/RimText shaders.",
        "Actual Vita runtime, screenshots, final blend/edge appearance, hardware performance and peak memory are unverified by this task.",
        "Existing runtime 2048x2048 R8 glyph cache is retained; it is not an HD game image package.",
        "Malformed-font checks cover the listed bounded negative cases; they are not an exhaustive font/parser safety audit.",
        "Resource lifetime checks prove recorder/deferred-command ownership; actual GPU retirement timing needs runtime evidence.",
        "The native platform/backend is shared with the parent task and may advance independently; the recorded ELF digest identifies this build.",
        "No full game UI/script/audio integration, VPK packaging, emulator launch or real-console run was performed by this task.",
    ],
}
out = ROOT / "doc/vita-text-verification.json"
out.write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(json.dumps({"receipt": str(out), "status": receipt["status"], "host_passed": 10,
                  "arm_cargo_exit": 0, "elf_sha256": receipt["arm"]["elf"]["sha256"],
                  "font_bytes": sum(item["bytes"] for item in receipt["inputs"])}, ensure_ascii=False))

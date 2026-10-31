#!/usr/bin/env python3
"""Record finite host/ARM evidence; this script never executes the Vita ELF."""
import datetime
import hashlib
import json
import pathlib
import os
import re
import subprocess

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
REPO = ROOT / "knife"
LOGS = ROOT / "doc/logs/vita"
CACHE = pathlib.Path(os.environ.get("CARGO_TARGET_DIR") or str(pathlib.Path(os.environ.get("XDG_CACHE_HOME") or pathlib.Path.home() / ".cache") / "knife-vita-movie-agent")).expanduser()
ELF = CACHE / "armv7-sony-vita-newlibeabihf/vita/vita-movie-probe.elf"
OFFLINE = " --offline" if os.environ.get("KNIFE_VITA_OFFLINE") == "1" else ""
SHARED_LOCK_BEFORE = "472a8049c030f86ef6a087d1efcfe36e1f46d433783efc751ae6780b7f830e49"


def identity(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return {"path": str(path), "bytes": path.stat().st_size, "sha256": digest.hexdigest()}


def git(*args):
    return subprocess.check_output(["git", "-C", str(REPO), *args], text=True).strip()


def actual_exit(path):
    matches = re.findall(r"^ACTUAL_CARGO_EXIT=(\d+)$", path.read_text(), re.M)
    if len(matches) != 1:
        raise RuntimeError(f"No unambiguous actual Cargo exit in {path}")
    return int(matches[0])


red = LOGS / "movie-probe-empty-red.log"
host = LOGS / "movie-probe-host-tests.log"
arm = LOGS / "movie-probe-arm-build.log"
elf_header = LOGS / "movie-probe-arm-elf-identity.log"
symbols = LOGS / "movie-probe-arm-symbols.log"
undefined = LOGS / "movie-probe-arm-undefined.log"
backend_receipt = ROOT / "doc/vita-movie-backend-verification.json"
assert actual_exit(red) == 101
assert actual_exit(host) == 0 and actual_exit(arm) == 0
assert "test result: ok. 8 passed; 0 failed; 0 ignored" in host.read_text()
assert "Machine:                           ARM" in elf_header.read_text()
strong_undefined = [line for line in undefined.read_text().splitlines()
                    if line.strip() and line.split()[0] not in ("w", "v")]
assert not strong_undefined, strong_undefined
elf = identity(ELF)
assert elf["sha256"] == "051237b25737260aaa902519794116164be563ea235ce2ee7f48c11d390b0108"

asset_root = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets"
asset_specs = [
    ("movie/logo.mp4", 336200, "34137eac81996def746518c7a00e4612cd41c63809a4d90c11fbfc42cfd86a84", True, 10453),
    ("movie/opening_en.mp4", 15826531, "6bc4686fb0e76d13542ee36d2d288c62e38e17d6128dccf4f49e33f4c98554c6", False, 197300),
    ("movie/opening_zc.mp4", 16279483, "11b8201ad1fcbc6242e6470ed63ac52f5096b281d4834fcff912c9cac0f50702", False, 197300),
]
assets = []
for relative, size, sha, audio, duration in asset_specs:
    item = identity(asset_root / relative)
    assert item["bytes"] == size and item["sha256"] == sha
    item.update(relative_path=relative, has_audio=audio, duration_ms=duration,
                stream_authority="prior ffprobe observation recorded in backend receipt; encoded identity rehashed now",
                runtime_sha256="not computed by probe; expected host identity logged with HOST_RECEIPT authority")
    assets.append(item)

shared_lock = identity(REPO / "Cargo.lock")
assert shared_lock["sha256"] == SHARED_LOCK_BEFORE
source_paths = ["Cargo.toml", "Cargo.lock", "src/lib.rs", "src/bin/vita-movie-probe.rs",
                "tests/contract.rs", "test-host.sh", "build-arm.sh", "README.md", "write-receipt.py"]
observed_links = ["knife-platform-vita/src/movie.rs", "knife-platform-vita/src/gxm.rs",
                  "knife-platform-vita/src/lib.rs", "knife-platform-vita/build.rs",
                  "vita/movie_player.c", "vita/movie_player.h", "vita/CMakeLists.txt"]
receipt = {
    "schema": "knife-vita-three-mp4-native-probe-v1",
    "recorded_at_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
    "status": "host_contract_and_arm_final_link_passed_runtime_not_run",
    "scope": "Standalone DEV binary exercising the production native AvPlayer owner and existing GXM movie texture bridge on exactly three staged current MP4 inputs.",
    "git": {"branch": git("branch", "--show-current"), "head": git("rev-parse", "HEAD"),
            "scoped_status": git("status", "--short", "--", "probes/vita-movie").splitlines(),
            "shared_lock": shared_lock, "shared_lock_unchanged_from_before": True,
            "commit_created": False, "remote_write_performed": False},
    "host_tests": {
        "command": "bash knife/probes/vita-movie/test-host.sh",
        "cargo_command": f"cargo +1.99.0 test{OFFLINE} --manifest-path knife/probes/vita-movie/Cargo.toml",
        "passed": 8, "failed": 0, "ignored": 0, "actual_cargo_exit": actual_exit(host),
        "fixtures": "small synthetic observation records only, never decoded-video substitutes",
        "negative_controls": ["empty observation", "premature/missing EOF", "missing/short/silent/invalid PCM",
                              "wrong dimensions/aspect", "nonmonotonic/out-of-range timestamps",
                              "duplicate-only video", "uncompleted GPU submissions", "broken pause/resume/stop order"],
        "tdd_red": {"actual_cargo_exit": actual_exit(red), "failed": 1,
                    "test": "empty_native_observation_cannot_pass", "log": identity(red)}},
    "arm_final_link": {
        "command": "bash knife/probes/vita-movie/build-arm.sh",
        "cargo_command": f"cargo +nightly-2026-05-28 build -Z build-std=std,panic_abort --locked{OFFLINE} --manifest-path knife/probes/vita-movie/Cargo.toml --profile vita --target armv7-sony-vita-newlibeabihf --bin vita-movie-probe",
        "actual_cargo_exit": actual_exit(arm), "elf": elf,
        "architecture": "ELF32 little-endian ARMv7-A, Thumb-2, VFPv3 hard-float EABI5",
        "strong_undefined_symbols": strong_undefined,
        "weak_runtime_undefined_symbols": [line.strip() for line in undefined.read_text().splitlines() if line.strip()],
        "warnings": ["two existing unused GXM DEV swap-target diagnostic items"],
        "independent_target_dir": str(CACHE)},
    "runtime_contract": {
        "title_id": "KNIF00011", "binary": "vita-movie-probe",
        "input_root": "ux0:data/knife-woh/assets",
        "log_path": "ux0:data/knife-woh-dev/movie-probe.log",
        "log_publication": "each recorded line flush closes and reopens the fixed guest path",
        "frame_ready": "ASSET <LOGO|OPENING_EN|OPENING_ZC> FRAME READY timestamp_ms=<actual> gpu_fence=COMPLETE",
        "result_markers": ["RESULT PASS", "RESULT FAIL"],
        "runner_timeout_seconds_suggested": 60,
        "logo": "actual advancing 960x540 video; >=2 completed GXM submissions; substantial nonzero finite PCM at 48k; near-total video/PCM duration; natural inactive/EOF; checked texture retirement and close",
        "openings": "actual advancing video for >=2.5 seconds; video-only stream expectation; pause/150ms clock hold/resume; stop and close; exact rejected seek preserved separately",
        "gpu_lifetime": "SDK allocation remains borrowed through native YVU420P2_CSC1 draw/present/finish; every next pull/control/close follows completion; no CPU stride assumption",
        "audio_bound": "one reusable 960-frame stereo float buffer; <=4 pulls per loop, paced against 48k wall clock; PCM measured and discarded",
        "failure_ownership": "failed GPU fence or texture retirement retains the bounded native owner; failed native close stays an error",
        "seek": "unsupported/native rejection reported with exact error and supported_seek=UNVERIFIED; accepted seek must expose actual target clock/frame",
        "runtime_executed": False, "vpk_packaged_by_this_subtask": False,
        "unverified": ["actual native decoding", "visible pixels", "hardware audio output", "audible correctness",
                       "complete openings", "successful native seek", "native hardware equivalence",
                       "public inactive state cannot distinguish every late asynchronous error from EOF"],
        "known_emulator_risk": {"commit": "1a9ad6aa1c73468012cd1e3c23741b6810b67614",
                                "evidence": "codec/src/player.cpp:219 assigns FFmpeg best_effort_timestamp directly to last_timestamp; SceAvPlayer GetVideoData/CurrentTime return it without observed time_base-to-millisecond conversion",
                                "probe_behavior": "strict timestamp/range/duration checks fail rather than silently reinterpret unknown units"}},
    "inputs": assets,
    "backend_receipt": identity(backend_receipt),
    "probe_sources": [identity(HERE / path) for path in source_paths],
    "production_link_inputs_observed_after_build": [identity(REPO / path) for path in observed_links],
    "concurrency_limitation": "Production input hashes are observations at receipt generation after the final build; other agents work concurrently. Exact final ELF digest is authoritative for the artifact to run.",
    "logs": [identity(path) for path in (host, arm, elf_header, symbols, undefined)],
}
output = ROOT / "doc/vita-movie-probe-verification.json"
output.write_text(json.dumps(receipt, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
print(json.dumps({"receipt": identity(output), "elf": elf, "host_passed": 8,
                  "host_actual_exit": actual_exit(host), "arm_actual_exit": actual_exit(arm),
                  "strong_undefined_count": len(strong_undefined), "runtime_executed": False}, indent=2))

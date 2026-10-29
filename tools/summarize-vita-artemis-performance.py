"""Verify retained local observations and write the bounded Artemis review.

Offline only: no guest execution, SDK invocation, remote access or old receipt
mutation. A valid freeze is artifact identity evidence, not build attestation.
"""
import hashlib
import json
import re
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BLOCK = re.compile(r"block: (allocated|after free): uid=(\d+) address=0x[\da-f]+ bytes=(\d+) mapping=\d+ cdram=([01])")
TEXTURE = re.compile(r"texture: created uid=(\d+) width=(\d+) height=(\d+) bpp=(\d+) target=(\d+)")


def path(value):
    return Path(value) if Path(value).is_absolute() else ROOT / value.replace("\\", "/")


def read_json(value):
    return json.loads(path(value).read_text(encoding="utf-8-sig"))


def identity(value):
    p = path(value)
    data = p.read_bytes()
    try:
        name = p.relative_to(ROOT).as_posix()
    except ValueError:
        name = p.as_posix()
    return {"path": name, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def check_identity(value):
    actual = identity(value["path"])
    assert (actual["bytes"], actual["sha256"]) == (value["bytes"], value["sha256"]), value["path"]
    return actual


def ledger(text):
    live, ever, textures = {}, set(), {}
    pools = {name: {"allocations": 0, "frees": 0, "peak_bytes": 0, "end_bytes": 0}
             for name in ("CDRAM", "non_CDRAM")}
    for line in text.splitlines():
        if m := BLOCK.search(line):
            op, uid, size, cdram = m.groups()
            uid, size = int(uid), int(size)
            name = "CDRAM" if cdram == "1" else "non_CDRAM"
            pool = pools[name]
            if op == "allocated":
                assert uid not in ever and size > 0, line
                ever.add(uid)
                live[uid] = {"uid": uid, "bytes": size, "pool": name}
                pool["allocations"] += 1
                pool["end_bytes"] += size
                pool["peak_bytes"] = max(pool["peak_bytes"], pool["end_bytes"])
            else:
                block = live.pop(uid)
                assert (block["pool"], block["bytes"]) == (name, size), line
                pool["frees"] += 1
                pool["end_bytes"] -= size
        if m := TEXTURE.search(line):
            uid, w, h, bpp, target = map(int, m.groups())
            textures[uid] = {"width": w, "height": h, "bpp": bpp, "target": bool(target)}
    groups = {}
    for uid, block in live.items():
        meta = textures.get(uid)
        label = json.dumps(meta, sort_keys=True) if meta else "texture_metadata_unavailable"
        key = (block["pool"], label, block["bytes"])
        group = groups.setdefault(key, {"pool": block["pool"], "texture": meta,
                                       "bytes_per_block": block["bytes"], "uids": [], "total_bytes": 0})
        group["uids"].append(uid)
        group["total_bytes"] += block["bytes"]
    for name, pool in pools.items():
        pool["end_blocks"] = sum(b["pool"] == name for b in live.values())
    return {"pools": pools, "end_groups": list(groups.values()),
            "scope": "Explicit native block logs only; separate pool peaks may occur at different times. End blocks after panic do not prove a leak.",
            "label_limitation": "Texture dimensions/flags do not bind a UID to a Rust field; Alter outputs also have target=true."}


def retained_run(name):
    manifest_path = f".work/vita/game-investigation/{name}/manifest.json"
    manifest = read_json(manifest_path)
    for entry in manifest["entries"]:
        check_identity({**entry, "path": entry["retained"]})
        # Historic runs bind their retained copies, not a later working tree.
        # Selected frozen sources still do not attest exhaustive build inputs.
    receipt = read_json(manifest["entries"][0]["retained"])
    build = read_json(manifest["entries"][1]["retained"])
    expected_sha = manifest["guest_vpk_sha256"]
    assert receipt["vpk_sha256"] == expected_sha
    vpk = next(a for a in build["artifacts"] if a["path"].endswith(".vpk"))
    assert vpk["sha256"] == expected_sha
    frozen_vpk = next(e for e in manifest["entries"]
                      if e["source"].replace("\\", "/") == vpk["path"].replace("\\", "/"))
    retained_vpk = check_identity({**vpk, "path": frozen_vpk["retained"]})
    vpk = {**retained_vpk, "original_path": vpk["path"]}
    for key in ("probe_log", "native_log"):
        data = receipt[key].encode("utf-8")
        assert hashlib.sha256(data).hexdigest() == receipt[key + "_sha256"]
        assert path(f".work/vita/game-investigation/{name}/{key}.log").read_bytes() == data
    states = [json.loads(line[6:]) for line in receipt["probe_log"].splitlines() if line.startswith("STATE ")]
    code = receipt["exit"]["code"]
    uploads = Counter(re.findall(r"GXT_UPLOAD label=(\S+)", receipt["probe_log"]))
    return {"manifest": identity(manifest_path), "retained_files": len(manifest["entries"]),
            "retained_bytes": sum(e["bytes"] for e in manifest["entries"]),
            "runtime_receipt": identity(manifest["entries"][0]["retained"]),
            "tested_at": receipt["tested_at"], "vpk": vpk,
            "probe_verified": receipt["probe_verified"],
            "input_valid": receipt["game_input_validation"]["valid"],
            "runtime_verified": receipt["runtime_verified"], "hardware_verified": receipt["hardware_verified"],
            "natural_stop": receipt["natural_stop"],
            "host_exit": {"code": code, "hex": f"0x{code:08X}", "valid": receipt["exit_validation"]["valid"]},
            "last_state": states[-1], "save_file_observations": receipt["game_input_validation"]["captures"],
            "gxt_upload_counts": dict(uploads), "memory": ledger(receipt["native_log"]),
            "probe_log_sha256": receipt["probe_log_sha256"], "native_log_sha256": receipt["native_log_sha256"],
            "asset_stage": receipt["game_assets_override"]["stage_manifest"],
            "error": receipt.get("error"), "guest_pass": "RESULT PASS" in receipt["probe_log"],
            "panic": receipt["probe_log"].split("PANIC ", 1)[1] if "PANIC " in receipt["probe_log"] else None}


def main():
    extraction = read_json(".work/vita/artemis-pcsg01127/extraction-manifest.json")
    official = {key: check_identity(extraction[key]) for key in ("reference", "output")}
    official["extraction_manifest"] = identity(".work/vita/artemis-pcsg01127/extraction-manifest.json")
    official["report"] = identity(".work/vita/artemis-pcsg01127/analysis/report.json")
    official["limitations"] = read_json(official["report"]["path"])["limitations"]
    official["primary_source_snapshots"] = [identity(f".work/vita/artemis-pcsg01127/{name}") for name in
                                            ("vita-toolchain-self.h", "Vita3K-load_self.cpp", "SceGxm.yml")]
    observations = [
        ("queue_pending_front_only", "animation-analysis", "812ebb0c", "Pending front returns immediately; completed front is destroyed and next front receives the same time argument recursively. No whole-engine fixed-capacity claim."),
        ("queue_lazy_allocation", "animation-analysis", "811a8fbc", "Queue lazy construction calls operator new(0x18); enqueue growth and global allocator policy remain unknown."),
        ("tween_step", "animation-analysis", "8121d478", "Delay/duration/value/completion state visible; time units and all interpolation branches remain unresolved."),
        ("tween_cancel", "animation-analysis", "8121eaec", "Marks done."),
        ("tween_finish", "animation-analysis", "8121eaf8", "Applies terminal value and possible completion callback."),
        ("target_reuse", "analysis", "8102edc4", "Matching width/height/format/flags returns; differing target configuration rebuilds."),
        ("begin_finish", "analysis", "8102e1a0", "Calls sceGxmFinish at 0x8102e1b0 before scene begin."),
        ("switch_scene", "analysis", "8102e3d4", "Ends an open scene before virtual begin."),
        ("display_end", "analysis", "8102e250", "EndScene and display queue; back index modulo three denotes display buffers."),
        ("renderer_init", "analysis", "8102da50", "Initializes ring/context resources and three 0x220000 display blocks, 960x544 pitch1024."),
        ("clear_reuses_geometry", "analysis", "8102e37c", "Uses existing shader and triangle geometry."),
        ("pool_allocate", "animation-analysis", "8102e7e0", "64-byte alignment, first-fit/splitting in observed pool; pool total and global LRU unknown."),
        ("pool_free", "animation-analysis", "8123a8e0", "Finds allocation and coalesces adjacent free regions."),
    ]
    official["static_observations"] = [{"id": label, "address": "0x" + addr,
                                        "evidence": identity(f".work/vita/artemis-pcsg01127/{folder}/{addr}.txt"),
                                        "observation": detail} for label, folder, addr, detail in observations]
    save = retained_run("artemis-frame-admission-save-load-7200")
    backlog = retained_run("artemis-frame-admission-backlog-still-oom-7200")
    assert save["guest_pass"] and save["input_valid"] and save["last_state"]["frame"] == 7200
    assert save["host_exit"]["hex"] == "0xC0000374" and not save["runtime_verified"]
    assert save["memory"]["pools"]["CDRAM"] == {"allocations": 105, "frees": 105, "peak_bytes": 50331648, "end_bytes": 0, "end_blocks": 0}
    assert save["memory"]["pools"]["non_CDRAM"]["end_blocks"] == 0
    assert not backlog["guest_pass"] and not backlog["input_valid"]
    assert "peak 4194304 > available 2883584" in backlog["panic"]
    assert backlog["host_exit"]["hex"] == "0xC0000005"
    assert backlog["memory"]["pools"]["CDRAM"]["end_bytes"] == 47448064
    tests = []
    for name, count, marker in [
        ("artemis-cache-host-tests.log", 52, "test result: ok. 52 passed; 0 failed"),
        ("artemis-image-cache-host-tests.log", 14, "test result: ok. 14 passed; 0 failed"),
        ("game-backlog-host-tests.log", 55, "ℹ pass 55"),
    ]:
        p = f".work/vita/{name}"
        text = path(p).read_text(encoding="utf-8-sig")
        assert marker in text, p
        if count == 55:
            assert "ℹ fail 0" in text
        tests.append({"evidence": identity(p), "passed": count, "failed": 0, "scope": "Host logic only; no hardware timing or GPU pixel claim."})
    build = read_json("doc/vita-game-build.json")
    for artifact in build["artifacts"]:
        check_identity(artifact)
    result = {
        "schema": "knife-vita-artemis-performance-review-v1",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "generator": identity(__file__),
        "scope": "Static official-reference analysis, bounded local changes and frozen functional regressions. Offline identity verification only.",
        "official": official, "tests": tests, "build": {"receipt": identity("doc/vita-game-build.json"), **build},
        "runs": {"save_load": save, "backlog": backlog},
        "budgets": {"policy_CDRAM_bytes": 67108864, "application_explicit_block_limit_bytes": 50331648,
                    "policy_reserve_bytes": 16777216, "deferred_resident_limit_bytes": 33554432,
                    "changed_in_this_review": False},
        "source_working_set_audit": [
            {"owner": "Game.stage_rt/scene_rt/old_rt/canvas", "source": "knife/knife/src/game.rs", "count": 4, "aligned_bytes": 8388608, "lifetime": "Eager Game lifetime; snapshot/transition semantics require separate audit before aliasing."},
            {"owner": "Game.group_rt", "source": "knife/knife/src/game.rs", "count": 3, "aligned_bytes": 6291456, "lifetime": "Lazy creation, retained after first group use; inspect inactive-phase release and pass overlap."},
            {"owner": "Game.menu_rt/menu_quarter", "source": "knife/knife/src/game.rs", "aligned_bytes": 2621440, "lifetime": "Lazy blur capture, retained after close; preserve captured contents while menu/backlog uses them."},
            {"owner": "WUi.backdrop_rt", "source": "knife/knife/src/ui.rs", "count": 2, "aligned_bytes": 4194304, "lifetime": "Shared menu/save pair; now released outside owning UI phase before Game render."},
        ],
        "follow_up_order": ["Establish phase/pass ownership and reserve entry peaks before additional LRU changes.",
                            "Audit inactive group, transition, blur and captured targets; alias only with proven nonoverlap and GPU completion.",
                            "Measure stage/script, text layout, command construction, I/O/decode/upload and GPU waits on real Vita at recorded normal clocks.",
                            "Use bounded asynchronous requests/results/backpressure; inspect cancellation and avoid cache eviction/reload oscillation.",
                            "Choose DXT1/DXT5/RGBA/R8/PVRTC per class from hardware timing and alpha/palette quality checks."],
        "unresolved": ["Backlog first open still exceeds native admission headroom: 4 MiB peak versus 2.75 MiB available.",
                       "Backlog host C0000005 and save shutdown C0000374 remain failures with distinct observations.",
                       "Game background/blur pixel comparison, more scenes, repeated UI cycles, movie/audio and hardware acceptance remain open.",
                       "Current Stage uses 40x6 inline tween slots; no generic rewrite is justified without semantic/state and hardware timing evidence.",
                       "Frame command Vec/HashSet allocations and catch-up work still require profiling; fast path is not an allocation-free-frame claim.",
                       "Native target creation before presentation is outside the new missing-texture admission callback.",
                       "codec8 CPU grade/raw/strip remains explicitly unsupported with no sidecar or HD fallback.",
                       "Selected frozen source identity is not exhaustive source-to-binary attestation."],
        "runtime_verified": False, "hardware_verified": False, "performance_verified": False,
        "version_control": {"branch": "work/woh-540p-chengkong", "head": "8de7f3b147e1c430fdfdc66af0ced7e79f8ab642", "committed_this_review": False, "pushed": False, "remote_refs_queried": False},
    }
    output = ROOT / "doc/vita-artemis-performance-review.json"
    output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"output": str(output), "official_entries": len(observations), "host_passes": [x["passed"] for x in tests],
                      "vpk_sha256": next(a["sha256"] for a in build["artifacts"] if a["path"].endswith(".vpk")),
                      "save_guest_pass": save["guest_pass"], "backlog_guest_pass": backlog["guest_pass"],
                      "runtime_verified": False, "hardware_verified": False}, ensure_ascii=False))


if __name__ == "__main__":
    main()

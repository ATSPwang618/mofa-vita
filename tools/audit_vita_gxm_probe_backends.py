"""Freeze existing DEV probe receipts and compare retained host-captured RGB.

This does not launch an emulator, change a VPK, or establish hardware behavior.
It deliberately keeps pixel observations separate from guest and host exit status.
"""
import argparse
import collections
import hashlib
import importlib.util
import json
import shutil
from datetime import datetime, timezone
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("gxm_frame_reference", ROOT / "tools/verify-vita-gxm-frame.py")
oracle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oracle)


def identity(path):
    path = Path(path).resolve()
    data = path.read_bytes()
    return dict(path=str(path), bytes=len(data), sha256=hashlib.sha256(data).hexdigest())


def check_region(image, name, rect, expected):
    pixels = image.crop(rect)
    result = oracle.compare(pixels.tobytes(), bytes(expected) * (pixels.width * pixels.height), 3, 2)
    # The full-frame oracle reports coordinates/counts for its 960x544 canvas.
    # Rebase that metadata for a crop; channel/pixel deltas remain unchanged.
    result["compared_pixels"] = pixels.width * pixels.height
    if first := result["first_mismatch"]:
        linear = first["y"] * 960 + first["x"]
        first["x"], first["y"] = rect[0] + linear % pixels.width, rect[1] + linear // pixels.width
    return dict(name=name, rect=list(rect), expected_rgb=list(expected), **result)


def inspect_frame(mode, stage, path):
    with Image.open(path) as source:
        image = source.convert("RGB")
    if image.size != (960, 544):
        raise ValueError(f"Unexpected capture size: {path}")
    histogram = collections.Counter(image.getdata())
    result = dict(artifact=identity(path), stage=stage, dimensions=list(image.size),
                  nonblack_rgb_pixels=image.width * image.height - histogram.get((0, 0, 0), 0),
                  dominant_colors=[dict(rgb=list(rgb), pixels=count) for rgb, count in histogram.most_common(9)])
    if mode == "gxm":
        expected, fixtures = oracle.reference(2)
        result["rgb_reference"] = oracle.compare(image.tobytes(), expected.convert("RGB").tobytes(), 3, 2)
        result["reference_fixtures"] = fixtures
    elif stage == "screen_capture":
        # Five ungraded patch interiors; Present's color transformation is not
        # silently treated as an identity operation or given a relaxed tolerance.
        names = ["basic/source", "effect/fill", "effect/sprite", "basic/RT-clear", "basic/RT-fill"]
        colors = [(128, 64, 200), (204, 51, 25), (128, 64, 200), (64, 128, 191), (204, 51, 25)]
        result["regions"] = [check_region(image, name, (x + 8, 30, x + 92, 114), color)
                             for x, name, color in zip([24, 140, 256, 372, 488], names, colors)]
        result["unverified_patches"] = ["effect/Present RT-clear", "effect/Present RT-fill"]
    else:
        y0, y1 = (2, 542) if stage == "crossover_capture" else (0, 544)
        result["regions"] = [check_region(image, "left basic blue", (0, y0, 480, y1), (64, 128, 191)),
                             check_region(image, "right effect red", (480, y0, 960, y1), (204, 51, 25))]
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snapshot-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        raise FileExistsError(args.output)
    args.snapshot_dir.mkdir(exist_ok=False)
    runs = []
    for mode in ("gxm", "pipeline"):
        build_path = ROOT / f"doc/vita-{mode}-build.json"
        build = json.loads(build_path.read_text(encoding="utf-8"))
        shutil.copyfile(build_path, args.snapshot_dir / build_path.name)
        vpk_path = ROOT / f".work/vita/{mode}/knife-vita-{mode}-dev.vpk"
        vpk = identity(vpk_path)
        if not any(a["sha256"] == vpk["sha256"] and a["bytes"] == vpk["bytes"] for a in build["artifacts"]):
            raise ValueError(f"VPK does not match build receipt: {mode}")
        shutil.copyfile(vpk_path, args.snapshot_dir / vpk_path.name)
        for backend in ("opengl", "vulkan"):
            source_path = ROOT / f"doc/vita-{mode}-{backend}-disabled-runtime.json"
            data = json.loads(source_path.read_text(encoding="utf-8"))
            frozen = args.snapshot_dir / source_path.name
            shutil.copyfile(source_path, frozen)
            if data["vpk_sha256"] != vpk["sha256"] or not data["renderer_observation"]["matches_request"]:
                raise ValueError(f"Mismatched inputs/backend in {source_path}")
            host = data["host_debugger"]
            for name in ("executable", "target"):
                observed = identity(host["executable"] if name == "executable" else data["executable"])
                if any(observed[k] != host[f"{name}_identity"][k] for k in ("bytes", "sha256")):
                    raise ValueError(f"Host tool changed: {name}")
            if hashlib.sha256(data["probe_log"].encode("utf-8")).hexdigest() != data["probe_log_sha256"]:
                raise ValueError("Probe log digest mismatch")
            runs.append(dict(mode=mode, host_backend=backend, receipt=identity(frozen),
                             original_receipt=identity(source_path), tested_at=data["tested_at"], vpk=vpk,
                             build_receipt=identity(args.snapshot_dir / build_path.name),
                             emulator_sha256=host["target_identity"]["sha256"],
                             debugger_sha256=host["executable_identity"]["sha256"],
                             mapping=data["config_overrides"]["memory-mapping"],
                             renderer_observation=data["renderer_observation"],
                             guest_probe_verified=data["probe_verified"],
                             original_runtime_verified=data["runtime_verified"],
                             child_exit=host.get("child_exit"),
                             frames=[inspect_frame(mode, stage, data[stage]["path"])
                                     for stage in ("screen_capture", "crossover_capture", "scanout_capture") if stage in data]))
    report = dict(schema="knife-vita-gxm-probe-backend-audit-v1", generated_at=datetime.now(timezone.utc).isoformat(),
                  tool=identity(__file__), reference_tool=identity(oracle.__file__), guest_api="direct GXM",
                  same_vpk_within_each_probe=all(len({r["vpk"]["sha256"] for r in runs if r["mode"] == mode}) == 1
                                               for mode in ("gxm", "pipeline")),
                  same_host_tools=len({(r["emulator_sha256"], r["debugger_sha256"]) for r in runs}) == 1,
                  runs=runs, root_cause_established=False, opengl_black_screen_fixed=False, hardware_verified=False,
                  limitations=[
                      "DEV runs use new processes but share a private pref and per-title shader cache; no clean-cache claim.",
                      "GXM raw CPU readback fails under both backends; both GXM runs crash at second initialization (0xC0000409).",
                      "Pipeline guest PASS and runtime_verified describe its finite API/lifetime checks; RGB checks are separate here.",
                      "OpenGL pipeline reaches guest PASS but host exit is 0xC0000374 and all three RGB captures are black.",
                      "Shared DEV native logs are not used as independently bound evidence; a later runner could sample the same file.",
                      "Pipeline Present patch colors are recorded, without claiming a complete shader/color reference.",
                      "Captured host RGB does not establish stored guest alpha, real hardware correctness, performance or a full game playthrough."])
    with args.output.open("x", encoding="utf-8") as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    print(json.dumps(dict(output=str(args.output), same_vpk=report["same_vpk_within_each_probe"],
                          runs=[dict(mode=r["mode"], backend=r["host_backend"], exit=r["child_exit"],
                                     nonblack=[f["nonblack_rgb_pixels"] for f in r["frames"]]) for r in runs]),
                     ensure_ascii=True, indent=2))


if __name__ == "__main__":
    main()

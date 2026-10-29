#!/usr/bin/env python3
"""Verify seven compressed-texture probe screenshots against hashed CPU refs.

This tool never starts an emulator and never rewrites fixtures. Comparison is
in display-encoded 8-bit RGB without ICC/sRGB conversion. GPU verification also
requires a successful runtime receipt; CPU-generated controls are labelled and
can never satisfy that claim. Absolute RGB error > tolerance marks a bad pixel.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
from pathlib import Path
import re
import struct
import sys
import time

from PIL import Image, ImageChops, ImageStat
import PIL


SCREEN = (960, 544)
CONTENT_RECT = (0, 2, 960, 542)
PORTRAIT_RECT = (320, 30, 640, 510)  # Coordinates within 960 x 540 content.
DEFAULT_TOLERANCE = 8
DEFAULT_BAD_RATIO = 0.02
STAGES = {
    "BACKGROUND_RGBA": ("background", None, (960, 540)),
    "BACKGROUND_PVRTC4": ("background", "pvrtc4", (960, 540)),
    "BACKGROUND_DXT1": ("background", "dxt1", (960, 540)),
    "BACKGROUND_DXT5": ("background", "dxt5", (960, 540)),
    "PORTRAIT_RGBA": ("portrait", None, (320, 480)),
    "PORTRAIT_PVRTC4": ("portrait", "pvrtc4", (320, 480)),
    "PORTRAIT_DXT5": ("portrait", "dxt5", (320, 480)),
}
FORMATS = {"pvrtc4": 0x81000000, "dxt1": 0x85000000, "dxt5": 0x87000000}
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
MAX_FILE_BYTES = 32 * 1024 * 1024


class EvidenceError(ValueError):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise EvidenceError(message)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def read_bounded(path: Path, maximum: int = MAX_FILE_BYTES) -> bytes:
    with path.open("rb") as stream:
        data = stream.read(maximum + 1)
    require(len(data) <= maximum, f"file exceeds {maximum} byte bound: {path}")
    return data


def unique_object(items):
    result = {}
    for key, value in items:
        require(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def read_json(path: Path) -> tuple[dict, dict]:
    data = read_bounded(path, 8 * 1024 * 1024)
    value = json.loads(data.decode("utf-8-sig"), object_pairs_hook=unique_object,
                       parse_constant=lambda s: (_ for _ in ()).throw(EvidenceError(f"nonfinite JSON: {s}")))
    require(isinstance(value, dict), f"JSON root must be an object: {path}")
    return value, {"path": str(path.resolve()), "bytes": len(data), "sha256": sha256(data)}


def safe_relative(root: Path, name: str) -> Path:
    require(isinstance(name, str) and bool(name), "fixture file name is missing")
    path = (root / name).resolve()
    require(path.is_relative_to(root), f"fixture path escapes fixture directory: {name}")
    return path


def load_references(fixtures: Path, runtime: dict) -> tuple[dict, dict]:
    manifest, identity = read_json(fixtures / "manifest.json")
    require(runtime.get("fixture_manifest_sha256") == identity["sha256"],
            "runtime fixture_manifest_sha256 does not match the fixture manifest")
    companion = read_bounded(fixtures / "manifest.sha256", 1024).decode("ascii").split()
    require(len(companion) == 2 and companion == [identity["sha256"], "manifest.json"],
            "fixture manifest.sha256 does not authenticate this manifest")
    require(manifest.get("schema") == "knife-vita-compressed-synthetic-fixtures-v1",
            "unsupported fixture manifest schema")
    require(all(manifest.get(k) is True for k in ("verified", "actualexecuted", "synthetic", "no_game_data")),
            "fixture manifest is not a successful synthetic conversion receipt")
    require(manifest.get("failures") == [], "fixture conversion receipt reports failures")
    records = manifest.get("files")
    require(isinstance(records, list) and len(records) <= 1000, "invalid fixture file inventory")
    by_name = {}
    for item in records:
        require(isinstance(item, dict), "invalid fixture hash record")
        name = item.get("path")
        safe_relative(fixtures, name)
        require(name not in by_name, f"ambiguous duplicate fixture hash: {name}")
        by_name[name] = item
    verified = {}
    cache = {}

    def authenticated(name: str) -> bytes:
        if name in cache:
            return cache[name]
        expected = by_name.get(name)
        require(isinstance(expected, dict), f"fixture has no hash commitment: {name}")
        require(isinstance(expected.get("sha256"), str) and SHA_RE.fullmatch(expected["sha256"]) is not None,
                f"invalid fixture hash: {name}")
        data = read_bounded(safe_relative(fixtures, name))
        require(type(expected.get("bytes")) is int and expected["bytes"] == len(data),
                f"fixture byte length mismatch: {name}")
        actual = sha256(data)
        require(actual == expected["sha256"], f"fixture SHA256 mismatch: {name}")
        verified[name] = {"path": str(safe_relative(fixtures, name)), "bytes": len(data), "sha256": actual}
        cache[name] = data
        return data

    sources = manifest.get("sources")
    converted = manifest.get("fixtures")
    require(isinstance(sources, list) and isinstance(converted, list), "missing fixture source/conversion inventory")
    references = {}
    for stage, (source_name, codec, dimensions) in STAGES.items():
        matches = [s for s in sources if s.get("name") == source_name]
        require(len(matches) == 1, f"ambiguous source: {source_name}")
        source = matches[0]
        require(source.get("logical_dimensions") == list(dimensions), f"source dimensions differ: {stage}")
        require(source.get("raw_stride") == dimensions[0] * 4, f"source stride differs: {stage}")
        require(source.get("raw_length") == dimensions[0] * dimensions[1] * 4, f"source size differs: {stage}")
        png_name = source.get("png")
        authenticated(png_name)
        with Image.open(safe_relative(fixtures, png_name)) as png:
            require(png.format == "PNG" and png.size == dimensions, f"invalid source PNG: {stage}")
            source_png_raw = png.convert("RGBA").tobytes()
        raw_name = source.get("raw_rgba")
        source_raw = authenticated(raw_name)
        require(source_raw == source_png_raw, f"source PNG/raw pixels differ: {stage}")
        reference = {"source": source_name, "codec": codec or "rgba", "dimensions": list(dimensions),
                     "source_png": verified[png_name], "source_rgba": verified[raw_name]}
        if codec is not None:
            matches = [f for f in converted if f.get("name") == f"{source_name}-{codec}"]
            require(len(matches) == 1, f"ambiguous compressed fixture: {stage}")
            fixture = matches[0]
            require(fixture.get("verified") is True and fixture.get("actualexecuted") is True,
                    f"compressed fixture was not successfully executed: {stage}")
            gxt_name = fixture.get("gxt")
            gxt = authenticated(gxt_name)
            require(len(gxt) >= 64, f"truncated GXT: {stage}")
            tag, version, count, data_offset, data_length, p4, p8, reserved = struct.unpack_from("<8I", gxt)
            offset, length, palette, flags, kind, fmt, width, height, mips, pad = struct.unpack_from("<6I2HB3s", gxt, 32)
            require((tag, version, count, p4, p8, reserved) == (0x00545847, 0x10000003, 1, 0, 0, 0),
                    f"GXT header differs: {stage}")
            require((width, height, mips, fmt, kind, flags, palette, pad) ==
                    (*dimensions, 1, FORMATS[codec], 0xA0000000, 0, 0xFFFFFFFF, b"\0\0\0"),
                    f"GXT record differs: {stage}")
            require(64 <= data_offset <= offset and offset + length <= data_offset + data_length == len(gxt),
                    f"GXT payload is outside data region: {stage}")
            cpu = fixture.get("cpu_reference", {})
            raw_name = cpu.get("decoded_logical_rgba")
            authenticated(cpu.get("decoded_dds"))
            reference["gxt"] = verified[gxt_name]
            reference["compression_error_against_original"] = cpu.get("metrics")
        raw = authenticated(raw_name)
        require(len(raw) == dimensions[0] * dimensions[1] * 4, f"reference RGBA size differs: {stage}")
        reference["reference_rgba"] = verified[raw_name]
        references[stage] = (Image.frombytes("RGBA", dimensions, raw), reference)
    return references, {"manifest": identity, "authenticated_files": verified,
                        "synthetic": True, "no_game_data": True}


def expected_screen(stage: str, texture: Image.Image) -> Image.Image:
    screen = Image.new("RGBA", SCREEN, (0, 0, 0, 255))
    if stage.startswith("BACKGROUND_"):
        screen.paste(texture, (0, 2))
        return screen
    # Deliberately independent of Pillow alpha_composite used by the controls.
    canvas = bytearray(960 * 540 * 4)
    for y in range(540):
        for x in range(960):
            color = (48, 64, 80, 255) if ((x // 16 + y // 16) & 1) == 0 else (208, 192, 176, 255)
            i = (y * 960 + x) * 4
            canvas[i:i + 4] = bytes(color)
    pixels = texture.tobytes()
    for y in range(480):
        for x in range(320):
            src = (y * 320 + x) * 4
            dst = ((y + 30) * 960 + x + 320) * 4
            alpha = pixels[src + 3]
            for channel in range(3):
                value = pixels[src + channel] * alpha + canvas[dst + channel] * (255 - alpha)
                canvas[dst + channel] = (value + 127) // 255
            # ONE / ONE_MINUS_SRC_ALPHA separate alpha, opaque destination.
            canvas[dst + 3] = 255
    screen.paste(Image.frombytes("RGBA", (960, 540), bytes(canvas)), (0, 2))
    return screen


def metrics(actual: Image.Image, expected: Image.Image, tolerance: int, ratio: float,
            exact_ratio: bool = False) -> tuple[dict, Image.Image]:
    difference = ImageChops.difference(actual.convert("RGB"), expected.convert("RGB"))
    stat = ImageStat.Stat(difference)
    channels = difference.split()
    greatest = ImageChops.lighter(ImageChops.lighter(channels[0], channels[1]), channels[2])
    mask = greatest.point([255 if value > tolerance else 0 for value in range(256)])
    count = actual.width * actual.height
    bad = mask.histogram()[255]
    alpha_diff = ImageChops.difference(actual.getchannel("A"), expected.getchannel("A"))
    alpha_bad = count - alpha_diff.histogram()[0]
    limit = 0.0 if exact_ratio else ratio
    return {"pixels": count, "channels": "RGB", "mae": stat.mean, "rmse": stat.rms,
            "max_absolute_error_by_channel": [c.getextrema()[1] for c in channels],
            "max_absolute_error": greatest.getextrema()[1], "bad_pixels": bad,
            "bad_pixel_ratio": bad / count, "rgb_tolerance": tolerance,
            "maximum_bad_pixel_ratio": limit,
            "alpha_bad_pixels": alpha_bad, "alpha_max_absolute_error": alpha_diff.getextrema()[1],
            "passed": bad / count <= limit and alpha_bad == 0}, mask


def capture_image(capture: dict, stage: str, runtime_dir: Path, used_paths: set) -> tuple[Image.Image, dict]:
    require(isinstance(capture, dict), "missing capture")
    require(capture.get("captured") is True, "capture did not succeed")
    require(capture.get("stage") == stage, "capture.stage does not match its stage key")
    require(capture.get("stage_valid_during_capture") is True, "stage was not valid throughout capture")
    artifact = capture.get("artifact")
    require(isinstance(artifact, dict), "capture lacks artifact identity")
    require(isinstance(artifact.get("path"), str) and bool(artifact["path"]), "capture lacks artifact.path")
    path = Path(artifact["path"])
    if not path.is_absolute():
        path = runtime_dir / path
    path = path.resolve()
    require(path not in used_paths, "same screenshot path is reused by more than one stage")
    used_paths.add(path)
    if capture.get("path") is not None:
        claimed = Path(capture["path"])
        claimed = claimed if claimed.is_absolute() else runtime_dir / claimed
        require(claimed.resolve() == path, "capture.path and artifact.path identify different files")
    data = read_bounded(path)
    require(type(artifact.get("bytes")) is int and artifact["bytes"] == len(data), "capture artifact length mismatch")
    require(isinstance(artifact.get("sha256"), str) and SHA_RE.fullmatch(artifact["sha256"]) is not None,
            "capture artifact SHA256 is missing or malformed")
    require(sha256(data) == artifact["sha256"], "capture artifact SHA256 mismatch")
    with Image.open(path) as opened:
        require(opened.format == "PNG", "capture is not a PNG")
        require(opened.size == SCREEN, f"actual screenshot size is {opened.size}, expected {SCREEN}")
        require(opened.mode in ("RGB", "RGBA"), f"unsupported screenshot mode: {opened.mode}")
        image = opened.convert("RGBA")
    for key, expected in (("width", 960), ("height", 544)):
        if key in capture:
            require(capture[key] == expected, f"declared capture {key} differs")
    return image, {"path": str(path), "bytes": len(data), "sha256": sha256(data), "dimensions": list(SCREEN)}


def save_png(path: Path, image: Image.Image) -> dict:
    image.save(path, format="PNG")
    data = path.read_bytes()
    return {"path": str(path.resolve()), "bytes": len(data), "sha256": sha256(data)}


def compare(runtime_path: Path, fixtures: Path, output: Path, tolerance: int, ratio: float) -> dict:
    started = time.perf_counter()
    result = {"schema": "knife-vita-compressed-pixel-comparison-v1",
              "tested_at": datetime.now(timezone.utc).isoformat(),
              "evidence_kind": "unresolved", "pixels_verified": False, "gpu_verified": False,
              "hardware_verified": False, "errors": [], "stages": {},
              "policy": {"rgb_tolerance": tolerance, "maximum_bad_pixel_ratio": ratio,
                         "uses_default_thresholds": tolerance == DEFAULT_TOLERANCE and ratio == DEFAULT_BAD_RATIO,
                         "bad_pixel": "max(abs(actual RGB - expected RGB)) > rgb_tolerance",
                         "gate": "all seven stages, content and portrait ROI bad ratios <= limit; letterbox has zero bad pixels; alpha exact",
                         "color_space": "8-bit display-encoded RGB; no sRGB/ICC transform",
                         "source_filtering": "point sampling, logical dimensions; never rescale to backing dimensions",
                         "rgb_blend": "round((srcRGB * srcA + dstRGB * (255 - srcA)) / 255)",
                         "alpha_blend": "srcA + dstA * (1 - srcA); opaque checker gives 255",
                         "screen_dimensions": list(SCREEN), "content_rectangle": [0, 2, 960, 540],
                         "portrait_content_rectangle": [320, 30, 320, 480],
                         "checker": {"cell_pixels": 16, "even_rgba": [48, 64, 80, 255],
                                     "odd_rgba": [208, 192, 176, 255]},
                         "letterbox_rgba": [0, 0, 0, 255]},
              "limitations": ["CPU compression references do not establish image quality against original assets.",
                              "Capture timing is bounded by the runtime's stage_valid_during_capture evidence.",
                              "No hardware is exercised; successful pixels do not override a failed runtime/exit gate."]}
    try:
        runtime, runtime_identity = read_json(runtime_path)
        result["runtime"] = runtime_identity
        kind = runtime.get("evidence_kind", "gpu-capture")
        require(kind in ("gpu-capture", "cpu-control"), "unknown runtime evidence_kind")
        require(not runtime.get("synthetic") or kind == "cpu-control", "synthetic screenshots must be labelled cpu-control")
        result["evidence_kind"] = kind
        result["runtime_verified"] = runtime.get("runtime_verified") is True
        if "expected_title_id" in runtime:
            require(runtime["expected_title_id"] == "KNIF00012", "runtime belongs to a different probe title")
        references, source_identity = load_references(fixtures, runtime)
        result["fixtures"] = source_identity
        captures = runtime.get("compressed_captures")
        require(isinstance(captures, dict), "runtime has no compressed_captures object")
        artifact_dir = output.parent / f"{output.stem}-artifacts"
        artifact_dir.mkdir(parents=True, exist_ok=True)
        require(not artifact_dir.resolve().is_relative_to(fixtures), "output artifacts may not overwrite fixture files")
        used_paths = set()
        for stage in STAGES:
            entry = {"passed": False, "errors": [], "reference": references[stage][1]}
            result["stages"][stage] = entry
            try:
                actual, identity = capture_image(captures.get(stage), stage, runtime_path.parent, used_paths)
                entry["capture"] = identity
                expected = expected_screen(stage, references[stage][0])
                content_actual, content_expected = actual.crop(CONTENT_RECT), expected.crop(CONTENT_RECT)
                entry["content"], _ = metrics(content_actual, content_expected, tolerance, ratio)
                if stage.startswith("PORTRAIT_"):
                    entry["portrait_roi"], _ = metrics(content_actual.crop(PORTRAIT_RECT),
                                                        content_expected.crop(PORTRAIT_RECT), tolerance, ratio)
                bars_actual = Image.new("RGBA", (960, 4))
                bars_expected = Image.new("RGBA", (960, 4), (0, 0, 0, 255))
                bars_actual.paste(actual.crop((0, 0, 960, 2)), (0, 0))
                bars_actual.paste(actual.crop((0, 542, 960, 544)), (0, 2))
                entry["letterbox"], _ = metrics(bars_actual, bars_expected, tolerance, ratio, exact_ratio=True)
                _, mask = metrics(actual, expected, tolerance, ratio)
                difference = ImageChops.difference(actual.convert("RGB"), expected.convert("RGB"))
                red = Image.new("RGB", SCREEN, (255, 0, 0))
                red.putalpha(mask)
                entry["artifacts"] = {
                    "expected": save_png(artifact_dir / f"{stage}-expected.png", expected),
                    "absolute_rgb_difference": save_png(artifact_dir / f"{stage}-absdiff.png", difference),
                    "bad_pixel_mask": save_png(artifact_dir / f"{stage}-bad-mask.png", red)}
                entry["passed"] = all(entry[k]["passed"] for k in ("content", "letterbox", "portrait_roi") if k in entry)
            except (EvidenceError, OSError, ValueError, KeyError, TypeError, struct.error) as error:
                entry["errors"].append(str(error))
        result["pixels_verified"] = len(result["stages"]) == len(STAGES) and all(s["passed"] for s in result["stages"].values())
        result["gpu_verified"] = result["pixels_verified"] and kind == "gpu-capture" and result["runtime_verified"]
    except (EvidenceError, OSError, ValueError, KeyError, TypeError, struct.error) as error:
        result["errors"].append(str(error))
    result["wall_seconds"] = time.perf_counter() - started
    result["tool"] = {"path": str(Path(__file__).resolve()), "sha256": sha256(Path(__file__).read_bytes()),
                      "python_version": sys.version, "pillow_version": PIL.__version__}
    return result


def bounded_tolerance(text: str) -> int:
    value = int(text)
    if not 0 <= value <= 255:
        raise argparse.ArgumentTypeError("RGB tolerance must be in 0..255")
    return value


def bounded_ratio(text: str) -> float:
    value = float(text)
    if not math.isfinite(value) or not 0 <= value <= 1:
        raise argparse.ArgumentTypeError("bad-pixel ratio must be finite and in 0..1")
    return value


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True, help="directory containing authenticated fixture manifest")
    parser.add_argument("--output", type=Path, required=True, help="comparison JSON; diagnostic PNGs go beside it")
    parser.add_argument("--rgb-tolerance", type=bounded_tolerance, default=DEFAULT_TOLERANCE)
    parser.add_argument("--max-bad-ratio", type=bounded_ratio, default=DEFAULT_BAD_RATIO)
    args = parser.parse_args()
    runtime, fixtures, output = args.runtime.resolve(), args.fixtures.resolve(), args.output.resolve()
    if output == runtime or output.is_relative_to(fixtures):
        parser.error("output may not overwrite runtime or fixture evidence")
    output.parent.mkdir(parents=True, exist_ok=True)
    result = compare(runtime, fixtures, output, args.rgb_tolerance, args.max_bad_ratio)
    output.write_text(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False) + "\n", encoding="utf8")
    print(json.dumps({"pixels_verified": result["pixels_verified"], "gpu_verified": result["gpu_verified"],
                      "evidence_kind": result["evidence_kind"], "stages": len(result["stages"]),
                      "failed_stages": [s for s, v in result["stages"].items() if not v["passed"]],
                      "errors": result["errors"], "output": str(output)}, ensure_ascii=True))
    control_passed = result["evidence_kind"] == "cpu-control" and result["pixels_verified"]
    return 0 if result["gpu_verified"] or control_passed else 1


if __name__ == "__main__":
    raise SystemExit(main())

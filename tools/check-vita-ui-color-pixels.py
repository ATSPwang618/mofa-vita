#!/usr/bin/env python3
"""Independent CPU oracle for synthetic UI Sprite/sRGB/linear-clamp GXM captures.

This does not run a GPU. CPU controls are always labelled cpu-control and cannot
set gpu_verified. The separate KNIF00013 probe uses the production native Sprite,
Alter and Present entries, but does not exercise renderer cache/archive loading.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import io
import json
import math
from pathlib import Path
import struct
import sys
import time

from PIL import Image, ImageChops

_spec = importlib.util.spec_from_file_location(
    "compressed_pixel_evidence", Path(__file__).with_name("check-vita-compressed-pixels.py"))
evidence = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(evidence)

STAGES = ("UI_RUNTIME_ALTER", "UI_OFFLINE_RGBA", "UI_OFFLINE_DXT5")
SIZE = (160, 112)
SCREEN = (960, 544)
CONTENT = (0, 2, 960, 542)
TITLE_ID = "KNIF00013"
DEFAULT_TOLERANCE = 8
DEFAULT_BAD_RATIO = .02
CHECKER = ((.12, .18, .24, 1.), (.72, .60, .48, 1.))
# Rectangles are physical content pixels. UV entries are (left, top, right,
# bottom) in logical 160x112 texture space, never padded PoT storage space.
PANELS = (
    {"name": "gray_color_clamp", "rect": [24., 24., 432., 216.],
     "uv": [-.05, -.05, 1.05, .55], "color": [1., 1., 1., 1.]},
    {"name": "full_atlas_tint", "rect": [504., 24., 432., 216.],
     "uv": [0., 0., 1., 1.], "color": [.64, .90, .45, .72]},
    {"name": "transparent_crop", "rect": [72., 284., 352., 224.],
     "uv": [.10, .45, .90, 1.05], "color": [.85, .55, 1., .85]},
    {"name": "mirror_fractional_clamp", "rect": [520.25, 288.25, 352., 216.],
     "uv": [1.10, 1.05, -.10, -.05], "color": [.50, 1., .65, .65]},
)


def contract() -> dict:
    return {"version": 1, "title_id": TITLE_ID, "stages": list(STAGES),
            "physical": list(SIZE), "logical": [320, 224], "backing": [256, 128],
            "screen": list(SCREEN), "content": [0, 2, 960, 540],
            "sampler": "linear clamp; texel centers at (i+0.5)/logicalPhysicalExtent",
            "source_srgb": True, "canvas_srgb": True,
            "source_alpha": "straight, including nonzero RGB under zero alpha",
            "shader": "Sprite", "blend": "Alpha: ONE/ONE_MINUS_SRC_ALPHA RGB; ZERO/ONE alpha",
            "alter": "srgb_encode(pow(srgb_decode(src_rgb),1/2.2)); alpha unchanged",
            "present": "srgb_encode(pow(srgb_decode(quantize(srgb_encode(canvas_linear))),2.2))",
            "checker": {"cell": 16, "working_linear_rgba": [list(v) for v in CHECKER]},
            "panels": list(PANELS)}


def srgb_decode(s: float) -> float:
    return s / 12.92 if s <= .04045 else ((s + .055) / 1.055) ** 2.4


def srgb_encode(l: float) -> float:
    return l * 12.92 if l <= .0031308 else 1.055 * l ** (1. / 2.4) - .055


def unorm8(value: float) -> int:
    return min(255, max(0, math.floor(value * 255. + .5)))


def alter_pixel(pixel: tuple[int, ...]) -> tuple[int, ...]:
    return tuple(unorm8(srgb_encode(srgb_decode(c / 255.) ** (1. / 2.2)))
                 for c in pixel[:3]) + (pixel[3],)


class Sampler:
    def __init__(self, image: Image.Image, gamma: bool = True):
        self.width, self.height = image.size
        lut = [srgb_decode(i / 255.) if gamma else i / 255. for i in range(256)]
        self.pixels = [(lut[r], lut[g], lut[b], a / 255.) for r, g, b, a in image.getdata()]

    def sample(self, u: float, v: float, point: bool = False) -> tuple[float, ...]:
        if not math.isfinite(u) or not math.isfinite(v):
            raise ValueError("sampler coordinates must be finite")
        if point:
            x = min(self.width - 1, max(0, math.floor(u * self.width)))
            y = min(self.height - 1, max(0, math.floor(v * self.height)))
            return self.pixels[y * self.width + x]
        x, y = u * self.width - .5, v * self.height - .5
        x0, y0 = math.floor(x), math.floor(y)
        fx, fy = x - x0, y - y0
        x1, y1 = min(self.width - 1, max(0, x0 + 1)), min(self.height - 1, max(0, y0 + 1))
        x0, y0 = min(self.width - 1, max(0, x0)), min(self.height - 1, max(0, y0))
        a, b = self.pixels[y0 * self.width + x0], self.pixels[y0 * self.width + x1]
        c, d = self.pixels[y1 * self.width + x0], self.pixels[y1 * self.width + x1]
        return tuple((a[k] * (1. - fx) + b[k] * fx) * (1. - fy)
                     + (c[k] * (1. - fx) + d[k] * fx) * fy for k in range(4))


def sample_linear_srgb(image: Image.Image, u: float, v: float) -> tuple[float, ...]:
    return Sampler(image).sample(u, v)


def sprite_blend(sample, color, destination) -> tuple[float, ...]:
    alpha = sample[3] * color[3]
    return tuple(sample[k] * color[k] * alpha + destination[k] * (1. - alpha)
                 for k in range(3)) + (destination[3],)


def present_working_rgb(rgb) -> tuple[int, ...]:
    # Model the finite RGBA8 sRGB offscreen surface before the final Present.
    return tuple(unorm8(srgb_encode(srgb_decode(unorm8(srgb_encode(c)) / 255.) ** 2.2))
                 for c in rgb)


def checker_bytes() -> bytes:
    """The point-sampled sRGB checker is already quantized before blending."""
    values = [bytes((*[unorm8(srgb_encode(c)) for c in pixel[:3]], 255)) for pixel in CHECKER]
    return b"".join(values[((x // 16 + y // 16) & 1)] for y in range(540) for x in range(960))


def render_expected(texture: Image.Image, fault: str | None = None) -> Image.Image:
    if texture.mode != "RGBA" or texture.size != SIZE:
        raise ValueError("reference must be 160x112 RGBA")
    sampler = Sampler(texture, gamma=fault != "gamma")
    checker_display = [(*present_working_rgb(c[:3]), 255) for c in CHECKER]
    checker_working = [tuple(srgb_decode(unorm8(srgb_encode(c)) / 255.) for c in pixel[:3]) + (1.,)
                       for pixel in CHECKER]
    pixels = [checker_display[((x // 16 + y // 16) & 1)] for y in range(540) for x in range(960)]
    for panel in PANELS:
        left, top, width, height = panel["rect"]
        u0, v0, u1, v1 = panel["uv"]
        color = panel["color"] if fault != "no-tint" else [1.] * 4
        for y in range(math.ceil(top - .5), math.ceil(top + height - .5)):
            v = v0 + (y + .5 - top) / height * (v1 - v0)
            for x in range(math.ceil(left - .5), math.ceil(left + width - .5)):
                u = u0 + (x + .5 - left) / width * (u1 - u0)
                sample = sampler.sample(u * (160. / 256.) if fault == "pot-uv" else u,
                                        v * (112. / 128.) if fault == "pot-uv" else v,
                                        point=fault == "point")
                if fault == "drop-alpha":
                    sample = (*sample[:3], 1.)
                if fault == "double-alpha":
                    sample = (*(c * sample[3] for c in sample[:3]), sample[3])
                background = checker_working[((x // 16 + y // 16) & 1)]
                result = sprite_blend(sample, color, background)
                pixels[y * 960 + x] = (*present_working_rgb(result[:3]), 255)
    screen = Image.new("RGBA", SCREEN, (0, 0, 0, 255))
    canvas = Image.new("RGBA", (960, 540))
    canvas.putdata(pixels)
    screen.paste(canvas, (0, 2))
    return screen


def load_references(fixtures: Path, runtime: dict) -> tuple[dict, dict]:
    manifest, identity = evidence.read_json(fixtures / "manifest.json")
    evidence.require(manifest.get("schema") == "knife-vita-ui-color-fixtures-v1", "fixture schema differs")
    evidence.require(manifest.get("synthetic") is True and manifest.get("no_game_data") is True,
                     "fixture must be synthetic and contain no game data")
    evidence.require(manifest.get("actualexecuted") is True, "fixture encoder did not execute")
    evidence.require(manifest.get("contract") == contract(), "fixture rendering contract differs")
    evidence.require(runtime.get("fixture_manifest_sha256") == identity["sha256"], "fixture manifest hash differs")
    inventory = manifest.get("files")
    evidence.require(isinstance(inventory, dict), "fixture has no authenticated file inventory")
    verified = {}

    def authenticated(name: str) -> bytes:
        evidence.require(isinstance(name, str) and name in inventory, "missing fixture file identity")
        path = evidence.safe_relative(fixtures, name)
        data = evidence.read_bounded(path, 16 * 1024 * 1024)
        item = inventory[name]
        evidence.require(isinstance(item, dict), f"invalid fixture identity: {name}")
        evidence.require(item.get("bytes") == len(data) and item.get("sha256") == evidence.sha256(data),
                         f"fixture hash/size differs: {name}")
        verified[name] = {"path": str(path), "bytes": len(data), "sha256": evidence.sha256(data)}
        return data

    original = authenticated("ui-source.rgba")
    altered = authenticated("ui-offline.alter.rgba")
    decoded = authenticated("ui-offline-decoded.rgba")
    for raw in (original, altered, decoded):
        evidence.require(len(raw) == SIZE[0] * SIZE[1] * 4, "fixture RGBA length differs")
    with Image.open(io.BytesIO(authenticated("ui-source.png"))) as source:
        evidence.require(source.size == SIZE and source.convert("RGBA").tobytes() == original,
                         "PNG source differs from raw source")
    evidence.require(authenticated("ui-checker.rgba") == checker_bytes(), "checker differs from rendering contract")
    independently_altered = bytes(v for p in Image.frombytes("RGBA", SIZE, original).getdata() for v in alter_pixel(p))
    evidence.require(independently_altered == altered, "converter pre-Alter differs from independent transfer formula")
    dds = authenticated("ui-offline.dds")
    with Image.open(io.BytesIO(dds)) as image:
        evidence.require(image.size == SIZE and image.convert("RGBA").tobytes() == decoded,
                         "DDS CPU decode differs from stored reference")
    gxt = authenticated("ui-offline.gxt")
    evidence.require(len(gxt) >= 64, "GXT truncated")
    head = struct.unpack_from("<8I", gxt)
    offset, length, palette, flags, kind, fmt, width, height, mips, pad = struct.unpack_from("<6I2HB3s", gxt, 32)
    evidence.require(head[:3] == (0x00545847, 0x10000003, 1) and head[5:] == (0, 0, 0), "GXT header differs")
    evidence.require((width, height, mips, kind, fmt, palette, flags, pad) ==
                     (*SIZE, 1, 0xA0000000, 0x87000000, 0xFFFFFFFF, 0, b"\0\0\0"), "GXT record differs")
    evidence.require((offset, length) == head[3:5] and offset >= 64 and offset % 16 == 0
                     and offset + length == len(gxt) and length == 256 * 128, "GXT payload range differs")
    authenticated("ui-offline.json")
    images = {STAGES[0]: Image.frombytes("RGBA", SIZE, independently_altered),
              STAGES[1]: Image.frombytes("RGBA", SIZE, altered),
              STAGES[2]: Image.frombytes("RGBA", SIZE, decoded)}
    return images, {"manifest": identity, "authenticated_files": verified}


def compare(runtime_path: Path, fixtures: Path, output: Path, tolerance: int = DEFAULT_TOLERANCE,
            ratio: float = DEFAULT_BAD_RATIO) -> dict:
    started = time.perf_counter()
    result = {"schema": "knife-vita-ui-color-comparison-v1", "tested_at": datetime.now(timezone.utc).isoformat(),
              "evidence_kind": "unresolved", "pixels_verified": False, "gpu_verified": False,
              "hardware_verified": False, "errors": [], "stages": {}, "contract": contract(),
              "policy": {"rgb_tolerance": tolerance, "maximum_bad_pixel_ratio": ratio,
                         "gate": "all stages and four panel ROIs pass; exact alpha and zero bad letterbox pixels",
                         "bad_pixel": "any RGB absolute difference > tolerance",
                         "uses_default_thresholds": tolerance == DEFAULT_TOLERANCE and ratio == DEFAULT_BAD_RATIO},
              "limitations": ["Synthetic native Sprite/Alter/Present path only; not renderer facade/cache/archive loading.",
                              "DXT CPU reference is Pillow DDS decode, not proof of swizzled GXT layout.",
                              "A matching CPU control never establishes GPU or hardware success.",
                              "Capture stage identity and timing depend on the runtime runner's evidence."]}
    try:
        runtime, identity = evidence.read_json(runtime_path)
        result["runtime"] = identity
        kind = runtime.get("evidence_kind", "gpu-capture")
        evidence.require(kind in ("gpu-capture", "cpu-control"), "unknown evidence kind")
        evidence.require(not runtime.get("synthetic") or kind == "cpu-control", "CPU screenshots must be cpu-control")
        evidence.require(runtime.get("expected_title_id") == TITLE_ID, "runtime title differs")
        result["evidence_kind"] = kind
        result["runtime_verified"] = runtime.get("runtime_verified") is True
        images, result["fixtures"] = load_references(fixtures, runtime)
        captures = runtime.get("ui_color_captures")
        evidence.require(isinstance(captures, dict), "missing ui_color_captures")
        directory = output.parent / f"{output.stem}-artifacts"
        evidence.require(not directory.resolve().is_relative_to(fixtures.resolve()), "artifacts may not overwrite fixtures")
        directory.mkdir(parents=True, exist_ok=True)
        used = set()
        for stage in STAGES:
            entry = {"passed": False, "errors": []}
            result["stages"][stage] = entry
            try:
                actual, entry["capture"] = evidence.capture_image(captures.get(stage), stage, runtime_path.parent, used)
                expected = render_expected(images[stage])
                entry["content"], _ = evidence.metrics(actual.crop(CONTENT), expected.crop(CONTENT), tolerance, ratio)
                entry["panels"] = {}
                for panel in PANELS:
                    x, y, w, h = panel["rect"]
                    roi = (math.ceil(x - .5), math.ceil(y - .5) + 2,
                           math.ceil(x + w - .5), math.ceil(y + h - .5) + 2)
                    entry["panels"][panel["name"]], _ = evidence.metrics(actual.crop(roi), expected.crop(roi), tolerance, ratio)
                bars = Image.new("RGBA", (960, 4))
                bars.paste(actual.crop((0, 0, 960, 2)), (0, 0))
                bars.paste(actual.crop((0, 542, 960, 544)), (0, 2))
                entry["letterbox"], _ = evidence.metrics(bars, Image.new("RGBA", (960, 4), (0, 0, 0, 255)),
                                                         tolerance, 0., exact_ratio=True)
                _, mask = evidence.metrics(actual, expected, tolerance, ratio)
                entry["artifacts"] = {
                    "expected": evidence.save_png(directory / f"{stage}-expected.png", expected),
                    "difference": evidence.save_png(directory / f"{stage}-difference.png", ImageChops.difference(actual, expected)),
                    "bad_pixel_mask": evidence.save_png(directory / f"{stage}-mask.png", mask)}
                entry["passed"] = entry["content"]["passed"] and entry["letterbox"]["passed"] \
                    and all(p["passed"] for p in entry["panels"].values())
            except (evidence.EvidenceError, OSError, ValueError, KeyError, TypeError, struct.error) as error:
                entry["errors"].append(str(error))
        result["pixels_verified"] = len(result["stages"]) == len(STAGES) and all(e["passed"] for e in result["stages"].values())
        result["gpu_verified"] = result["pixels_verified"] and result["runtime_verified"] and kind == "gpu-capture"
    except (evidence.EvidenceError, OSError, ValueError, KeyError, TypeError, struct.error) as error:
        result["errors"].append(str(error))
    result["wall_seconds"] = time.perf_counter() - started
    result["tool"] = {"path": str(Path(__file__).resolve()), "sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                      "shared_evidence_helper": {"path": str(Path(evidence.__file__).resolve()),
                          "sha256": hashlib.sha256(Path(evidence.__file__).read_bytes()).hexdigest()}, "python": sys.version}
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--fixtures", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rgb-tolerance", type=evidence.bounded_tolerance, default=DEFAULT_TOLERANCE)
    parser.add_argument("--max-bad-ratio", type=evidence.bounded_ratio, default=DEFAULT_BAD_RATIO)
    args = parser.parse_args()
    runtime, fixtures, output = args.runtime.resolve(), args.fixtures.resolve(), args.output.resolve()
    if output == runtime or output.is_relative_to(fixtures):
        parser.error("output may not overwrite runtime or fixtures")
    output.parent.mkdir(parents=True, exist_ok=True)
    result = compare(runtime, fixtures, output, args.rgb_tolerance, args.max_bad_ratio)
    output.write_text(json.dumps(result, indent=2, allow_nan=False) + "\n", encoding="utf8")
    print(json.dumps({k: result[k] for k in ("pixels_verified", "gpu_verified", "evidence_kind", "errors")}))
    return 0 if result["gpu_verified"] or result["evidence_kind"] == "cpu-control" and result["pixels_verified"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

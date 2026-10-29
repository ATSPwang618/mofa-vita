#!/usr/bin/env python3
"""Generate one synthetic UI atlas through the existing production GXT converter.

Only creates a fresh directory under .work/vita; never reads game images or
changes canonical archives. The installed SDK tool stays at its original path.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import struct
import sys
import time

from PIL import Image
import PIL


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), Path(__file__).with_name(name))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


oracle = load("check-vita-ui-color-pixels.py")
converter = load("vita-game-gxt.py")
SAFE_ROOT = Path(__file__).resolve().parents[1] / ".work" / "vita"


def synthetic_atlas() -> Image.Image:
    """Original RGBA artwork, with independent channel colors under every alpha."""
    image = Image.new("RGBA", oracle.SIZE)
    pixels = []
    colors = ((224, 20, 35), (25, 210, 60), (25, 55, 230), (195, 175, 30),
              (30, 190, 200), (210, 35, 185), (16, 49, 97), (221, 137, 74))
    for y in range(image.height):
        for x in range(image.width):
            if y < 24:
                gray = round(x * 255 / (image.width - 1)) if y < 12 else (x // 20) * 32
                pixel = (gray, gray, gray, 255)
            elif y < 48:
                rgb = colors[x // 20]
                # Thin high-contrast edges distinguish point/linear sampling.
                pixel = (*((255, 255, 255) if x % 20 == 0 or y == 36 else rgb), 255)
            elif x < 80:
                distance = math.hypot((x - 39.5) / 32., (y - 79.5) / 27.)
                alpha = round(min(1., max(0., (1. - distance) / .25)) * 255)
                rgb = (30 + x * 2, 205 - (y - 48) * 2, 65 + (x % 13) * 10)
                pixel = (*rgb, alpha)
            else:
                alpha = round((x - 80) * 255 / 79)
                if y in (57, 71, 91, 105) or x % 17 == 0:
                    alpha = 0 if y % 2 else 255
                pixel = (210 - (y - 48) * 2, 35 + (x - 80) * 2, 65 + (y % 11) * 15, alpha)
            pixels.append(pixel)
    image.putdata(pixels)
    return image


def file_identity(path: Path) -> dict:
    data = path.read_bytes()
    return {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}


def generate(directory: Path) -> dict:
    started = time.perf_counter()
    directory = directory.resolve()
    if not directory.is_relative_to(SAFE_ROOT.resolve()) or directory == SAFE_ROOT.resolve():
        raise ValueError("output must be a new child directory under .work/vita")
    directory.mkdir(parents=True, exist_ok=False)
    image = synthetic_atlas()
    image.save(directory / "ui-source.png", format="PNG")
    image.tobytes()  # The fixed size is checked before every format wrapper.
    (directory / "ui-source.rgba").write_bytes(image.tobytes())
    (directory / "ui-checker.rgba").write_bytes(oracle.checker_bytes())
    png = (directory / "ui-source.png").read_bytes()
    (directory / "ui-source.kimg").write_bytes(
        b"KIMG" + struct.pack("<7I", 1, 320, 224, *oracle.SIZE, 2, len(png)) + png)
    receipt = converter.convert(directory / "ui-source.kimg", directory / "ui-offline.kimg", "auto")
    if receipt["codec"] != "dxt5" or not all(receipt["gxt"]["checks"].values()):
        raise AssertionError("synthetic transparent atlas did not produce verified DXT5")
    with Image.open(directory / "ui-offline.dds") as dds:
        decoded = dds.convert("RGBA")
        if decoded.size != oracle.SIZE:
            raise AssertionError("DDS reference extent differs")
        (directory / "ui-offline-decoded.rgba").write_bytes(decoded.tobytes())
        decoded.save(directory / "ui-offline-decoded.png", format="PNG")
    independently_altered = bytes(c for pixel in image.getdata() for c in oracle.alter_pixel(pixel))
    if independently_altered != (directory / "ui-offline.alter.rgba").read_bytes():
        raise AssertionError("production converter pre-Alter differs from independent transfer formula")
    Image.frombytes("RGBA", oracle.SIZE, independently_altered).save(directory / "ui-offline-altered.png")
    manifest = {"schema": "knife-vita-ui-color-fixtures-v1",
                "created_utc": datetime.now(timezone.utc).isoformat(),
                "synthetic": True, "no_game_data": True, "actualexecuted": True,
                "gpu_verified": False, "hardware_verified": False,
                "contract": oracle.contract(), "alpha_extrema": image.getchannel("A").getextrema(),
                "source_description": "Original grayscale ramp, independent RGB blocks, transparent color circle and alpha ramp with fine outlines.",
                "source_color": "IEC sRGB byte interpretation, no embedded ICC profile, straight alpha before DXT loss.",
                "gxt": receipt["gxt"], "converter_cpu_error": receipt["cpu_decode"],
                "tools": {"generator": {"path": str(Path(__file__).resolve()), **file_identity(Path(__file__))},
                          "converter": receipt["tool"]["converter"], "psp2gxt": receipt["tool"]["psp2gxt"],
                          "pillow": PIL.__version__, "python": sys.version},
                "wall_seconds": time.perf_counter() - started,
                "files": {path.name: file_identity(path) for path in sorted(directory.iterdir()) if path.is_file()}}
    raw = (json.dumps(manifest, indent=2, allow_nan=False) + "\n").encode("utf8")
    (directory / "manifest.json").write_bytes(raw)
    (directory / "manifest.sha256").write_text(hashlib.sha256(raw).hexdigest() + "  manifest.json\n", encoding="ascii")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=SAFE_ROOT / "ui-color-fixtures")
    args = parser.parse_args()
    result = generate(args.output)
    print(json.dumps({"directory": str(args.output.resolve()), "actualexecuted": result["actualexecuted"],
                      "gxt": result["gxt"], "wall_seconds": result["wall_seconds"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

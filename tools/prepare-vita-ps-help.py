#!/usr/bin/env python3
"""Encode the editable PS help PNGs as codec-9 DXT GXT replacements.

Requires the same Pillow/numpy/ispc_texcomp dependencies as quality540.
Only updates the named embedded KIMG; never edits game archives.
"""
import argparse
from pathlib import Path
import struct
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools/quality540"))
from PIL import Image
from rebuild_scene_art import encode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("name", choices=("help_key_ps", "conf_manual0_ps"))
    args = parser.parse_args()
    physical, logical = ((384, 256), (384, 256)) if args.name == "help_key_ps" else ((960, 540), (1920, 1080))
    base = ROOT / "knife/knife/data" / args.name
    with Image.open(base.with_suffix(".png")) as source:
        image = source.convert("RGBA")
    assert image.size == physical, "Replacement must preserve atlas dimensions/UVs"
    payload, decoded, kind = encode(image)
    assert decoded.size == physical
    result = b"KIMG" + struct.pack("<7I", 1, *logical, *physical, 9, len(payload)) + payload
    base.with_suffix(".kimg").write_bytes(result)
    print(f"{args.name}: {kind}, {len(result)} bytes; PNG is an offline source only")


if __name__ == "__main__":
    main()

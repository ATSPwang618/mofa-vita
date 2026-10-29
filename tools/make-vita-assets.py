#!/usr/bin/env python3
"""Build the Vita 960x540 asset pack from game data.

Two entry points, depending on what you have:

  --compact DIR   a knife "compact" assets directory: the game's own archives plus
                  <archive>.compact.json audits (this is what the desktop build
                  produces from a romfs, see doc/08).
  --out DIR       destination for the Vita pack (default .work/assets).

Stages:
  1. every archive listed in --archives is rewritten: PNG/JPEG textures become
     pre-baked GXT (DXT1 when fully opaque, DXT5 with alpha);
  2. the remaining runtime files (movies, audio, fonts, scripts, language
     tables) are copied unchanged;
  3. vita-540p-assets.json is generated and every file is hashed.

Use --limit/--archives for a quick smoke test; a full pack is ~3 GB and takes
tens of minutes.
"""
from __future__ import annotations

import argparse
import importlib
import shutil
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))

DEFAULT_ARCHIVES = "jallpac,jallpachd,allui,allpacml,allpachdml,parts"


def load(name: str):
    return importlib.import_module(name)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--compact", type=Path, required=True,
                        help="compact assets directory (archives + *.compact.json)")
    parser.add_argument("--out", type=Path, default=ROOT / ".work/assets",
                        help="output assets directory")
    parser.add_argument("--archives", default=DEFAULT_ARCHIVES,
                        help=f"comma separated archives (default: {DEFAULT_ARCHIVES})")
    parser.add_argument("--limit", type=int, help="only convert the first N images per archive")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--keep", type=Path, help="text file of image stems to leave unconverted")
    parser.add_argument("--nx", action="store_true", help="wrap converted entries in NXCX")
    parser.add_argument("--manifest-only", action="store_true",
                        help="skip conversion and only regenerate vita-540p-assets.json")
    args = parser.parse_args()

    src = args.compact.resolve()
    out = args.out.resolve()
    if not src.is_dir():
        parser.error(f"not a directory: {src}")
    out.mkdir(parents=True, exist_ok=True)

    dxt = load("pack_dxt_540")
    dxt.SOURCE = src
    dxt.OUTPUT = out
    dxt.BASE = out if args.manifest_only else src

    if args.keep:
        for line in args.keep.read_text(encoding="utf-8").splitlines():
            if line and not line.startswith("#"):
                dxt.GRADED.add(dxt.stem(line))
        print(f"keep-list: {len(dxt.GRADED)} image stems stay unconverted", flush=True)

    if not args.manifest_only:
        # Copy runtime files that the converter does not rewrite.
        rewritten = {f"{n}{s}" for n in DEFAULT_ARCHIVES.split(",")
                     for s in (".mrg", ".hed", ".nam")}
        copied = 0
        for item in src.iterdir():
            target = out / item.name
            if item.is_dir():
                shutil.copytree(item, target, dirs_exist_ok=True)
            elif item.name not in rewritten and not item.name.endswith(".compact.json"):
                shutil.copyfile(item, target)
                copied += 1
        print(f"runtime files copied: {copied}", flush=True)

        started = time.time()
        for name in args.archives.split(","):
            name = name.strip()
            if not (src / f"{name}.compact.json").is_file():
                print(f"skip {name}: no {name}.compact.json", flush=True)
                continue
            receipt = dxt.convert_archive(name, args.limit, args.jobs, args.nx)
            print(f"{name}: {receipt['outputs']}", flush=True)
        print(f"conversion finished in {time.time() - started:.1f}s", flush=True)

    # Manifest: hash everything that will ship.
    native = load("create_native540")
    native.SOURCE = out
    native.OUTPUT = out
    # --manifest-only hashes what is already on disk instead of re-copying.
    sys.argv = ["create_native540.py", "--manifest-only", "--out", str(out)]
    native.main()
    print(f"manifest: {out / 'vita-540p-assets.json'}", flush=True)


if __name__ == "__main__":
    main()

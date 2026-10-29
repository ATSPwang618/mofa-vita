"""Convert the native 960x540 pack's still images to pre-baked GXT (codec 8).

Each convertible KIMG becomes a GXT container holding DXT1 (fully opaque) or
DXT5 (any alpha), whose pixels are the same default-Alter transform the GPU bake
would apply. Archive entries are written raw (no NX) because DXT is already
compressed and the reader accepts raw sector entries, so loading skips inflate.

Never converted, by contract:
  * language strips (runtime CPU-crops them; codec8 hard-rejects a strip request)
  * codec 7 (unorm masks whose semantics codec8 cannot express)
  * the BORDER_PARTS atlas (soft-clip LUT reads its pixels on the CPU)
Usage: python tools/pack_dxt_540.py [--archives a,b] [--limit N] [--jobs N]
"""
from __future__ import annotations

import argparse
import importlib.util
import io
import json
import mmap
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import zlib
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
from optimize_compact import ArchiveWriter, textures  # noqa: E402
from verify_540p_assets import decoded, stored  # noqa: E402

SOURCE = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets"
BASE = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets"
OUTPUT = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-540p-DXT/assets"
SDK = Path("D:/unity/PSVITA/sdk/host_tools/bin/psp2gxt.exe")
# Entries that must keep their original encoding:
#  * the soft-clip gradient atlas is read back on the CPU to build the LUT
#  * every image a scene applies STEC / SNX2 / SNX3 to (codec8 rejects a bake)
KEEP_ATLAS = {"BORDER_PARTS.NXZ"}
GRADED: set[str] = set()


def stem(name: str) -> str:
    return name.split(".")[0].upper()

spec = importlib.util.spec_from_file_location("vita_game_gxt", ROOT / "tools/vita-game-gxt.py")
gxt_tool = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gxt_tool)


def sha(data: bytes) -> str:
    import hashlib
    return hashlib.sha256(data).hexdigest()


def archive_rows(path: Path):
    hed = path.read_bytes()
    rows = list(struct.iter_unpack("<IHH", hed))
    terminal = next(i for i, row in enumerate(rows) if row[0] == 0xFFFFFFFF)
    return rows[:terminal], hed[terminal * 8:]


def encode(rgba: Image.Image, work: Path, token: str):
    """Alter-bake, compress to DXT, wrap in GXT. Returns (gxt bytes, codec name, check)."""
    altered = gxt_tool.offline_alter(rgba)
    opaque = altered.getchannel("A").getextrema() == (255, 255)
    codec = "dxt1" if opaque else "dxt5"
    dds_path = work / f"{token}.dds"
    gxt_path = work / f"{token}.gxt"
    try:
        altered.save(dds_path, format="DDS", pixel_format=codec.upper())
        completed = subprocess.run([str(SDK), "-i", str(dds_path), "-o", str(gxt_path)],
                                   capture_output=True)
        if completed.returncode != 0:
            raise RuntimeError(f"psp2gxt rc={completed.returncode}: "
                               f"{completed.stderr.decode(errors='replace')[:300]}")
        raw = gxt_path.read_bytes()
    finally:
        dds_path.unlink(missing_ok=True)
        gxt_path.unlink(missing_ok=True)
    check = gxt_tool.validate_gxt(raw, altered.size, codec)
    return raw, codec, check


def convert_kimg(raw: bytes, meta: dict, work: Path, token: str, stats: dict):
    magic, version, lw, lh, w, h, codec, length = struct.unpack_from("<4s7I", raw)
    if (codec not in (1, 2) or meta.get("strips", 1) > 1 or length + 32 != len(raw)
            or stem(meta.get("name", "")) in GRADED):
        stats["kept"] += 1
        return raw, {"logical": [lw, lh], "physical": [w, h], "codec": codec, "decision": "keep"}
    with Image.open(io.BytesIO(raw[32:])) as original:
        if original.size != (w, h):
            raise ValueError("KIMG payload dimensions mismatch")
        rgba = original.convert("RGBA")
    payload, chosen, check = encode(rgba, work, token)
    out = struct.pack("<4s7I", b"KIMG", 1, lw, lh, w, h, 8, len(payload)) + payload
    stats["gxt_" + chosen] += 1
    return out, {"logical": [lw, lh], "physical": [w, h], "codec": 8, "decision": chosen,
                 "gxt_bytes": len(payload), "check": check}


def convert_atlas(raw: bytes, meta: dict, work: Path, token: str, stats: dict):
    members = textures(raw)
    infos = meta["textures"]
    if len(members) != len(infos):
        raise ValueError("atlas member count mismatch")
    out = bytearray(raw[:12])
    rows = []
    for index, ((label, member), info) in enumerate(zip(members, infos)):
        if (meta.get("name") in KEEP_ATLAS or stem(info.get("name", "")) in GRADED
                or info.get("strips", 1) > 1):
            converted, row = member, {"label": label, "decision": "keep"}
            stats["kept"] += 1
        else:
            converted, row = convert_kimg(member, info, work, f"{token}_{index}", stats)
            row["label"] = label
        name = label.encode("utf-8")
        out.extend(struct.pack("<II", len(name), len(converted)) + name + converted)
        rows.append(row)
    return bytes(out), rows


def convert_archive(name: str, limit: int | None, jobs: int, use_nx: bool) -> dict:
    started = time.monotonic()
    mrg_path, hed_path = SOURCE / f"{name}.mrg", SOURCE / f"{name}.hed"
    audit = json.loads((SOURCE / f"{name}.compact.json").read_text(encoding="utf-8"))
    rows, terminal = archive_rows(hed_path)
    if len(rows) != len(audit["records"]):
        raise ValueError(f"{name}: record count mismatch")
    total = len(rows) if limit is None else min(limit, len(rows))
    work_root = Path(tempfile.mkdtemp(prefix=f"dxt-{name}-"))
    stats = {"gxt_dxt1": 0, "gxt_dxt5": 0, "kept": 0}
    hed = bytearray()
    records = []
    partial = OUTPUT / f"{name}.mrg.partial"
    with mrg_path.open("rb") as handle, mmap.mmap(handle.fileno(), 0, access=mmap.ACCESS_READ) as mm, \
            partial.open("w+b") as out, ThreadPoolExecutor(max_workers=jobs) as pool:
        writer = ArchiveWriter(out)

        def job(index):
            meta = audit["records"][index]
            before = stored(mm, rows[index], missing=meta["name"] == "NOFILE.EXISTS")
            raw = decoded(before)
            if meta.get("kind") == "portable-atlas" or raw[:4] == b"KATL":
                converted, rows_out = convert_atlas(raw, meta, work_root, f"{name}{index}", stats)
            elif raw[:4] == b"KIMG":
                converted, rows_out = convert_kimg(raw, meta, work_root, f"{name}{index}", stats)
                rows_out = [rows_out]
            else:
                converted, rows_out = before, []
                stats["kept"] += 1
            return converted, rows_out, meta["name"], len(converted)

        for index in range(total):
            data, image_rows, entry_name, _size = job(index)
            if use_nx and data[:4] in (b"KIMG", b"KATL"):
                packed = zlib.compress(data, 6)
                data = struct.pack("<4sIII", b"NXCX", len(data), len(packed), 0) + packed
                unpacked = (_size + 2047) // 2048
            else:
                unpacked = (len(data) + 2047) // 2048
            hed.extend(writer.add(data, unpacked))
            records.append({"index": index, "name": entry_name, "textures": image_rows})
            if index % 500 == 0:
                print(f"{name}: {index}/{total}, {writer.total / 1048576:.1f} MiB", flush=True)
    hed.extend(terminal)
    partial.replace(OUTPUT / f"{name}.mrg")
    (OUTPUT / f"{name}.hed").write_bytes(bytes(hed))
    if (SOURCE / f"{name}.nam").exists():
        shutil.copyfile(SOURCE / f"{name}.nam", OUTPUT / f"{name}.nam")
    shutil.rmtree(work_root, ignore_errors=True)
    elapsed = round(time.monotonic() - started, 1)
    receipt = {"schema": "knife-540p-dxt-archive-v1", "name": name, "entries": len(records),
               "statistics": stats, "seconds": elapsed,
               "outputs": {f".{suffix}": {"bytes": (OUTPUT / f'{name}{suffix}').stat().st_size,
                                          "sha256": sha((OUTPUT / f'{name}{suffix}').read_bytes())}
                           for suffix in (".mrg", ".hed")},
               "records": records}
    (OUTPUT / f"{name}.dxt.json").write_text(json.dumps(receipt, ensure_ascii=False, indent=1) + "\n",
                                             encoding="utf-8")
    print(f"{name}: {stats} in {elapsed}s", flush=True)
    return receipt


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archives", default="allpachdml,allpacml,jallpachd,jallpac,allui,parts")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--out", type=Path, help="output assets directory")
    parser.add_argument("--base", type=Path,
                        help="PNG/JPEG pack to copy runtime files from (default: the 540p-Vita pack)")
    parser.add_argument("--keep", type=Path,
                        help="text file of image stems that must keep their original encoding")
    parser.add_argument("--nx", action="store_true",
                        help="wrap converted entries in NXCX (zlib); smaller pack, costs an inflate per load")
    args = parser.parse_args()
    global BASE, OUTPUT
    if args.out:
        OUTPUT = args.out.resolve()
    if args.base:
        BASE = args.base.resolve()
    if args.keep:
        for line in args.keep.read_text(encoding="utf-8").splitlines():
            if line and not line.startswith("#"):
                GRADED.add(stem(line))
        print(f"keep-list: {len(GRADED)} graded image stems stay decode-capable", flush=True)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    # Copy every runtime file except the six image archives (rewritten below);
    # allscr.mrg and script_text.mrg are .mrg but are not image archives.
    rewritten = {f"{name}{suffix}" for name in
                 ("allpachdml", "allpacml", "jallpachd", "jallpac", "allui", "parts")
                 for suffix in (".mrg", ".hed", ".nam")}
    for item in BASE.iterdir():
        target = OUTPUT / item.name
        if item.is_dir():
            shutil.copytree(item, target, dirs_exist_ok=True)
        elif item.name not in rewritten and item.name != "vita-540p-assets.json"                 and not item.name.startswith("vita-540p-assets"):
            shutil.copyfile(item, target)
    for name in args.archives.split(","):
        convert_archive(name.strip(), args.limit, args.jobs, args.nx)


if __name__ == "__main__":
    main()

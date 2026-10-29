#!/usr/bin/env python3
"""Fresh Vita-only stage for the exact EN/ZC bg_backlog PNG -> DXT1 pair.

Canonical PNG/JPEG assets are never written. Inherited files are immutable hard
links; only the private allui triple is rebuilt. This is not hardware proof.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("vita_backlog_cg_helpers", ROOT / "tools/stage-vita-gxt-cg.py")
cg = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = cg
spec.loader.exec_module(cg)
checked, converter = cg.checked, cg.converter
require, sha = checked.require, checked.sha
file_hash, signature, bounded_read = checked.file_hash, checked.signature, checked.bounded_read
MAX_ENTRY, SECTOR = checked.MAX_ENTRY, checked.SECTOR
PARENT = "vita-gxt-cg-stage-manifest.json"
PARENT_SHA = "45dee802c6ecb27f3f838c67819c1a01a2648d37940980f1da36e1d7f3cf00b0"
MANIFEST = "vita-gxt-backlog-stage-manifest.json"
PRIVATE = {"allui.hed", "allui.nam", "allui.mrg"}
TARGETS = [(6, "DISP_PARTS_EN.NXZ", "1c4acabc39582579437f6577fd804eadbbf76842d73a41944a9c5871cc1cfa15"),
           (7, "DISP_PARTS_ZC.NXZ", "b244013533092537524c440e15ee351265d2ff2e88c4f14ce38384dd7b6845bf")]
KIMG_SHA = "a34fa4f0918734efaa02ccff5fd214d8eb01d55b22d2c71475de2cadb257faa0"


def replace_backlog(atlas: bytes, replacement: bytes, atlas_sha: str, image_sha: str):
    require(sha(atlas) == atlas_sha, "selected atlas SHA mismatch")
    # Non-target source names can collide after the runtime's lossy decoding.
    # Keep their raw names and ordered records; only bg_backlog is a selector.
    items = checked.parse_atlas(atlas)
    matches = [item for item in items if item["name"] == "bg_backlog"]
    require(len(matches) == 1, "atlas must contain exactly one bg_backlog")
    original = matches[0]["image"]
    require(sha(original) == image_sha, "selected KIMG SHA mismatch")
    old, new = matches[0]["metadata"], checked.parse_kimg(replacement, replacement=True)
    require(old["codec"] == 2 and new["gxt"]["format"] == "0x85000000", "PNG -> DXT1 only")
    require(old["logical"] == new["logical"] and old["physical"] == new["physical"],
            "replacement dimensions differ")
    output = bytearray(atlas[:12])
    for item in items:
        if item["name"] == "bg_backlog":
            output.extend(struct.pack("<II", len(item["name_bytes"]), len(replacement)))
            output.extend(item["name_bytes"])
            output.extend(replacement)
        else:
            output.extend(item["record"])
    require(len(output) <= MAX_ENTRY, "rebuilt atlas exceeds bound")
    final = checked.parse_atlas(bytes(output))
    require(len(final) == len(items), "member count changed")
    for before, after in zip(items, final, strict=True):
        require(before["name_bytes"] == after["name_bytes"], "member order/name changed")
        require(after["image"] == replacement if before["name"] == "bg_backlog"
                else before["record"] == after["record"], "non-target member bytes changed")
    return bytes(output), original


def extent_groups(entries, exclude):
    groups = {}
    for e in entries:
        if e.index not in exclude and e.stored_sectors:
            groups.setdefault((e.offset, e.stored_sectors), []).append(e.index)
    return sorted(indices for indices in groups.values() if len(indices) > 1)


def rebuild_archive(source: Path, output: Path, changes: dict[int, bytes]) -> dict:
    archive = checked.Archive(source)
    require(changes and set(changes).issubset(range(len(archive.entries))), "invalid changed entry indices")
    converted = {index: checked.wrap_nx(data) for index, data in changes.items()}
    originals = []
    for e in archive.entries:
        stored = archive.stored(e)
        raw, decoded = checked.unpack_nx(stored)
        # Validate even unselected KIMG/KATL structures without changing their
        # raw records, indexed palettes, transparency or nonzero sector padding.
        metadata = cg.describe(decoded)
        originals.append({"index": e.index, "name": e.name, "source_sector_sha256": sha(stored),
                          "source_decoded_sha256": sha(decoded), "source_metadata": metadata})
    hed, offset, relocated = bytearray(), 0, {}
    with (output / "allui.mrg").open("xb") as dst:
        for e, observed in zip(archive.entries, originals, strict=True):
            changed, key = e.index in changes, (e.offset, e.stored_sectors)
            if not changed and e.stored_sectors and key in relocated:
                new_offset, size = relocated[key]
            else:
                stored = archive.stored(e)
                require(sha(stored) == observed["source_sector_sha256"], "source changed while copying")
                data = converted[e.index] if changed else stored
                size, new_offset = cg.round_sector(len(data)) // SECTOR, offset
                require(size <= 0xffff and offset + size < 0xffffffff, "HED overflow")
                dst.write(data)
                dst.write(b"\0" * (size * SECTOR - len(data)))
                offset += size
                if not changed and size:
                    relocated[key] = (new_offset, size)
            unpacked = cg.round_sector(len(changes[e.index])) // SECTOR if changed else e.unpacked_sectors
            require(unpacked <= 0xffff, "HED decoded sector overflow")
            hed.extend(struct.pack("<IHH", new_offset, size, unpacked))
        dst.flush()
        os.fsync(dst.fileno())
    for name, data in (("allui.hed", bytes(hed) + archive.tail), ("allui.nam", archive.nam)):
        with (output / name).open("xb") as dst:
            dst.write(data)
            dst.flush()
            os.fsync(dst.fileno())
    final = checked.Archive(output)
    require(final.nam == archive.nam and final.tail == archive.tail and
            len(final.entries) == len(archive.entries), "NAM/HED terminal/count changed")
    records = []
    for before, after, observed in zip(archive.entries, final.entries, originals, strict=True):
        require(before.name == after.name and before.index == after.index, "entry order/name changed")
        src, dst = archive.stored(before), final.stored(after)
        src_raw, src_decoded = checked.unpack_nx(src)
        dst_raw, dst_decoded = checked.unpack_nx(dst)
        require(sha(src) == observed["source_sector_sha256"], "source changed after snapshot")
        changed = before.index in changes
        if changed:
            require(dst_decoded == changes[before.index] and dst_raw == converted[before.index] and
                    after.unpacked_sectors == cg.round_sector(len(dst_decoded)) // SECTOR,
                    "replacement archive bytes did not round trip")
        else:
            require(src == dst and src_raw == dst_raw and src_decoded == dst_decoded and
                    before.stored_sectors == after.stored_sectors and
                    before.unpacked_sectors == after.unpacked_sectors, "unselected entry changed")
        records.append({**observed, "changed": changed, "output_sector_sha256": sha(dst),
                        "output_decoded_sha256": sha(dst_decoded), "output_metadata": cg.describe(dst_decoded)})
    require(extent_groups(archive.entries, changes) == extent_groups(final.entries, changes),
            "unselected shared extents split or merged")
    return {"archive_entries": len(records), "changed_archive_entries": len(changes),
            "unchanged_entries_stored_and_decoded_exact": len(records) - len(changes),
            "reopened_all_entries": True, "exact_nam_and_hed_terminal": True,
            "records": records}


def read_targets(source: Path):
    archive = checked.Archive(source)
    require(len(archive.entries) == 70, "expected exactly 70 allui entries")
    selected = []
    for index, name, expected in TARGETS:
        e = archive.entries[index]
        require(e.name == name, "exact atlas index/name mismatch")
        _, atlas = checked.unpack_nx(archive.stored(e))
        require(sha(atlas) == expected, "exact source atlas SHA mismatch")
        items = checked.parse_atlas(atlas)
        matches = [item for item in items if item["name"] == "bg_backlog"]
        require(len(items) == 50 and len(matches) == 1 and sha(matches[0]["image"]) == KIMG_SHA,
                "exact 50-member backlog atlas/source KIMG mismatch")
        meta = matches[0]["metadata"]
        require(meta["logical"] == [1920, 1080] and meta["physical"] == [960, 540] and meta["codec"] == 2,
                "reviewed source dimensions/PNG codec mismatch")
        selected.append((index, name, atlas, matches[0]["image"], meta))
    require(selected[0][3] == selected[1][3], "EN/ZC backgrounds differ")
    return selected


def validate_conversion(path, data, original, source):
    receipt_path = path.with_suffix(".json")
    receipt = json.loads(bounded_read(receipt_path, 1024 * 1024))
    require(receipt.get("schema") == "vita-game-gxt-conversion-v1", "SDK conversion receipt required")
    out = receipt.get("artifacts", {}).get("output", {})
    require(Path(out.get("path", "")).resolve() == path and
            {k: out.get(k) for k in ("bytes", "sha256")} == {"bytes": len(data), "sha256": sha(data)},
            "replacement conversion identity mismatch")
    require(receipt.get("selected_texture") is None and receipt.get("selected_kimg_sha256") == sha(original)
            and receipt.get("source", {}).get("sha256") == sha(original)
            and receipt["source"].get("bytes") == len(original), "conversion source KIMG mismatch")
    require(receipt.get("logical") == source["logical"] and receipt.get("physical") == source["physical"]
            and receipt.get("codec") == "dxt1" and receipt.get("input_codec") == "PNG"
            and receipt.get("fully_opaque") is True and receipt.get("alpha_extrema") == [255, 255]
            and receipt.get("alter") == "srgb_encode(pow(srgb_decode(src_RGB),1/2.2)); alpha=src_alpha",
            "conversion opacity/default Alter/dimensions contract mismatch")
    with converter.Image.open(io.BytesIO(original[32:32 + source["payload_bytes"]])) as image:
        image.load()
        # Expansion before testing alpha preserves paletted PNG/tRNS semantics.
        require(image.format == "PNG" and list(image.size) == source["physical"] and
                image.convert("RGBA").getchannel("A").getextrema() == (255, 255),
                "actual PNG transparency or dimensions are incompatible with DXT1")
    for name, expected in (("converter", ROOT / "tools/vita-game-gxt.py"), ("psp2gxt", converter.SDK)):
        record = receipt.get("tool", {}).get(name, {})
        require(Path(record.get("path", "")).resolve() == expected.resolve() and
                {k: record.get(k) for k in ("bytes", "sha256")} == file_hash(expected),
                "converter/SDK tool identity mismatch")
    for name in ("dds", "gxt", "rgba"):
        record = receipt.get("artifacts", {}).get(name, {})
        actual = Path(record.get("path", ""))
        require(file_hash(actual) == {k: record.get(k) for k in ("bytes", "sha256")}, "conversion artifact changed")
        if name == "gxt":
            require(bounded_read(actual, MAX_ENTRY) == data[32:], "SDK GXT bytes mismatch")
        if name == "rgba":
            require(record["bytes"] == 960 * 540 * 4, "Alter buffer byte count mismatch")
    converter.validate_gxt(data[32:], (960, 540), "dxt1")
    return {"path": str(receipt_path), **file_hash(receipt_path), "schema": receipt["schema"],
            "cpu_decode": receipt.get("cpu_decode"), "hardware_verified": False}


def inventory(source):
    paths = []
    require(source.is_dir() and not source.is_symlink() and
            not (getattr(source.lstat(), "st_file_attributes", 0) & 0x400), "invalid source directory")
    seen = set()
    for parent, dirs, filenames in os.walk(source, followlinks=False):
        for name in dirs:
            p = Path(parent) / name
            require(not p.is_symlink() and not (getattr(p.lstat(), "st_file_attributes", 0) & 0x400),
                    "reparse directory in source")
        for name in filenames:
            p = Path(parent) / name
            checked.regular(p)
            rel = p.relative_to(source)
            cg.safe_relative(rel.as_posix())
            key = rel.as_posix().casefold()
            require(key not in seen and key != MANIFEST.casefold(), "duplicate/reserved source path")
            paths.append(rel)
            seen.add(key)
    return sorted(paths, key=lambda p: p.as_posix())


def stage(source, replacement, output):
    output = checked.destination(output)
    source, replacement = source.resolve(), replacement.resolve()
    require(source != output and not source.is_relative_to(output) and not output.is_relative_to(source),
            "overlapping source/output")
    require(source.is_relative_to(checked.WORK_ROOT) and not replacement.is_relative_to(output),
            "source must be private parent stage; replacement outside output")
    parent_id = file_hash(source / PARENT)
    require(parent_id["sha256"] == PARENT_SHA, "exact combined parent manifest SHA mismatch")
    parent = json.loads(bounded_read(source / PARENT, 64 * 1024 * 1024))
    require(parent.get("schema") == "vita-gxt-cg-stage-v1" and parent.get("verification", {}).get("all_pass") is True
            and Path(parent["output_assets"]).resolve() == source, "invalid parent CG stage")
    paths = inventory(source)
    snapshots = {p: signature(source / p) for p in paths}
    expected = {r["relative_path"]: r["output"] for r in parent["files"]}
    require(len(expected) == len(parent["files"]), "duplicate parent file record")
    expected[PARENT] = parent_id
    require(set(expected) == {p.as_posix() for p in paths}, "parent inventory mismatch")
    require(not any(p.as_posix() == "allui.compact.json" for p in paths), "stale allui audit in source")
    for number, p in enumerate(paths, 1):
        require(file_hash(source / p) == expected[p.as_posix()], f"parent source changed: {p}")
        if number % 3000 == 0:
            print(f"verified parent file hashes {number}/{len(paths)}", flush=True)
    selected = read_targets(source)
    replacement_id, replacement_snapshot = file_hash(replacement), signature(replacement)
    data = bounded_read(replacement, MAX_ENTRY)
    metadata = checked.parse_kimg(data, replacement=True)
    conversion = validate_conversion(replacement, data, selected[0][3], selected[0][4])
    changes, selections = {}, []
    for index, name, atlas, original, old in selected:
        changes[index], _ = replace_backlog(atlas, data, sha(atlas), KIMG_SHA)
        selections.append({"archive": "allui", "index": index, "name": name, "texture": "bg_backlog",
                           "source_atlas_sha256": sha(atlas), "source_kimg_sha256": sha(original),
                           "output_kimg_sha256": sha(data), "members": 50, "other_member_records_exact": 49,
                           "source_metadata": old, "output_metadata": metadata})
    require(all(signature(source / p) == snapshots[p] for p in paths), "source changed before staging")
    output.mkdir(parents=True, exist_ok=False)
    files = []
    for p in paths:
        name = p.as_posix()
        mode = ("private-exact-copy" if name.endswith(".nam") else "private-rebuilt") if name in PRIVATE else "hard-link"
        if mode == "hard-link":
            dst = output / p
            dst.parent.mkdir(parents=True, exist_ok=True)
            os.link(source / p, dst)
            require(os.path.samefile(source / p, dst), "inherited hard-link mismatch")
        files.append({"relative_path": name, "mode": mode, "source": expected[name]})
    verification = rebuild_archive(source, output, changes)
    records = verification.pop("records")
    for record in files:
        name = record["relative_path"]
        same = os.path.samefile(source / name, output / name)
        require(same == (record["mode"] == "hard-link"), "private archive ownership mismatch")
        record.update({"same_file": same, "output": expected[name] if same else file_hash(output / name)})
    require(all(signature(source / p) == snapshots[p] for p in paths) and paths == inventory(source),
            "source inventory/file identities changed")
    for name in PRIVATE:
        require(file_hash(source / name) == expected[name], "parent allui bytes changed")
    require(signature(replacement) == replacement_snapshot and file_hash(replacement) == replacement_id,
            "replacement changed while staging")
    verification.update({"all_pass": True, "private_archive_files": 3, "parent_manifest_exact": True,
                         "parent_jallpac_output_hashes_match": True,
                         "source_inventory_and_file_identities_unchanged": True})
    report = {"schema": "vita-gxt-backlog-stage-v1", "created_utc": datetime.now(timezone.utc).isoformat(),
              "scope": "two exact EN/ZC bg_backlog atlas members; default Alter DXT1 only",
              "runtime_verified": False, "hardware_verified": False,
              "source_assets": str(source), "output_assets": str(output),
              "parent_manifest": {"relative_path": PARENT, **parent_id, "schema": parent["schema"]},
              "tool": {"path": str(Path(__file__).resolve()), **file_hash(Path(__file__))},
              "selections": selections, "replacement": {"path": str(replacement), **replacement_id,
                                                            "conversion_receipt": conversion},
              "runtime_contract": {"default_alter_only": True, "fallback_provided": False,
                                   "production_assets_modified": False},
              "storage": {"private_archive_bytes": sum((output / n).stat().st_size for n in PRIVATE),
                          "allui_mrg_growth_bytes": (output / "allui.mrg").stat().st_size - expected["allui.mrg"]["bytes"],
                          "png_alter_peak_cdram_bytes": 4194304, "gxt_peak_cdram_bytes": 524288,
                          "scope": "static allocation accounting, not measured device performance"},
              "files": files, "archive_entries": records, "verification": verification}
    with (output / MANIFEST).open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(report, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
    print(json.dumps({"manifest": str(output / MANIFEST), "verification": verification,
                      "storage": report["storage"]}, ensure_ascii=False), flush=True)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-assets", required=True, type=Path)
    parser.add_argument("--replacement", type=Path)
    parser.add_argument("--output-assets", type=Path)
    parser.add_argument("--extract-input", type=Path)
    args = parser.parse_args()
    if args.extract_input:
        require(not args.replacement and not args.output_assets, "extract/stage modes are exclusive")
        target = args.extract_input.resolve()
        require(target.is_relative_to(checked.SAFE_ROOT) and not target.exists(), "input output must be fresh under .work/vita")
        selected = read_targets(args.source_assets.resolve())
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("xb") as stream:
            stream.write(selected[0][3])
        print(json.dumps({"path": str(target), **file_hash(target)}, ensure_ascii=False))
    else:
        require(args.replacement and args.output_assets, "replacement/output required")
        stage(args.source_assets, args.replacement, args.output_assets)


if __name__ == "__main__":
    main()

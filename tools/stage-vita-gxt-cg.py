#!/usr/bin/env python3
"""Stage one index-bound default-Alter DXT1 CG over the menu experiment.

The CLI only accepts the reviewed jallpac/1884/IMG1227.JPG source identity.
It creates a fresh private archive under .work/vita, never writes a source or
hard-linked output, and does not establish runtime or hardware compatibility.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass, replace
from datetime import datetime, timezone
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import struct
import sys
import tempfile
import traceback

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[1]
SAFE_ROOT = (ROOT / ".work" / "vita").resolve()
WORK_ROOT = SAFE_ROOT / "game-gxt-stage"
PARENT_MANIFEST = "vita-gxt-stage-manifest.json"
MANIFEST = "vita-gxt-cg-stage-manifest.json"
PARENT_SHA = "96cb0d6d5e3c9a1426408abc52aa97c344fe9b14017d19f7b73241104b4c76fa"


def load_tool(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / "tools" / filename)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


checked = load_tool("vita_cg_checked_menu", "stage-vita-gxt-menu.py")
converter = load_tool("vita_cg_existing_converter", "vita-game-gxt.py")
SECTOR, MAX_ENTRY = checked.SECTOR, checked.MAX_ENTRY
sha, require = checked.sha, checked.require
bounded_read, file_hash = checked.bounded_read, checked.file_hash
signature, destination = checked.signature, checked.destination


@dataclass(frozen=True)
class Selection:
    archive: str = "jallpac"
    index: int = 1884
    name: str = "IMG1227.JPG"
    source_sha256: str = "ac47dc8aff15d0bf957279e557e68b733c81c97e7f9421639433c9fa044b683b"
    entries: int = 22156
    parent_sha256: str = PARENT_SHA


def stage(source: Path, replacement: Path, output: Path,
          selection: Selection = Selection()) -> dict:
    output = destination(output)
    source, replacement = source.resolve(), replacement.resolve()
    require(source != output and not source.is_relative_to(output) and not output.is_relative_to(source),
            "source/output trees must not overlap")
    require(not replacement.is_relative_to(output), "replacement must be outside fresh output")
    require(selection.archive == "jallpac", "only the reviewed jallpac archive is supported")
    private = {f"{selection.archive}.{suffix}" for suffix in ("hed", "mrg", "nam")}
    omitted_name = f"{selection.archive}.compact.json"
    paths = inventory(source)
    require(private.issubset({path.as_posix() for path in paths}), "source needs jallpac archive triple")
    snapshots = {path: signature(source / path) for path in paths}
    parent_path = source / PARENT_MANIFEST
    parent_id = file_hash(parent_path)
    require(parent_id["sha256"] == selection.parent_sha256, "parent manifest SHA mismatch")
    parent = json.loads(bounded_read(parent_path, MAX_ENTRY).decode("utf-8"))
    require(parent.get("schema") == "vita-gxt-menu-stage-v1" and
            parent.get("verification", {}).get("all_pass") is True and
            parent.get("verification", {}).get("changed_archive_entries") == 1,
            "parent menu manifest has no complete validated stage")
    require(parent.get("selection", {}).get("archive") == "allui" and
            parent["selection"].get("index") == 11 and
            parent["selection"].get("name") == "MENU_PARTS_EN.NXZ" and
            parent["selection"].get("texture") == "menu_btn_en", "parent menu selection mismatch")
    parent_files = {}
    for record in parent.get("files", []):
        name = record.get("relative_path", "")
        safe_relative(name)
        require(name not in parent_files, "duplicate parent file record")
        expected = record.get("output", record)
        require(isinstance(expected.get("bytes"), int) and expected["bytes"] >= 0 and
                isinstance(expected.get("sha256"), str) and len(expected["sha256"]) == 64,
                "invalid parent file identity")
        parent_files[name] = {"bytes": expected["bytes"], "sha256": expected["sha256"]}
    require(set(parent_files) | {PARENT_MANIFEST} == {path.as_posix() for path in paths},
            "source inventory does not match parent menu manifest")
    require("allui.compact.json" not in parent_files, "stale allui audit in parent stage")
    require({f"allui.{suffix}" for suffix in ("hed", "mrg", "nam")}.issubset(parent_files),
            "parent menu archive triple missing")

    archive = Archive(source, selection.archive)
    require(len(archive.entries) == selection.entries, "archive entry count mismatch")
    require(0 <= selection.index < len(archive.entries), "target index outside archive")
    selected = archive.entries[selection.index]
    require(selected.name == selection.name, "target exact name mismatch at selected index")
    original_sectors = archive.stored(selected)
    original_raw, original = checked.unpack_nx(original_sectors)
    require(sha(original) == selection.source_sha256, "target source KIMG SHA mismatch")
    source_metadata = checked.parse_kimg(original)
    require(source_metadata["codec"] == 1, "reviewed target must be an opaque JPEG KIMG")
    replacement_id = file_hash(replacement)
    replacement_snapshot = signature(replacement)
    replacement_bytes = bounded_read(replacement, MAX_ENTRY)
    replacement_metadata = checked.parse_kimg(replacement_bytes, replacement=True)
    require(source_metadata["logical"] == replacement_metadata["logical"] and
            source_metadata["physical"] == replacement_metadata["physical"],
            "replacement dimensions differ from selected source")
    require(replacement_metadata["gxt"]["format"] == "0x85000000", "replacement must be SDK DXT1 GXT")
    conversion = validate_conversion(replacement, replacement_bytes, original,
                                     source_metadata, replacement_metadata)
    converted_nx = checked.wrap_nx(replacement_bytes)
    require(round_sector(len(converted_nx)) <= MAX_ENTRY, "replacement padded NX exceeds 32 MiB")

    # Validate every entry with bounded reads before creating any output. Keep
    # the digest of the entire stored sector extent, including nonzero padding.
    original_records = []
    with archive.mrg.open("rb") as stream:
        for entry in archive.entries:
            sectors = archive.stored(entry, stream)
            raw, decoded = checked.unpack_nx(sectors)
            metadata = describe(decoded)
            original_records.append({"index": entry.index, "name": entry.name,
                                     "source_offset_sectors": entry.offset,
                                     "source_stored_sectors": entry.stored_sectors,
                                     "source_unpacked_sectors": entry.unpacked_sectors,
                                     "source_sector_sha256": sha(sectors),
                                     "source_raw_sha256": sha(raw), "source_decoded_sha256": sha(decoded),
                                     "source_decoded_bytes": len(decoded), "source_metadata": metadata})
            if entry.index and entry.index % 4000 == 0:
                archive.unchanged()
                print(f"prevalidated {entry.index}/{len(archive.entries)} jallpac entries", flush=True)
    archive.unchanged()

    # The parent manifest cryptographically binds every inherited file; verify
    # those bytes before linking. No existing output path is ever opened to write.
    identities = {}
    for number, path in enumerate(paths, 1):
        actual = file_hash(source / path)
        expected = parent_id if path.as_posix() == PARENT_MANIFEST else parent_files[path.as_posix()]
        require(actual == expected, f"source content differs from parent: {path}")
        identities[path.as_posix()] = actual
        if number % 2000 == 0:
            print(f"verified parent file hashes {number}/{len(paths)}", flush=True)
    source_archive = {suffix: identities[f"jallpac.{suffix}"] for suffix in ("hed", "mrg", "nam")}
    require(all(signature(source / path) == snapshots[path] for path in paths), "source changed during validation")
    output.mkdir(parents=True, exist_ok=False)
    files, omitted = [], []
    for path in paths:
        name = path.as_posix()
        if name == omitted_name:
            omitted.append({"relative_path": name, **identities[name],
                            "reason": "source compact audit does not describe the selected SDK DXT1 entry"})
            continue
        mode = ("private-exact-copy" if name.endswith(".nam") else "private-rebuilt") if name in private else "hard-link"
        record = {"relative_path": name, "mode": mode, "source": identities[name]}
        if mode == "hard-link":
            dst = output / path
            dst.parent.mkdir(parents=True, exist_ok=True)
            os.link(source / path, dst)
            require(os.path.samefile(source / path, dst) and signature(dst) == snapshots[path],
                    f"hard-link identity mismatch: {name}")
            record["output"] = identities[name]
        files.append(record)

    offset, hed, relocated = 0, bytearray(), {}
    with archive.mrg.open("rb") as src, (output / "jallpac.mrg").open("xb") as dst:
        for entry, observed in zip(archive.entries, original_records, strict=True):
            changed = entry.index == selection.index
            key = (entry.offset, entry.stored_sectors)
            if not changed and entry.stored_sectors and key in relocated:
                new_offset, stored = relocated[key]
            else:
                if changed:
                    data = converted_nx
                else:
                    data = archive.stored(entry, src)
                    require(sha(data) == observed["source_sector_sha256"], "source sectors changed before copying")
                stored = round_sector(len(data)) // SECTOR
                new_offset = offset
                require(stored <= 0xffff and offset + stored < 0xffffffff,
                        "rebuilt HED stored sector or offset overflow")
                dst.write(data)
                dst.write(b"\0" * (stored * SECTOR - len(data)))
                offset += stored
                if not changed and stored:
                    relocated[key] = (new_offset, stored)
            unpacked = round_sector(len(replacement_bytes)) // SECTOR if changed else entry.unpacked_sectors
            require(unpacked <= 0xffff, "rebuilt HED decoded sector overflow")
            hed.extend(struct.pack("<IHH", new_offset, stored, unpacked))
        dst.flush()
        os.fsync(dst.fileno())
    for name, data in (("jallpac.hed", bytes(hed) + archive.tail), ("jallpac.nam", archive.nam)):
        with (output / name).open("xb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    archive.unchanged()
    reopened = Archive(output, selection.archive)
    require(len(reopened.entries) == len(archive.entries) and reopened.nam == archive.nam and
            reopened.tail == archive.tail, "reopened NAM, entry count or HED terminal changed")

    records = []
    with archive.mrg.open("rb") as src, reopened.mrg.open("rb") as dst:
        for before, after, observed in zip(archive.entries, reopened.entries, original_records, strict=True):
            require(before.name == after.name and before.index == after.index, "archive entry order/name changed")
            source_sectors, final_sectors = archive.stored(before, src), reopened.stored(after, dst)
            source_raw, source_decoded = checked.unpack_nx(source_sectors)
            final_raw, final_decoded = checked.unpack_nx(final_sectors)
            require(sha(source_sectors) == observed["source_sector_sha256"] and
                    sha(source_decoded) == observed["source_decoded_sha256"], "source entry changed after snapshot")
            changed = before.index == selection.index
            if changed:
                require(final_decoded == replacement_bytes and final_raw == converted_nx and
                        after.unpacked_sectors == round_sector(len(replacement_bytes)) // SECTOR,
                        "selected replacement NX/decoded bytes did not round trip exactly")
            else:
                require(source_sectors == final_sectors and source_raw == final_raw and source_decoded == final_decoded and
                        before.stored_sectors == after.stored_sectors and before.unpacked_sectors == after.unpacked_sectors,
                        f"non-target stored/decoded bytes or size changed: {before.index} {before.name}")
            records.append({**observed, "changed": changed, "output_offset_sectors": after.offset,
                            "output_stored_sectors": after.stored_sectors,
                            "output_unpacked_sectors": after.unpacked_sectors,
                            "output_sector_sha256": sha(final_sectors), "output_raw_sha256": sha(final_raw),
                            "output_decoded_sha256": sha(final_decoded), "output_decoded_bytes": len(final_decoded),
                            "output_metadata": describe(final_decoded),
                            "unchanged_sector_and_decoded_exact": not changed})
            if before.index and before.index % 4000 == 0:
                archive.unchanged()
                reopened.unchanged()
                print(f"reopened and verified {before.index}/{len(records)}/{len(archive.entries)} entries", flush=True)
    archive.unchanged()
    reopened.unchanged()
    shared = shared_extents(archive.entries, exclude=selection.index)
    output_shared = shared_extents(reopened.entries, exclude=selection.index)
    require([group["indices"] for group in shared] == [group["indices"] for group in output_shared],
            "non-target shared extent groups were split or merged")
    duplicates = duplicate_names(archive.entries)
    require(duplicates == duplicate_names(reopened.entries), "duplicate-name order or membership changed")
    output_archive = {}
    for record in files:
        src, dst = source / record["relative_path"], output / record["relative_path"]
        same = os.path.samefile(src, dst)
        require(same == (record["mode"] == "hard-link"), "private/linked inode ownership mismatch")
        if not same:
            record["output"] = file_hash(dst)
            suffix = dst.suffix[1:]
            output_archive[suffix] = record["output"]
            if suffix == "nam":
                require(record["source"] == record["output"], "private NAM copy differs")
        stat = dst.stat()
        record.update({"same_file": same, "device": str(stat.st_dev), "file_id": str(stat.st_ino),
                       "link_count_after": stat.st_nlink})
    require(file_hash(archive.mrg) == source_archive["mrg"], "source MRG full-file hash changed")
    require(all(signature(source / path) == snapshots[path] for path in paths), "source file identities changed")
    require(paths == inventory(source), "source inventory changed")
    require(signature(replacement) == replacement_snapshot and file_hash(replacement) == replacement_id,
            "replacement changed during staging")
    require(file_hash(output / PARENT_MANIFEST) == parent_id, "parent manifest was not preserved exactly")
    for suffix in ("hed", "mrg", "nam"):
        name = f"allui.{suffix}"
        require(file_hash(output / name) == parent_files[name], "inherited menu output hash mismatch")
    require(not (output / omitted_name).exists(), "stale jallpac audit copied")
    payload = replacement_metadata["gxt"]["payload_bytes"]
    linear_pitch = (source_metadata["physical"][0] + 7) & ~7
    linear_allocation = align_up(linear_pitch * source_metadata["physical"][1] * 4, 256 * 1024)
    private_paths = [output / record["relative_path"] for record in files if record["mode"] != "hard-link"]
    storage = {"private_archive_bytes": sum(path.stat().st_size for path in private_paths),
               "private_archive_allocated_bytes": sum(allocated_bytes(path) for path in private_paths),
               "hard_link_new_payload_bytes": 0, "manifest_allocation_included": False,
               "source_mrg_bytes": source_archive["mrg"]["bytes"], "output_mrg_bytes": output_archive["mrg"]["bytes"],
               "archive_mrg_growth_bytes": output_archive["mrg"]["bytes"] - source_archive["mrg"]["bytes"],
               "source_jpeg_payload_bytes": source_metadata["payload_bytes"],
               "replacement_kimg_bytes": len(replacement_bytes),
               "replacement_vs_jpeg_payload_growth_bytes": len(replacement_bytes) - source_metadata["payload_bytes"],
               "source_target_sector_bytes": len(original_sectors),
               "output_target_sector_bytes": records[selection.index]["output_stored_sectors"] * SECTOR,
               "compressed_gxm_payload_bytes": payload, "compressed_cdram_allocation_bytes": align_up(payload, 256 * 1024),
               "linear_rgba_cdram_baseline_bytes": linear_allocation,
               "estimated_native_cdram_saving_bytes": linear_allocation - align_up(payload, 256 * 1024),
               "allocation_method": "GetFileInformationByHandleEx FileStandardInfo.AllocationSize" if os.name == "nt" else "stat.st_blocks*512",
               "allocation_scope": "measured filesystem allocation of 3 private archive files; GXM bytes are static native accounting, not hardware proof"}
    report = {"schema": "vita-gxt-cg-stage-v1", "created_utc": datetime.now(timezone.utc).isoformat(),
              "status": "partial experimental Vita-only; no hardware proof", "runtime_verified": False,
              "scope": "one index-bound opaque CG replaces JPEG with SDK DXT1; default Alter only",
              "source_assets": str(source), "output_assets": str(output),
              "source_writes": "none; hard-link counts/ctime may change; source content is never opened for writing",
              "hard_link_constraint": "inherited files share storage; copy before any subsequent editing",
              "parent_manifest": {"relative_path": PARENT_MANIFEST, **parent_id, "schema": parent["schema"]},
              "tool": {"path": str(Path(__file__).resolve()), **file_hash(Path(__file__)),
                       "checked_menu_helper": {"path": str(ROOT / "tools/stage-vita-gxt-menu.py"),
                                               **file_hash(ROOT / "tools/stage-vita-gxt-menu.py")}},
              "selection": {"archive": selection.archive, "index": selection.index, "name": selection.name,
                            "source_kimg_sha256": sha(original), "output_kimg_sha256": sha(replacement_bytes),
                            "source_metadata": source_metadata, "output_metadata": replacement_metadata},
              "runtime_contract": {"default_alter_only": True, "cpu_grade_supported": False,
                                   "raw_supported": False, "language_strip_supported": False,
                                   "fallback_provided": False, "production_assets_modified": False},
              "replacement": {"path": str(replacement), **replacement_id, "metadata": replacement_metadata,
                              "conversion_receipt": conversion},
              "source_archive": source_archive, "output_archive": output_archive,
              "omitted": omitted, "files": files, "archive_entries": records,
              "preserved_shared_extents": shared, "preserved_duplicate_names": duplicates,
              "storage": storage,
              "verification": {"all_pass": True, "archive_entries": len(records), "changed_archive_entries": 1,
                               "unchanged_entries_stored_and_decoded_exact": len(records) - 1,
                               "reopened_all_entries": True, "exact_nam_and_hed_terminal": True,
                               "private_archive_files": len(private_paths),
                               "hard_link_files": sum(record["mode"] == "hard-link" for record in files),
                               "stale_jallpac_compact_audit_copied": False, "parent_manifest_exact": True,
                               "parent_allui_output_hashes_match": True,
                               "source_inventory_and_file_identities_unchanged": True,
                               "preserved_shared_extent_groups": len(shared), "preserved_duplicate_name_groups": len(duplicates)}}
    with (output / MANIFEST).open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(report, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    print(f"verified {len(records)} entries; independent CG manifest written", flush=True)
    return report


def align_up(value: int, alignment: int) -> int:
    return (value + alignment - 1) // alignment * alignment


def round_sector(value: int) -> int:
    return align_up(value, SECTOR)


def safe_relative(name: str) -> None:
    path = Path(name)
    require(name and not path.is_absolute() and not path.drive and ".." not in path.parts and
            path.as_posix() == name and "\\" not in name and ":" not in name,
            "invalid relative file path")


def inventory(root: Path) -> list[Path]:
    require(root.is_dir() and not root.is_symlink() and
            not (getattr(root.lstat(), "st_file_attributes", 0) & 0x400), "source assets absent or reparse root")
    paths, seen = [], set()
    for parent, dirs, filenames in os.walk(root, followlinks=False):
        for name in dirs:
            path = Path(parent) / name
            require(not path.is_symlink() and not (getattr(path.lstat(), "st_file_attributes", 0) & 0x400),
                    "source contains reparse directory")
        for name in filenames:
            path = Path(parent) / name
            checked.regular(path)
            relative = path.relative_to(root)
            key = relative.as_posix().casefold()
            require(key not in seen and key != MANIFEST.casefold(), "ambiguous or reserved source file path")
            seen.add(key)
            paths.append(relative)
    return sorted(paths, key=lambda path: path.as_posix())


class Archive:
    """Bounded indexed HED/NAM reader. Duplicate names are data, never selectors."""
    def __init__(self, root: Path, archive: str):
        require(archive == "jallpac", "unsupported archive")
        self.mrg = root / f"{archive}.mrg"
        self.hed = bounded_read(root / f"{archive}.hed", 4 * 1024 * 1024)
        self.nam = bounded_read(root / f"{archive}.nam", 16 * 1024 * 1024)
        self.mrg_signature = signature(self.mrg)
        require(len(self.hed) % 8 == 0 and len(self.nam) % 32 == 0, "HED/NAM alignment mismatch")
        require(self.mrg_signature[2] % SECTOR == 0, "MRG sector alignment mismatch")
        self.entries, intervals, terminal = [], set(), None
        for index in range(len(self.hed) // 8):
            row = self.hed[index * 8:index * 8 + 8]
            if row == b"\xff" * 8:
                terminal = index * 8
                require(self.hed[terminal:] == b"\xff" * (len(self.hed) - terminal),
                        "bytes follow HED terminator")
                break
            offset, stored, unpacked = struct.unpack("<IHH", row)
            require(offset != 0xffffffff, "partial HED terminator")
            require((index + 1) * 32 <= len(self.nam), "missing archive name record")
            raw = self.nam[index * 32:index * 32 + 32]
            name = raw.split(b"\0", 1)[0].split(b"\r", 1)[0].decode("utf-8")
            require(name and not any(char in name for char in "/\\\n\r"), "invalid archive entry name")
            size = stored * SECTOR
            require(size <= MAX_ENTRY and unpacked * SECTOR <= MAX_ENTRY, "archive entry exceeds 32 MiB")
            require(offset * SECTOR <= self.mrg_signature[2] and offset * SECTOR + size <= self.mrg_signature[2],
                    "HED entry outside MRG")
            require(stored > 0 or (unpacked == 0 and name.casefold() == "nofile.exists"),
                    "zero non-placeholder entry")
            self.entries.append(checked.Entry(index, name, offset, stored, unpacked))
            if stored:
                intervals.add((offset, offset + stored))
        require(terminal is not None, "HED complete terminator absent")
        self.tail = self.hed[terminal:]
        previous = 0
        for start, end in sorted(intervals):
            require(start >= previous, "partial overlapping archive extents")
            previous = end

    def unchanged(self):
        require(signature(self.mrg) == self.mrg_signature, "source MRG changed")

    def stored(self, entry, stream=None) -> bytes:
        if stream is None:
            with self.mrg.open("rb") as opened:
                return self.stored(entry, opened)
        stream.seek(entry.offset * SECTOR)
        data = stream.read(entry.stored_sectors * SECTOR)
        require(len(data) == entry.stored_sectors * SECTOR, "short archive entry read")
        return data


def describe(data: bytes) -> dict:
    if data.startswith(b"KIMG"):
        return {"kind": "KIMG", **checked.parse_kimg(data)}
    if data.startswith(b"KATL"):
        members = checked.parse_atlas(data)
        # The complete decoded-byte equality below includes every palette and
        # sub-image record, independently of this compact metadata summary.
        return {"kind": "KATL", "members": len(members),
                "member_metadata_sha256": sha(json.dumps([
                    {"name": member["name"], "metadata": member["metadata"],
                     "record_sha256": sha(member["record"])} for member in members],
                    ensure_ascii=True, separators=(",", ":")).encode())}
    return {"kind": "other", "decoded_bytes": len(data)}


def shared_extents(entries, *, exclude: int) -> list[dict]:
    groups = {}
    for entry in entries:
        if entry.index != exclude and entry.stored_sectors:
            groups.setdefault((entry.offset, entry.stored_sectors), []).append(entry.index)
    return sorted([{"indices": indices, "offset_sectors": extent[0], "stored_sectors": extent[1]}
                   for extent, indices in groups.items() if len(indices) > 1], key=lambda row: row["indices"])


def duplicate_names(entries) -> list[dict]:
    groups = {}
    for entry in entries:
        groups.setdefault(entry.name, []).append(entry.index)
    return [{"name": name, "indices": indices} for name, indices in sorted(groups.items()) if len(indices) > 1]


def validate_conversion(path: Path, data: bytes, original: bytes, source: dict, replacement: dict) -> dict:
    receipt_path = path.with_suffix(".json")
    receipt_id = file_hash(receipt_path)
    receipt = json.loads(bounded_read(receipt_path, 1024 * 1024).decode("utf-8"))
    require(receipt.get("schema") == "vita-game-gxt-conversion-v1", "SDK conversion receipt required")
    identity = {"bytes": len(data), "sha256": sha(data)}
    output = receipt.get("artifacts", {}).get("output", {})
    require({key: output.get(key) for key in identity} == identity and
            Path(output.get("path", "")).resolve() == path,
            "replacement identity differs from conversion receipt")
    require(receipt.get("selected_kimg_sha256") == sha(original) and
            receipt.get("source", {}).get("sha256") == sha(original) and
            receipt["source"].get("bytes") == len(original) and receipt.get("selected_texture") is None,
            "converter source identity differs from selected archive KIMG")
    require(receipt.get("logical") == source["logical"] and receipt.get("physical") == source["physical"] and
            receipt.get("codec") == "dxt1" and receipt.get("fully_opaque") is True and
            receipt.get("alpha_extrema") == [255, 255] and receipt.get("input_codec") == "JPEG" and
            receipt.get("alter") == "srgb_encode(pow(srgb_decode(src_RGB),1/2.2)); alpha=src_alpha",
            "conversion dimensions, opacity or default Alter contract mismatch")
    with converter.Image.open(io.BytesIO(original[32:32 + source["payload_bytes"]])) as image:
        image.load()
        require(image.format == "JPEG" and list(image.size) == source["physical"] and
                image.convert("RGBA").getchannel("A").getextrema() == (255, 255),
                "actual selected JPEG is not opaque at declared dimensions")
    for name, required_path in (("converter", ROOT / "tools/vita-game-gxt.py"), ("psp2gxt", converter.SDK)):
        record = receipt.get("tool", {}).get(name, {})
        require(Path(record.get("path", "")).resolve() == required_path.resolve() and
                {key: record.get(key) for key in ("bytes", "sha256")} == file_hash(required_path),
                f"conversion {name} executable/source identity mismatch")
    for name in ("dds", "gxt", "rgba"):
        record = receipt.get("artifacts", {}).get(name, {})
        artifact_path = Path(record.get("path", ""))
        require(file_hash(artifact_path) == {key: record.get(key) for key in ("bytes", "sha256")},
                f"conversion {name} artifact hash mismatch")
        if name == "gxt":
            require(bounded_read(artifact_path, MAX_ENTRY) == data[32:], "replacement differs from exact SDK GXT bytes")
        elif name == "rgba":
            require(record["bytes"] == source["physical"][0] * source["physical"][1] * 4,
                    "conversion Alter RGBA byte length mismatch")
    converter.validate_gxt(data[32:], tuple(source["physical"]), "dxt1")
    return {"path": str(receipt_path), **receipt_id, "schema": receipt["schema"],
            "converter": receipt["tool"]["converter"], "psp2gxt": receipt["tool"]["psp2gxt"],
            "fully_opaque": True, "codec": "dxt1", "alter": receipt["alter"],
            "hardware_verified": False}


def allocated_bytes(path: Path) -> int:
    if os.name == "nt":
        import ctypes
        from ctypes import wintypes
        class FileStandardInfo(ctypes.Structure):
            _fields_ = [("AllocationSize", ctypes.c_longlong), ("EndOfFile", ctypes.c_longlong),
                        ("NumberOfLinks", wintypes.DWORD), ("DeletePending", ctypes.c_ubyte),
                        ("Directory", ctypes.c_ubyte)]
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD,
                                      ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
        kernel.CreateFileW.restype = wintypes.HANDLE
        kernel.GetFileInformationByHandleEx.argtypes = [wintypes.HANDLE, ctypes.c_int,
                                                       ctypes.c_void_p, wintypes.DWORD]
        kernel.GetFileInformationByHandleEx.restype = wintypes.BOOL
        kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        kernel.CloseHandle.restype = wintypes.BOOL
        # FILE_READ_ATTRIBUTES, share read/write/delete, OPEN_EXISTING. A
        # regular-file check precedes the handle open; this is read-only.
        checked.regular(path)
        handle = kernel.CreateFileW(str(path), 0x80, 7, None, 3, 0, None)
        if handle == ctypes.c_void_p(-1).value:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            info = FileStandardInfo()
            if not kernel.GetFileInformationByHandleEx(handle, 1, ctypes.byref(info), ctypes.sizeof(info)):
                raise ctypes.WinError(ctypes.get_last_error())
            require(info.AllocationSize >= 0 and info.EndOfFile == path.stat().st_size and not info.Directory,
                    "filesystem allocation observation is invalid or changed")
            return info.AllocationSize
        finally:
            if not kernel.CloseHandle(handle):
                raise ctypes.WinError(ctypes.get_last_error())
    stat = path.stat()
    require(hasattr(stat, "st_blocks"), "host filesystem allocation accounting unavailable")
    return stat.st_blocks * 512


def fixture(directory: Path) -> tuple[Path, Path, Selection]:
    """Tiny real JPEG + SDK DXT1, duplicate names, exact alias and raw padding."""
    source = directory / "source-assets"
    source.mkdir(parents=True)
    image = converter.Image.new("RGB", (8, 8), (83, 149, 226))
    stream = io.BytesIO()
    image.save(stream, format="JPEG", quality=88)
    target = checked.image_bytes(logical=(16, 12), physical=(8, 8), codec=1,
                                 payload=stream.getvalue())
    input_path = directory / "input.kimg"
    input_path.write_bytes(target)
    replacement = directory / "replacement.kimg"
    converter.convert(input_path, replacement, "dxt1")
    stream = io.BytesIO()
    converter.Image.new("RGBA", (4, 4), (20, 40, 60, 173)).save(stream, format="PNG")
    alpha = checked.image_bytes(logical=(8, 8), physical=(4, 4), codec=2,
                                payload=stream.getvalue())
    raw = [b"", checked.wrap_nx(target), checked.wrap_nx(alpha),
           checked.wrap_nx(b"distinct thumbnail"), None, b"unwrapped raw" + b"\xa5" * 2035, b""]
    names = ["NOFILE.EXISTS", "IMG1227.JPG", "IMG2328.NXZ", "IMG2328.NXZ",
             "ALIAS_EN.NXZ", "RAW.BIN", "NOFILE.EXISTS"]
    hed, nam, mrg, rows = bytearray(), bytearray(), bytearray(), []
    offset = 0
    for index, (data, name) in enumerate(zip(raw, names, strict=True)):
        if data is None:
            row = rows[2]
        else:
            sectors = (len(data) + SECTOR - 1) // SECTOR
            row = (offset, sectors, (len(target) + SECTOR - 1) // SECTOR if index == 1 else sectors)
            mrg.extend(data + b"\xa5" * (sectors * SECTOR - len(data)))
            offset += sectors
        rows.append(row)
        hed.extend(struct.pack("<IHH", *row))
        nam.extend(name.encode().ljust(30, b"\0") + b"\r\n")
    hed.extend(b"\xff" * 16)
    nam.extend(b"\xff" * 64)
    for suffix, data in (("hed", hed), ("nam", nam), ("mrg", mrg)):
        (source / f"jallpac.{suffix}").write_bytes(data)
    (source / "jallpac.compact.json").write_bytes(b'{"stale":true}')
    (source / "other.bin").write_bytes(b"immutable linked resource")
    for suffix in ("hed", "nam", "mrg"):
        (source / f"allui.{suffix}").write_bytes(b"already staged menu " + suffix.encode())
    parent = {"schema": "vita-gxt-menu-stage-v1", "runtime_verified": False,
              "selection": {"archive": "allui", "index": 11, "name": "MENU_PARTS_EN.NXZ", "texture": "menu_btn_en"},
              "verification": {"all_pass": True, "changed_archive_entries": 1},
              "files": [{"relative_path": p.name, "mode": "hard-link", **file_hash(p)}
                        for p in sorted(source.iterdir())]}
    parent_path = source / PARENT_MANIFEST
    parent_path.write_text(json.dumps(parent) + "\n", encoding="utf-8")
    return source, replacement, Selection(index=1, source_sha256=sha(target), entries=7,
                                           parent_sha256=sha(parent_path.read_bytes()))


def self_test() -> int:
    WORK_ROOT.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="cg-self-test-", dir=WORK_ROOT))
    source, replacement, selection = fixture(directory)
    checks = []
    counter = 0

    def check(label, callback):
        try:
            callback()
            checks.append({"name": label, "pass": True})
        except Exception as exc:
            checks.append({"name": label, "pass": False, "error": f"{type(exc).__name__}: {exc}"})
            traceback.print_exc()

    def positive():
        output = directory / "positive-assets"
        report = stage(source, replacement, output, selection)
        assert report["verification"]["all_pass"]
        assert report["verification"]["archive_entries"] == 7
        assert report["verification"]["unchanged_entries_stored_and_decoded_exact"] == 6
        assert report["verification"]["preserved_shared_extent_groups"] == 1
        assert report["verification"]["preserved_duplicate_name_groups"] == 2
        assert report["verification"]["exact_nam_and_hed_terminal"] is True
        assert not (output / "jallpac.compact.json").exists()
        assert (output / PARENT_MANIFEST).read_bytes() == (source / PARENT_MANIFEST).read_bytes()
        assert os.path.samefile(source / "other.bin", output / "other.bin")
        assert not os.path.samefile(source / "jallpac.mrg", output / "jallpac.mrg")
        result_hed = (output / "jallpac.hed").read_bytes()
        assert result_hed[2 * 8:2 * 8 + 8] == result_hed[4 * 8:4 * 8 + 8]
        assert (output / "jallpac.nam").read_bytes() == (source / "jallpac.nam").read_bytes()

    check("private CG rewrite preserves duplicate names, aliases, raw padding, NAM and terminal", positive)

    def allocation_rounding():
        probe = directory / "allocation-probe.bin"
        probe.write_bytes(bytes(5003))
        actual = allocated_bytes(probe)
        # This uncompressed ordinary file cannot report its non-aligned EOF
        # as its allocation. The former GetCompressedFileSizeW call did so.
        assert actual >= 5003 and actual % 512 == 0, actual
    check("filesystem allocation query reports allocated sectors rather than logical EOF", allocation_rounding)

    def rejected(label, *, selected=selection, src=source, repl=replacement, output=None,
                 reason=""):
        nonlocal counter
        counter += 1
        out = output or directory / f"refused-{counter}-assets"
        before = {p.name: file_hash(p) for p in src.iterdir() if p.is_file()}

        def run():
            try:
                stage(src, repl, out, selected)
            except (ValueError, OSError) as exc:
                assert not reason or reason in str(exc), str(exc)
                assert output is not None or not out.exists(), "refusal created an output directory"
                assert {p.name: file_hash(p) for p in src.iterdir() if p.is_file()} == before
                checks.append({"name": label + " rejection evidence", "pass": True,
                               "rejection": f"{type(exc).__name__}: {exc}"})
                return
            raise AssertionError("invalid staging request was accepted")
        check(label, run)

    rejected("wrong index", selected=replace(selection, index=2), reason="name")
    rejected("negative index", selected=replace(selection, index=-1), reason="index")
    rejected("index beyond archive", selected=replace(selection, index=999), reason="index")
    rejected("wrong exact name", selected=replace(selection, name="img1227.jpg"), reason="name")
    rejected("wrong source KIMG hash", selected=replace(selection, source_sha256="0" * 64), reason="SHA")
    rejected("wrong parent manifest hash", selected=replace(selection, parent_sha256="0" * 64), reason="parent")
    rejected("wrong archive", selected=replace(selection, archive="../jallpac"), reason="archive")
    existing = directory / "existing-assets"
    existing.mkdir()
    (existing / "marker").write_bytes(b"do not overwrite")
    rejected("existing output", output=existing, reason="exists")
    check("existing output marker remains exact", lambda: require((existing / "marker").read_bytes() == b"do not overwrite", "marker overwritten"))
    rejected("outside safe workspace path", output=ROOT / ".work/cg-forbidden-output", reason="fresh directory below")

    def corrupt_case(label, suffix, mutate, reason):
        nonlocal counter
        counter += 1
        case = directory / f"corrupt-{counter}"
        case.mkdir()
        altered = case / "assets"
        altered.mkdir()
        for original in source.iterdir():
            (altered / original.name).write_bytes(original.read_bytes())
        path = altered / f"jallpac.{suffix}"
        path.write_bytes(mutate(path.read_bytes()))
        # This is a deliberately different, fully bound synthetic parent; the
        # corrupt-archive checks must reach the reader, not just parent hashing.
        parent = json.loads((altered / PARENT_MANIFEST).read_text(encoding="utf-8"))
        for row in parent["files"]:
            if row["relative_path"] == path.name:
                row.update(file_hash(path))
        (altered / PARENT_MANIFEST).write_text(json.dumps(parent) + "\n", encoding="utf-8")
        spec = replace(selection, parent_sha256=sha((altered / PARENT_MANIFEST).read_bytes()))
        rejected(label, src=altered, selected=spec, reason=reason)

    def patch(data, offset, fmt, value):
        out = bytearray(data)
        struct.pack_into(fmt, out, offset, value)
        return bytes(out)

    corrupt_case("out-of-range HED", "hed", lambda b: patch(b, 8, "<I", 0xfffffffe), "outside MRG")
    corrupt_case("partial overlapping extents", "hed", lambda b: patch(patch(b, 2 * 8, "<I", 0), 2 * 8 + 4, "<H", 2), "overlap")
    corrupt_case("incomplete HED terminal", "hed", lambda b: b[:-9], "alignment")
    corrupt_case("nonterminal HED after sentinel", "hed", lambda b: b[:-8] + bytes(8), "terminator")
    corrupt_case("missing NAM row", "nam", lambda b: b[:32], "name record")
    corrupt_case("NX truncated declared payload", "mrg", lambda b: patch(b, 8, "<I", 32000), "payload")
    corrupt_case("NX decoded allocation bomb", "mrg", lambda b: patch(b, 4, "<I", MAX_ENTRY + 1), "length")
    corrupt_case("NX wrong magic", "mrg", lambda b: b"NX??" + b[4:], "magic")

    # An altered replacement must fail the source-bound converter receipt.
    bad = directory / "changed-replacement.kimg"
    changed_bytes = bytearray(replacement.read_bytes())
    changed_bytes[-1] ^= 1
    bad.write_bytes(changed_bytes)
    bad.with_suffix(".json").write_bytes(replacement.with_suffix(".json").read_bytes())
    rejected("replacement bytes do not match conversion receipt", repl=bad, reason="replacement")
    dim = directory / "wrong-dimensions.kimg"
    dim.write_bytes(patch(replacement.read_bytes(), 8, "<I", 17))
    dim.with_suffix(".json").write_bytes(replacement.with_suffix(".json").read_bytes())
    rejected("replacement logical dimensions mismatch", repl=dim, reason="dimensions")

    record = {"schema": "vita-gxt-cg-stage-self-test-v1", "created_utc": datetime.now(timezone.utc).isoformat(),
              "directory": str(directory), "tool_sha256": sha(Path(__file__).read_bytes()),
              "passed": sum(row["pass"] for row in checks), "total": len(checks),
              "all_pass": all(row["pass"] for row in checks), "checks": checks}
    (directory / "self-test.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: value for key, value in record.items() if key != "checks"}, indent=2))
    return 0 if record["all_pass"] else 1


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-assets", type=Path)
    parser.add_argument("--replacement", type=Path)
    parser.add_argument("--output-assets", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    supplied = (args.source_assets, args.replacement, args.output_assets)
    if args.self_test:
        if any(supplied):
            parser.error("--self-test is independent of staging arguments")
        return self_test()
    if not all(supplied):
        parser.error("--source-assets, --replacement and --output-assets are required")
    try:
        report = stage(*supplied)
    except (ValueError, OSError, NotImplementedError) as exc:
        print(f"CG stage refused: {exc}", file=sys.stderr)
        return 1
    print(json.dumps({"manifest": str(args.output_assets.resolve() / MANIFEST),
                      "verification": report["verification"], "storage": report["storage"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

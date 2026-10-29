#!/usr/bin/env python3
"""Stage an experimental Vita asset tree with exactly one converted menu KIMG.

All destinations must be absent and below this workspace's .work/vita. Files
shared with the source are hard links and are never opened for writing. This
tool does not establish GXM sampling, visual quality, or hardware correctness.
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import struct
import sys
import tempfile
import traceback
import zlib

ROOT = Path(__file__).resolve().parents[1]
SAFE_ROOT = (ROOT / ".work" / "vita").resolve()
WORK_ROOT = SAFE_ROOT / "game-gxt-stage"
SECTOR = 2048
MAX_ENTRY = 32 * 1024 * 1024
TARGET_INDEX = 11
TARGET_ENTRY = "MENU_PARTS_EN.NXZ"
TARGET_TEXTURE = "menu_btn_en"
PRIVATE = {"allui.hed", "allui.mrg", "allui.nam"}
OMIT = "allui.compact.json"
MANIFEST = "vita-gxt-stage-manifest.json"


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def regular(path: Path) -> os.stat_result:
    stat = path.lstat()
    require(path.is_file() and not path.is_symlink() and
            not (getattr(stat, "st_file_attributes", 0) & 0x400),
            f"expected a regular file without reparse points: {path}")
    return stat


def signature(path: Path) -> tuple[int, int, int, int]:
    stat = regular(path)
    return stat.st_dev, stat.st_ino, stat.st_size, stat.st_mtime_ns


def bounded_read(path: Path, limit: int) -> bytes:
    before = signature(path)
    require(before[2] <= limit, f"file exceeds {limit} byte bound: {path}")
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    require(len(data) == before[2] and signature(path) == before,
            f"file changed or short read: {path}")
    return data


def file_hash(path: Path) -> dict:
    before = signature(path)
    digest = hashlib.sha256()
    total = 0
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
            total += len(block)
    require(total == before[2] and signature(path) == before,
            f"file changed while hashing: {path}")
    return {"bytes": total, "sha256": digest.hexdigest()}


def destination(path: Path) -> Path:
    resolved = path.resolve()
    require(resolved != SAFE_ROOT and resolved.is_relative_to(SAFE_ROOT),
            f"output must be a fresh directory below {SAFE_ROOT}")
    require(not os.path.lexists(path) and not os.path.lexists(resolved),
            f"output already exists: {path}")
    return resolved


def unpack_nx(sectors: bytes) -> tuple[bytes, bytes]:
    """Return exact stored bytes (without padding), and bounded decoded bytes."""
    require(len(sectors) <= MAX_ENTRY, "stored entry exceeds 32 MiB")
    if not sectors.startswith(b"NX"):
        return sectors, sectors
    require(len(sectors) >= 16 and sectors[:4] in (b"NXCX", b"NXGX"),
            "invalid or truncated NX header/magic")
    unpacked, compressed = struct.unpack_from("<II", sectors, 4)
    require(unpacked <= MAX_ENTRY and compressed > 0 and compressed <= MAX_ENTRY - 16,
            "invalid or oversized NX declared lengths")
    end = 16 + compressed
    require(end <= len(sectors), "NX compressed payload exceeds stored sectors")
    decoder = zlib.decompressobj(31 if sectors[:4] == b"NXGX" else 15)
    try:
        decoded = decoder.decompress(sectors[16:end], unpacked + 1)
    except zlib.error as exc:
        raise ValueError(f"invalid NX stream: {exc}") from exc
    require(len(decoded) == unpacked and decoder.eof and
            not decoder.unused_data and not decoder.unconsumed_tail,
            "NX length, checksum, stream end or compressed consumption mismatch")
    return sectors[:end], decoded


@dataclass(frozen=True)
class Entry:
    index: int
    name: str
    offset: int
    stored_sectors: int
    unpacked_sectors: int


class Archive:
    def __init__(self, root: Path):
        self.root = root
        self.hed = bounded_read(root / "allui.hed", 1024 * 1024)
        self.nam = bounded_read(root / "allui.nam", 4 * 1024 * 1024)
        self.mrg_signature = signature(root / "allui.mrg")
        self.entries = []
        require(len(self.hed) % 8 == 0 and len(self.nam) % 32 == 0,
                "archive HED/NAM record alignment mismatch")
        seen, intervals, terminal = set(), set(), None
        for index in range(len(self.hed) // 8):
            row = self.hed[index * 8:index * 8 + 8]
            if row == b"\xff" * 8:
                terminal = index * 8
                require(self.hed[terminal:] == b"\xff" * (len(self.hed) - terminal),
                        "non-terminal HED bytes follow terminator")
                break
            offset, stored, unpacked = struct.unpack("<IHH", row)
            require(offset != 0xffffffff, "partial HED terminator")
            require((index + 1) * 32 <= len(self.nam), "missing archive name record")
            raw_name = self.nam[index * 32:index * 32 + 32]
            try:
                name = raw_name.split(b"\0", 1)[0].split(b"\r", 1)[0].decode("utf-8")
            except UnicodeDecodeError as exc:
                raise ValueError("invalid UTF-8 archive name") from exc
            require(bool(name) and not any(c in name for c in "\0/\\\n\r"), "invalid archive entry name")
            key = name.casefold()
            require(key == "nofile.exists" or key not in seen, "duplicate archive entry name")
            seen.add(key)
            size = stored * SECTOR
            require(size <= MAX_ENTRY and unpacked * SECTOR <= MAX_ENTRY,
                    "HED single entry exceeds 32 MiB budget")
            require(offset * SECTOR <= self.mrg_signature[2] and
                    offset * SECTOR + size <= self.mrg_signature[2], "HED entry outside MRG")
            require(stored > 0 or (unpacked == 0 and key == "nofile.exists"),
                    "zero-length non-placeholder entry")
            if stored:
                intervals.add((offset * SECTOR, offset * SECTOR + size))
            self.entries.append(Entry(index, name, offset, stored, unpacked))
        require(terminal is not None, "HED has no complete terminator")
        self.tail = self.hed[terminal:]
        previous_end = 0
        for start, end in sorted(intervals):
            # Exact shared extents are valid language aliases. A partial overlap
            # cannot safely be rewritten as a separate archive entry.
            require(start >= previous_end, "partially overlapping HED entry extents")
            previous_end = end
        require(len(self.entries) > TARGET_INDEX and
                self.entries[TARGET_INDEX].name == TARGET_ENTRY,
                f"archive index {TARGET_INDEX} must be exactly {TARGET_ENTRY}")

    def stored(self, entry: Entry) -> bytes:
        with (self.root / "allui.mrg").open("rb") as stream:
            stream.seek(entry.offset * SECTOR)
            data = stream.read(entry.stored_sectors * SECTOR)
        require(len(data) == entry.stored_sectors * SECTOR and
                signature(self.root / "allui.mrg") == self.mrg_signature,
                "MRG changed or short archive read")
        return data


def parse_kimg(data: bytes, *, replacement: bool = False) -> dict:
    require(32 <= len(data) <= MAX_ENTRY and data[:4] == b"KIMG", "invalid or oversized KIMG")
    version, lw, lh, pw, ph, codec, length = struct.unpack_from("<7I", data, 4)
    require(version == 1 and 1 <= pw <= lw <= 32768 and 1 <= ph <= lh <= 32768 and
            pw * ph <= 268435456, "invalid KIMG version or dimensions")
    require(codec in (1, 2, 3, 4, 7, 8) and 0 < length <= MAX_ENTRY - 32 and
            32 + length <= len(data), "unsupported KIMG codec or invalid payload length")
    if codec in (3, 4):
        require(length == ((pw + 3) // 4) * ((ph + 3) // 4) * 16, "KIMG BC7 size mismatch")
    result = {"logical": [lw, lh], "physical": [pw, ph], "codec": codec,
              "payload_bytes": length, "padding_bytes": len(data) - 32 - length}
    if replacement:
        require(codec == 8 and 32 + length == len(data), "replacement must be exact codec8 KIMG")
        require(pw <= 2048 and ph <= 2048, "replacement exceeds native texture dimensions")
        result["gxt"] = parse_gxt(data[32:], (pw, ph))
    return result


def parse_gxt(data: bytes, physical: tuple[int, int]) -> dict:
    require(64 <= len(data) <= MAX_ENTRY - 32, "truncated or oversized GXT")
    tag, version, count, header_off, header_size, p4, p8, pad = struct.unpack_from("<8I", data)
    off, size, palette, flags, kind, fmt, width, height, mips, reserved = struct.unpack_from("<6I2HB3s", data, 32)
    require(tag == 0x00545847 and version == 0x10000003 and count == 1,
            "replacement needs single-texture GXT v3")
    require((width, height) == physical and width > 0 and height > 0 and mips == 1,
            "GXT dimensions/mip count mismatch")
    require(fmt in (0x81000000, 0x85000000, 0x87000000), "unsupported GXT compressed format")
    minimum = 8 if fmt == 0x81000000 else 4
    bw = max(minimum, 1 << (width - 1).bit_length())
    bh = max(minimum, 1 << (height - 1).bit_length())
    expected = bw * bh if fmt == 0x87000000 else bw * bh // 2
    require(kind in (0, 0xa0000000) and ((bw, bh) == physical or kind == 0xa0000000),
            "GXT arbitrary/swizzled type mismatch")
    require(p4 == p8 == pad == flags == 0 and palette == 0xffffffff and reserved == b"\0" * 3,
            "GXT palette, flags or reserved metadata unsupported")
    require(header_off == off and header_size == size == expected and off >= 64 and off % 16 == 0 and
            off + size == len(data) and not any(data[64:off]), "GXT payload range/backing length mismatch")
    return {"format": f"0x{fmt:08x}", "physical": [width, height], "backing": [bw, bh],
            "payload_bytes": size, "payload_sha256": sha(data[off:]),
            "scope": "container/range only; no color, palette-conversion or hardware proof"}


def parse_atlas(data: bytes, *, unique_names: bool = False) -> list[dict]:
    require(12 <= len(data) <= MAX_ENTRY and data[:4] == b"KATL", "invalid or oversized KATL")
    version, count = struct.unpack_from("<II", data, 4)
    require(version == 1 and 0 < count <= 4096, "invalid KATL header")
    items, names, offset = [], set(), 12
    for _ in range(count):
        start = offset
        require(offset + 8 <= len(data), "truncated KATL entry header")
        nl, il = struct.unpack_from("<II", data, offset)
        offset += 8
        require(0 < nl <= 1024 and 32 <= il <= MAX_ENTRY and offset + nl + il <= len(data),
                "invalid KATL name/image bounds")
        name_bytes = data[offset:offset + nl]
        try:
            name = name_bytes.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ValueError("invalid UTF-8 KATL name") from exc
        require("\0" not in name and (not unique_names or name not in names),
                "duplicate or invalid KATL texture name")
        names.add(name)
        offset += nl
        image = data[offset:offset + il]
        metadata = parse_kimg(image)
        offset += il
        items.append({"name": name, "name_bytes": name_bytes, "image": image, "metadata": metadata,
                      "record": data[start:offset]})
    require(offset == len(data), "unexpected trailing KATL bytes")
    return items


def swap_atlas(data: bytes, replacement: bytes) -> tuple[bytes, list[dict]]:
    # Only the selected atlas needs unique selectors. Existing replacement-
    # character names collide in other unmodified source atlases and remain
    # valid ordered records in the original runtime.
    items = parse_atlas(data, unique_names=True)
    matches = [i for i in items if i["name"] == TARGET_TEXTURE]
    require(len(matches) == 1, f"KATL must contain exactly one {TARGET_TEXTURE}")
    target, new = matches[0]["metadata"], parse_kimg(replacement, replacement=True)
    require(new["logical"] == target["logical"] and new["physical"] == target["physical"],
            "replacement KIMG logical/physical dimension mismatch")
    output = bytearray(data[:12])
    for item in items:
        if item["name"] == TARGET_TEXTURE:
            output.extend(struct.pack("<II", len(item["name_bytes"]), len(replacement)))
            output.extend(item["name_bytes"])
            output.extend(replacement)
        else:
            output.extend(item["record"])
    require(len(output) <= MAX_ENTRY, "replacement atlas exceeds 32 MiB")
    return bytes(output), items


def inventory(root: Path) -> list[Path]:
    require(root.is_dir(), f"source asset directory absent: {root}")
    paths, keys = [], set()
    for parent, dirs, files in os.walk(root, followlinks=False):
        for name in dirs:
            path = Path(parent) / name
            stat = path.lstat()
            require(not path.is_symlink() and not (getattr(stat, "st_file_attributes", 0) & 0x400),
                    f"source directory contains a reparse point: {path}")
        for name in files:
            path = Path(parent) / name
            regular(path)
            relative = path.relative_to(root)
            key = relative.as_posix().casefold()
            require(key not in keys and key != MANIFEST.casefold(), "ambiguous or reserved source file path")
            keys.add(key)
            paths.append(relative)
    require(PRIVATE.issubset({p.as_posix() for p in paths}), "source needs allui.hed/mrg/nam")
    return sorted(paths, key=lambda p: p.as_posix())


def wrap_nx(data: bytes) -> bytes:
    require(len(data) <= MAX_ENTRY, "NX decoded data exceeds 32 MiB")
    body = zlib.compress(data, 9)
    require(len(body) + 16 <= MAX_ENTRY, "NX stored data exceeds 32 MiB")
    return b"NXCX" + struct.pack("<III", len(data), len(body), 0) + body


def image_bytes(logical=(16, 8), physical=(8, 4), codec=2, payload=b"fixture") -> bytes:
    return b"KIMG" + struct.pack("<7I", 1, *logical, *physical, codec, len(payload)) + payload


def atlas_bytes(items: list[tuple[str, bytes]]) -> bytes:
    out = bytearray(b"KATL" + struct.pack("<II", 1, len(items)))
    for name, data in items:
        encoded = name.encode("utf-8")
        out.extend(struct.pack("<II", len(encoded), len(data)))
        out.extend(encoded)
        out.extend(data)
    return bytes(out)


def gxt_fixture(w=8, h=4, fmt=0x87000000) -> bytes:
    bw, bh = max(4, 1 << (w - 1).bit_length()), max(4, 1 << (h - 1).bit_length())
    size = bw * bh if fmt == 0x87000000 else bw * bh // 2
    header = struct.pack("<8I", 0x00545847, 0x10000003, 1, 64, size, 0, 0, 0)
    texture = struct.pack("<6I2HB3s", 64, size, 0xffffffff, 0, 0xa0000000, fmt, w, h, 1, b"\0" * 3)
    return header + texture + bytes((i * 17 + 3) % 256 for i in range(size))


def source_fixture(where: Path, *, items=None) -> tuple[Path, Path]:
    src = where / "source-assets"
    src.mkdir(parents=True)
    if items is None:
        items = [(TARGET_TEXTURE, image_bytes()), ("keep_me", image_bytes(payload=b"unchanged"))]
    raw = [wrap_nx(f"entry {i}".encode()) for i in range(TARGET_INDEX)]
    raw.append(wrap_nx(atlas_bytes(items)))
    raw.append(b"raw unwrapped sector with deliberate nonzero padding")
    offset, hed, nam, mrg = 0, bytearray(), bytearray(), bytearray()
    for i, data in enumerate(raw):
        count = (len(data) + SECTOR - 1) // SECTOR
        unpacked = len(atlas_bytes(items)) if i == TARGET_INDEX else (7 if i < TARGET_INDEX else len(data))
        hed.extend(struct.pack("<IHH", offset, count, (unpacked + SECTOR - 1) // SECTOR))
        name = TARGET_ENTRY if i == TARGET_INDEX else f"ENTRY_{i:02}.BIN"
        nam.extend(name.encode().ljust(30, b"\0") + b"\r\n")
        mrg.extend(data + bytes([0xa5 if i != TARGET_INDEX else 0]) * (count * SECTOR - len(data)))
        offset += count
    hed.extend(b"\xff" * 8)
    for filename, data in [("allui.hed", hed), ("allui.nam", nam), ("allui.mrg", mrg),
                           (OMIT, b'{"old_audit":true}'), ("other.dat", b"hard-linked immutable input")]:
        src.joinpath(filename).write_bytes(data)
    src.joinpath("en").mkdir()
    src.joinpath("en/nested.dat").write_bytes(b"nested hard link")
    replacement = where / "replacement.kimg"
    replacement.write_bytes(image_bytes(codec=8, payload=gxt_fixture()))
    return src, replacement


def stage(source: Path, replacement: Path, output: Path) -> dict:
    output = destination(output)
    source, replacement = source.resolve(), replacement.resolve()
    require(source != output and not output.is_relative_to(source) and not source.is_relative_to(output),
            "source/output trees must not overlap")
    require(not replacement.is_relative_to(output), "replacement must be outside fresh output")
    paths = inventory(source)
    snapshots = {p: signature(source / p) for p in paths}
    replacement_signature = signature(replacement)
    replacement_bytes = bounded_read(replacement, MAX_ENTRY)
    replacement_metadata = parse_kimg(replacement_bytes, replacement=True)
    archive = Archive(source)
    _, atlas = unpack_nx(archive.stored(archive.entries[TARGET_INDEX]))
    new_atlas, old_items = swap_atlas(atlas, replacement_bytes)
    converted_nx = wrap_nx(new_atlas)
    require((len(converted_nx) + SECTOR - 1) // SECTOR * SECTOR <= MAX_ENTRY,
            "replacement stored sector extent exceeds 32 MiB")
    # Validate every entry before creating output. Zero-length placeholder rows
    # and exact shared extents are preserved, not silently turned into images.
    preserved_name_collisions = []
    for entry in archive.entries:
        _, decoded = unpack_nx(archive.stored(entry))
        if decoded.startswith(b"KATL"):
            items = parse_atlas(decoded)
            counts = {}
            for item in items:
                counts[item["name"]] = counts.get(item["name"], 0) + 1
            collisions = [{"name": name, "count": count} for name, count in counts.items() if count > 1]
            if collisions:
                preserved_name_collisions.append({"index": entry.index, "name": entry.name,
                                                  "duplicates": collisions,
                                                  "disposition": "unchanged ordered source records; no lookup or rename"})
        elif decoded.startswith(b"KIMG"):
            parse_kimg(decoded)

    print(f"validated {len(archive.entries)} archive entries; staging {len(paths)} source files", flush=True)
    output.mkdir(parents=True, exist_ok=False)
    # Never open a linked path in write mode. Only the private triple below is
    # rebuilt; on any error the partial destination is left for inspection.
    files, omitted = [], []
    for count, relative in enumerate(paths, 1):
        src, dst = source / relative, output / relative
        key = relative.as_posix()
        source_id = file_hash(src)
        if key == OMIT:
            omitted.append({"relative_path": key, **source_id,
                            "reason": "original compact audit does not describe the replaced atlas"})
            continue
        if key in PRIVATE:
            files.append({"relative_path": key, "mode": "private-rebuilt" if key != "allui.nam" else "private-exact-copy",
                          "source": source_id})
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        os.link(src, dst)
        require(os.path.samefile(src, dst) and signature(src) == snapshots[relative] and
                signature(dst) == snapshots[relative], f"hard-link identity changed: {relative}")
        stat = dst.stat()
        files.append({"relative_path": key, "mode": "hard-link", **source_id,
                      "same_file": True, "device": str(stat.st_dev), "file_id": str(stat.st_ino),
                      "link_count_after": stat.st_nlink})
        if count % 1000 == 0:
            print(f"hashed and linked {count}/{len(paths)} files", flush=True)

    offset, hed, shared = 0, bytearray(), {}
    with (output / "allui.mrg").open("xb") as stream:
        for entry in archive.entries:
            changed = entry.index == TARGET_INDEX
            key = (entry.offset, entry.stored_sectors)
            if not changed and entry.stored_sectors and key in shared:
                row_offset, stored = shared[key]
            else:
                data = converted_nx if changed else archive.stored(entry)
                stored = (len(data) + SECTOR - 1) // SECTOR
                row_offset = offset
                require(stored <= 0xffff and offset + stored <= 0xffffffff, "output HED sector overflow")
                stream.write(data)
                stream.write(b"\0" * (stored * SECTOR - len(data)))
                offset += stored
                if not changed and stored:
                    shared[key] = row_offset, stored
            unpacked = (len(new_atlas) + SECTOR - 1) // SECTOR if changed else entry.unpacked_sectors
            require(unpacked <= 0xffff, "output unpacked sector overflow")
            hed.extend(struct.pack("<IHH", row_offset, stored, unpacked))
    with (output / "allui.hed").open("xb") as stream:
        stream.write(hed + archive.tail)
    with (output / "allui.nam").open("xb") as stream:
        stream.write(archive.nam)

    reopened = Archive(output)
    require(len(reopened.entries) == len(archive.entries) and reopened.nam == archive.nam,
            "reopened archive entry count or exact NAM changed")
    records, item_records = [], []
    for before, after in zip(archive.entries, reopened.entries, strict=True):
        require(before.name == after.name, "archive entry order/name changed")
        original_sectors, final_sectors = archive.stored(before), reopened.stored(after)
        original_raw, original = unpack_nx(original_sectors)
        final_raw, final = unpack_nx(final_sectors)
        changed = before.index == TARGET_INDEX
        if changed:
            require(final == new_atlas and final_raw == converted_nx, "changed NX did not round trip exactly")
            new_items = parse_atlas(final, unique_names=True)
            require(len(new_items) == len(old_items), "KATL entry count changed")
            for old, new in zip(old_items, new_items, strict=True):
                require(old["name"] == new["name"] and
                        old["metadata"]["logical"] == new["metadata"]["logical"] and
                        old["metadata"]["physical"] == new["metadata"]["physical"],
                        "KATL names, order or dimensions changed")
                selected = old["name"] == TARGET_TEXTURE
                require(new["image"] == (replacement_bytes if selected else old["image"]),
                        "unexpected KATL member byte change")
                if not selected:
                    require(old["record"] == new["record"], "unchanged KATL member record changed")
                item_records.append({"name": old["name"], "changed": selected,
                                     "source_sha256": sha(old["image"]), "output_sha256": sha(new["image"]),
                                     "source": old["metadata"], "output": new["metadata"],
                                     "exact_unchanged_record": not selected})
        else:
            require(original_sectors == final_sectors and original_raw == final_raw and original == final and
                    before.stored_sectors == after.stored_sectors and
                    before.unpacked_sectors == after.unpacked_sectors,
                    f"unchanged archive entry bytes/size changed: {before.index} {before.name}")
        records.append({"index": before.index, "name": before.name, "changed": changed,
                        "source_offset_sectors": before.offset, "output_offset_sectors": after.offset,
                        "source_stored_sectors": before.stored_sectors, "output_stored_sectors": after.stored_sectors,
                        "source_unpacked_sectors": before.unpacked_sectors, "output_unpacked_sectors": after.unpacked_sectors,
                        "source_sector_sha256": sha(original_sectors), "output_sector_sha256": sha(final_sectors),
                        "source_raw_sha256": sha(original_raw), "output_raw_sha256": sha(final_raw),
                        "source_decoded_sha256": sha(original), "output_decoded_sha256": sha(final),
                        "unchanged_sector_and_decoded_exact": not changed})
    for record in files:
        if record["mode"] != "hard-link":
            src, dst = source / record["relative_path"], output / record["relative_path"]
            require(not os.path.samefile(src, dst), "private archive unexpectedly hard-linked")
            record["output"] = file_hash(dst)
            record["same_file"] = False
            if record["mode"] == "private-exact-copy":
                require(record["source"] == record["output"], "NAM exact-copy hash mismatch")
    # New hard links change link count/ctime, but contents, mtime, file identity
    # and size of every source must remain exactly the observed originals.
    require(all(signature(source / p) == snapshots[p] for p in paths), "source file identity/content metadata changed")
    require(signature(replacement) == replacement_signature, "replacement changed during staging")
    require({p.as_posix() for p in paths} == {p.as_posix() for p in inventory(source)}, "source inventory changed")
    for record in files:
        if record["mode"] == "hard-link":
            src, dst = source / record["relative_path"], output / record["relative_path"]
            require(os.path.samefile(src, dst), "output hard link lost source identity")
    receipt = {"schema": "vita-gxt-menu-stage-v1", "created_utc": datetime.now(timezone.utc).isoformat(),
               "status": "partial experimental Vita-only; no hardware proof", "runtime_verified": False,
               "scope": "exactly one allui KATL member replaced; other files hard-linked read-only by this tool",
               "source_assets": str(source), "output_assets": str(output),
               "source_writes": "none; NTFS hard-link counts/ctime can change; contents are never written",
               "hard_link_constraint": "output shares source storage; any later editor must copy before writing",
               "tool": {"path": str(Path(__file__).resolve()), **file_hash(Path(__file__))},
               "replacement": {"path": str(replacement), "bytes": len(replacement_bytes),
                               "sha256": sha(replacement_bytes), "metadata": replacement_metadata},
               "selection": {"archive": "allui", "index": TARGET_INDEX, "name": TARGET_ENTRY,
                             "texture": TARGET_TEXTURE, "source_atlas_sha256": sha(atlas),
                             "output_atlas_sha256": sha(new_atlas)},
               "omitted": omitted, "files": files, "archive_entries": records, "katl_members": item_records,
               "preserved_source_atlas_name_collisions": preserved_name_collisions,
               "verification": {"all_pass": True, "archive_entries": len(records),
                                "changed_archive_entries": sum(r["changed"] for r in records),
                                "unchanged_entries_stored_and_decoded_exact": len(records) - 1,
                                "katl_members": len(item_records), "other_katl_member_records_exact": len(item_records) - 1,
                                "hard_link_files": sum(r["mode"] == "hard-link" for r in files),
                                "private_archive_files": 3, "stale_allui_compact_audit_copied": False,
                                "source_inventory_and_file_identities_unchanged": True,
                                "reopened_all_entries": True}}
    with (output / MANIFEST).open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(receipt, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
    print(f"verified all entries; manifest {output / MANIFEST}", flush=True)
    return receipt


def self_test() -> int:
    WORK_ROOT.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="self-test-", dir=WORK_ROOT))
    checks = []

    def check(name: str, callback) -> None:
        try:
            callback()
            checks.append({"name": name, "pass": True})
        except Exception as exc:
            checks.append({"name": name, "pass": False, "error": f"{type(exc).__name__}: {exc}"})
            traceback.print_exc()

    def positive() -> None:
        src, replacement = source_fixture(directory / "positive")
        before = {p.relative_to(src): file_hash(p) for p in src.rglob("*") if p.is_file()}
        receipt = stage(src, replacement, directory / "positive/output-assets")
        assert receipt["verification"]["all_pass"]
        assert os.path.samefile(src / "other.dat", directory / "positive/output-assets/other.dat")
        assert os.path.samefile(src / "en/nested.dat", directory / "positive/output-assets/en/nested.dat")
        assert not os.path.samefile(src / "allui.nam", directory / "positive/output-assets/allui.nam")
        assert not directory.joinpath("positive/output-assets", OMIT).exists()
        assert before == {p.relative_to(src): file_hash(p) for p in src.rglob("*") if p.is_file()}
        old, new = Archive(src), Archive(directory / "positive/output-assets")
        for i in range(len(old.entries)):
            if i != TARGET_INDEX:
                assert old.stored(old.entries[i]) == new.stored(new.entries[i])
        _, atlas = unpack_nx(new.stored(new.entries[TARGET_INDEX]))
        items = parse_atlas(atlas)
        assert items[0]["image"] == replacement.read_bytes()
        assert items[1]["image"] == image_bytes(payload=b"unchanged")
        assert receipt["verification"]["changed_archive_entries"] == 1
        assert receipt["verification"]["hard_link_files"] == 2

    def rejected(name: str, callback) -> None:
        def test():
            try:
                callback()
            except ValueError:
                return
            raise AssertionError("invalid input was accepted")
        check(name, test)

    def archive_case(name: str, mutation) -> None:
        def test():
            src, replacement = source_fixture(directory / name)
            mutation(src, replacement)
            output = src.parent / "output-assets"
            try:
                stage(src, replacement, output)
            finally:
                assert not output.exists(), "invalid inputs created a partial stage before validation"
        rejected(f"archive rejects {name}", test)

    def edit_word(path: Path, offset: int, fmt: str, value: int) -> None:
        data = bytearray(path.read_bytes())
        struct.pack_into(fmt, data, offset, value)
        path.write_bytes(data)

    def set_name(src: Path, index: int, name: str) -> None:
        data = bytearray(src.joinpath("allui.nam").read_bytes())
        data[index * 32:(index + 1) * 32] = name.encode().ljust(30, b"\0") + b"\r\n"
        src.joinpath("allui.nam").write_bytes(data)

    def replace_target(src: Path, data: bytes) -> None:
        hed = bytearray(src.joinpath("allui.hed").read_bytes())
        offset, count, _ = struct.unpack_from("<IHH", hed, TARGET_INDEX * 8)
        assert len(data) <= count * SECTOR
        with src.joinpath("allui.mrg").open("r+b") as stream:
            stream.seek(offset * SECTOR)
            stream.write(data + b"\0" * (count * SECTOR - len(data)))

    def valid_aliases() -> None:
        src, replacement = source_fixture(directory / "exact-aliases")
        hed = bytearray(src.joinpath("allui.hed").read_bytes())
        hed[8:16] = hed[:8]
        # A real source has repeated zero-length NOFILE.EXISTS rows; preserve
        # both them and exact language aliases while rejecting partial overlap.
        for index in (2, 3):
            hed[index * 8:(index + 1) * 8] = struct.pack("<IHH", 0, 0, 0)
            set_name(src, index, "NOFILE.EXISTS")
        src.joinpath("allui.hed").write_bytes(hed)
        receipt = stage(src, replacement, src.parent / "output-assets")
        reopened = Archive(src.parent / "output-assets")
        assert reopened.entries[0].offset == reopened.entries[1].offset
        assert reopened.entries[2].stored_sectors == reopened.entries[3].stored_sectors == 0
        assert receipt["verification"]["all_pass"]

    def existing_other_atlas_duplicates() -> None:
        src, replacement = source_fixture(directory / "existing-other-atlas-duplicates")
        atlas = wrap_nx(atlas_bytes([("existing\ufffdduplicate", image_bytes())] * 2))
        with src.joinpath("allui.mrg").open("r+b") as stream:
            stream.seek(SECTOR)
            stream.write(atlas + b"\x57" * (SECTOR - len(atlas)))
        original = src.joinpath("allui.mrg").read_bytes()[SECTOR:2 * SECTOR]
        stage(src, replacement, src.parent / "output-assets")
        reopened = Archive(src.parent / "output-assets")
        assert reopened.stored(reopened.entries[1]) == original

    check("positive: one replaced texture, immutable archives, private triple and hard links", positive)
    check("exact HED aliases and repeated empty placeholders remain supported", valid_aliases)
    check("pre-existing duplicate names in an unchanged other KATL remain byte-exact", existing_other_atlas_duplicates)
    archive_case("offset-outside-mrg", lambda src, _: edit_word(src / "allui.hed", 0, "<I", 0xfffffffe))
    archive_case("stored-bound", lambda src, _: edit_word(src / "allui.hed", 4, "<H", 0xffff))
    archive_case("unpacked-bound", lambda src, _: edit_word(src / "allui.hed", 6, "<H", 0xffff))
    archive_case("partial-overlap", lambda src, _: edit_word(src / "allui.hed", 4, "<H", 2))
    archive_case("missing-terminator", lambda src, _: src.joinpath("allui.hed").write_bytes(src.joinpath("allui.hed").read_bytes()[:-8]))
    archive_case("partial-terminator", lambda src, _: edit_word(src / "allui.hed", 0, "<I", 0xffffffff))
    archive_case("hed-misalignment", lambda src, _: src.joinpath("allui.hed").write_bytes(src.joinpath("allui.hed").read_bytes() + b"x"))
    archive_case("nam-misalignment", lambda src, _: src.joinpath("allui.nam").write_bytes(b"x"))
    archive_case("nam-short", lambda src, _: src.joinpath("allui.nam").write_bytes(b"\0" * 32))
    archive_case("duplicate-archive-name", lambda src, _: set_name(src, 0, TARGET_ENTRY))
    archive_case("wrong-index-name", lambda src, _: set_name(src, TARGET_INDEX, "WRONG.NXZ"))
    archive_case("zero-length-nonplaceholder", lambda src, _: edit_word(src / "allui.hed", 4, "<H", 0))
    archive_case("target-missing", lambda src, _: replace_target(src, wrap_nx(atlas_bytes([("wrong_texture", image_bytes())]))))
    archive_case("target-duplicate", lambda src, _: replace_target(src, wrap_nx(atlas_bytes([(TARGET_TEXTURE, image_bytes())] * 2))))
    archive_case("other-member-duplicate", lambda src, _: replace_target(src, wrap_nx(atlas_bytes([(TARGET_TEXTURE, image_bytes()), ("other", image_bytes()), ("other", image_bytes())]))))
    archive_case("replacement-physical-mismatch", lambda _, repl: edit_word(repl, 16, "<I", 7))
    archive_case("replacement-logical-mismatch", lambda _, repl: edit_word(repl, 8, "<I", 17))
    archive_case("replacement-codec", lambda _, repl: edit_word(repl, 24, "<I", 2))
    archive_case("replacement-truncated", lambda _, repl: repl.write_bytes(repl.read_bytes()[:-1]))
    archive_case("replacement-trailing", lambda _, repl: repl.write_bytes(repl.read_bytes() + b"x"))
    archive_case("bad-nx-magic", lambda src, _: replace_target(src, b"NXQQ" + b"\0" * 12))
    archive_case("bad-nx-bounds", lambda src, _: replace_target(src, b"NXCX" + struct.pack("<III", 5, SECTOR, 0)))
    archive_case("bad-nx-expanded-bound", lambda src, _: replace_target(src, b"NXCX" + struct.pack("<III", MAX_ENTRY + 1, 1, 0) + b"x"))

    for data, label in [(b"NX", "short header"), (b"NX??" + b"\0" * 12, "unsupported magic"),
                        (b"NXCX" + struct.pack("<III", 1, 0, 0), "zero compressed length"),
                        (b"NXCX" + struct.pack("<III", 1, 4, 0) + b"oops", "invalid compressed stream")]:
        rejected(f"NX {label}", lambda data=data: unpack_nx(data))
    good_nx = wrap_nx(b"decoded payload")
    for declared, label in [(14, "smaller declared expansion"), (16, "larger declared expansion")]:
        altered = good_nx[:4] + struct.pack("<I", declared) + good_nx[8:]
        rejected(f"NX {label}", lambda altered=altered: unpack_nx(altered))
    rejected("NX checksum mismatch", lambda: unpack_nx(good_nx[:-1] + bytes([good_nx[-1] ^ 1])))
    compressed = good_nx[16:] + b"trailing"
    rejected("NX unused compressed bytes", lambda: unpack_nx(b"NXCX" + struct.pack("<III", 15, len(compressed), 0) + compressed))
    bomb = zlib.compress(b"a" * 4096)
    rejected("NX bounded output rejects expansion beyond declared byte", lambda: unpack_nx(b"NXCX" + struct.pack("<III", 1, len(bomb), 0) + bomb))

    def gzip_valid() -> None:
        encoder = zlib.compressobj(wbits=31)
        body = encoder.compress(b"gzip") + encoder.flush()
        assert unpack_nx(b"NXGX" + struct.pack("<III", 4, len(body), 0) + body)[1] == b"gzip"
    check("valid gzip and exact zlib declarations", gzip_valid)
    atlas = atlas_bytes([(TARGET_TEXTURE, image_bytes())])
    for cut in [0, 11, 12, 18, len(atlas) - 1]:
        rejected(f"KATL truncated at {cut}", lambda cut=cut: parse_atlas(atlas[:cut]))
    rejected("KATL invalid count", lambda: parse_atlas(b"KATL" + struct.pack("<II", 1, 4097)))
    rejected("KATL huge name length", lambda: parse_atlas(atlas[:12] + struct.pack("<I", 0xffffffff) + atlas[16:]))
    rejected("KATL huge image length", lambda: parse_atlas(atlas[:16] + struct.pack("<I", 0xffffffff) + atlas[20:]))
    rejected("KATL trailing data", lambda: parse_atlas(atlas + b"x"))
    valid = image_bytes(codec=8, payload=gxt_fixture())
    for offset, value, label in [(4, 2, "version"), (16, 0, "zero physical"), (20, 9, "physical above logical"),
                                 (24, 99, "unknown codec"), (28, 0xffffffff, "payload overflow"),
                                 (32, 0, "GXT magic"), (32 + 8, 2, "GXT texture count"),
                                 (32 + 12, 80, "GXT header offset"), (32 + 24, 1, "GXT palette count"),
                                 (32 + 40, 0, "GXT palette index"), (32 + 52, 0xdeadbeef, "GXT format")]:
        altered = bytearray(valid)
        struct.pack_into("<I", altered, offset, value)
        rejected(f"KIMG rejects {label}", lambda altered=altered: parse_kimg(bytes(altered), replacement=True))

    def dimensions_valid() -> None:
        for w, h, fmt in [(1, 1, 0x85000000), (3, 5, 0x87000000), (8, 4, 0x87000000)]:
            meta = parse_kimg(image_bytes(logical=(w, h), physical=(w, h), codec=8,
                                         payload=gxt_fixture(w, h, fmt)), replacement=True)
            assert meta["gxt"]["physical"] == [w, h]
    check("compressed small blocks and non-power-of-two axes", dimensions_valid)
    rejected("existing output directory", lambda: destination(directory))
    rejected("workspace safe root itself", lambda: destination(SAFE_ROOT))
    rejected("output outside .work/vita", lambda: destination(ROOT / ".work/forbidden-gxt-stage-output"))
    rejected("path traversal outside .work/vita", lambda: destination(SAFE_ROOT / "../../forbidden-gxt-stage-output"))

    passed = sum(c["pass"] for c in checks)
    record = {"schema": "vita-gxt-stage-self-test-v1", "created_utc": datetime.now(timezone.utc).isoformat(),
              "directory": str(directory), "tool_sha256": sha(Path(__file__).read_bytes()),
              "passed": passed, "total": len(checks), "all_pass": passed == len(checks), "checks": checks}
    directory.joinpath("self-test.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(record, indent=2))
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
            parser.error("--self-test is independent of asset staging arguments")
        return self_test()
    if not all(supplied):
        parser.error("--source-assets, --replacement and --output-assets are all required")
    try:
        receipt = stage(*supplied)
    except (ValueError, OSError, NotImplementedError) as exc:
        print(f"stage refused: {exc}", file=sys.stderr)
        return 1
    print(json.dumps({"manifest": str(args.output_assets.resolve() / MANIFEST),
                      "verification": receipt["verification"]}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

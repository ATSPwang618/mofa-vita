#!/usr/bin/env python3
"""Add the two boot credit screens to the Vita 960x540 parts archive (KNIF00540).

The engine's launch sequence reads `assets/parts.mrg` by entry index. Entry 0 is
the boot logo and entry 1 the caution notice; the two "thank you" screens added
here become entries 2 and 3 and are shown between the TYPE-MOON movie and the
caution notice.

Each entry is a KIMG container (codec 7, PNG-UNORM: the payload is the PNG file
itself) inside an NXCX (zlib) wrapper, exactly like the existing entries, and
entries 0..1 are copied byte for byte. The archive header `parts.hed` is
rewritten with four entries and the 0xFFFFFFFF terminator.

The script also patches the numbers of `assets/vita-540p-assets.json` that cover
the parts archive: the two per-file records (size + SHA-256), the `parts` record
of `archives` and the `statistics` totals. Everything else in that manifest is
kept byte-identical. The EBOOT-embedded startup profile is generated afterwards
by `tools/generate_vita_asset_profile_540.py`.

Usage:
    python3 tools/credit-intro/apply_credit_intro.py            # apply + verify
    python3 tools/credit-intro/apply_credit_intro.py --check    # verify only

The images are the project's own artwork (`credit_1_supporters.png`,
`credit_2_special_thanks.png` next to this script), not game data. The first
apply copies the untouched `parts.hed` / `parts.mrg` to
`.work/credit-intro/backup-original/` so the change can be reverted.
"""
import argparse
import gzip
import hashlib
import json
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
ASSETS = ROOT / "dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets"
HERE = Path(__file__).resolve().parent
PROFILE = ROOT / "knife/knife-platform-vita/src/asset_profile_540p.rs"
GENERATOR = ROOT / "tools/generate_vita_asset_profile_540.py"

SECTOR = 0x800
MARKER = "vita-540p-assets.json"
ARCHIVE = "parts"
IMAGES = ("credit_1_supporters.png", "credit_2_special_thanks.png")
FIRST_NEW_ENTRY = 2
KIMG_VERSION = 1
KIMG_PNG_UNORM = 7
PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
PNG_DROP_CHUNKS = {b"eXIf", b"tEXt", b"iTXt", b"zTXt", b"tIME"}


def crc32(data):
    return zlib.crc32(data) & 0xFFFFFFFF


def ceil_div(value, unit):
    return (value + unit - 1) // unit


# --------------------------------------------------------------------------- archive


def parse_hed(data):
    """[(offset_sectors, stored_sectors, unpacked_sectors)] up to the 0xFFFFFFFF terminator."""
    entries = []
    for pos in range(0, len(data) - 7, 8):
        offset, stored, unpacked = struct.unpack("<IHH", data[pos:pos + 8])
        if offset == 0xFFFFFFFF:
            break
        entries.append((offset, stored, unpacked))
    return entries


def entry_raw(mrg, offset_sectors, stored_sectors):
    start = offset_sectors * SECTOR
    return mrg[start:start + stored_sectors * SECTOR]


def entry_decoded(mrg, offset_sectors, stored_sectors):
    raw = entry_raw(mrg, offset_sectors, stored_sectors)
    if raw[:4] in (b"NXGX", b"NXCX"):
        unpacked_len, compressed_len = struct.unpack("<II", raw[4:12])
        body = raw[16:16 + compressed_len]
        data = (zlib.decompress if raw[:4] == b"NXCX" else gzip.decompress)(body)
        assert len(data) == unpacked_len, "NX header disagrees with its payload"
        return data
    return raw


def kimg_header(data):
    assert data[:4] == b"KIMG", "not a KIMG container"
    version, logical_w, logical_h, width, height, encoding, payload_len = struct.unpack("<7I", data[4:32])
    return version, logical_w, logical_h, width, height, encoding, payload_len


def png_clean(data):
    """Drop metadata-only chunks (eXIf and friends); the image data is untouched."""
    assert data[:8] == PNG_MAGIC, "not a PNG"
    out = bytearray(PNG_MAGIC)
    pos = 8
    while pos < len(data):
        length = struct.unpack(">I", data[pos:pos + 4])[0]
        kind = data[pos + 4:pos + 8]
        chunk = data[pos:pos + 12 + length]
        assert len(chunk) == 12 + length, "truncated PNG chunk"
        pos += 12 + length
        if kind not in PNG_DROP_CHUNKS:
            out += chunk
        if kind == b"IEND":
            break
    return bytes(out)


def png_header(data):
    width, height = struct.unpack(">II", data[16:24])
    depth, colour, interlace = data[24], data[25], data[28]
    return width, height, depth, colour, interlace


def build_entry(png_path):
    """NXCX-wrapped KIMG codec-7 entry for one PNG plus its .hed sector count."""
    payload = png_clean(png_path.read_bytes())
    width, height, depth, colour, interlace = png_header(payload)
    assert (depth, colour, interlace) == (8, 6, 0), f"{png_path.name}: needs 8-bit RGBA, non-interlaced"
    kimg = b"KIMG" + struct.pack(
        "<7I", KIMG_VERSION, width, height, width, height, KIMG_PNG_UNORM, len(payload)) + payload
    compressed = zlib.compress(kimg, 9)
    nx = b"NXCX" + struct.pack("<II", len(kimg), len(compressed)) + b"\0\0\0\0" + compressed
    stored_sectors = ceil_div(len(nx), SECTOR)
    return nx + b"\0" * (stored_sectors * SECTOR - len(nx)), ceil_div(len(kimg), SECTOR)


def build_archive(mrg, hed):
    """(mrg, hed) with entries 0..1 copied byte for byte and both credit screens appended."""
    entries = parse_hed(hed)
    assert len(entries) == FIRST_NEW_ENTRY, f"parts.hed has {len(entries)} entries, expected {FIRST_NEW_ENTRY}"
    body = bytearray()
    table = []
    for offset, stored, unpacked in entries:
        chunk = entry_raw(mrg, offset, stored)
        assert len(chunk) == stored * SECTOR, "existing entry runs past parts.mrg"
        table.append((len(body) // SECTOR, stored, unpacked))
        body += chunk
    for name in IMAGES:
        chunk, unpacked = build_entry(HERE / name)
        table.append((len(body) // SECTOR, len(chunk) // SECTOR, unpacked))
        body += chunk
    hed_out = b"".join(struct.pack("<IHH", *row) for row in table) + b"\xff" * 8
    return bytes(body), hed_out


# -------------------------------------------------------------------------- manifest


def file_record(path, data):
    return (f'{{\n      "path": "{path}",\n      "bytes": {len(data)},\n'
            f'      "sha256": "{hashlib.sha256(data).hexdigest()}"\n    }}')


def archive_record(name, entries, textures, pixels, nbytes):
    return (f'{{\n      "name": "{name}",\n      "entries": {entries},\n      "textures": {textures},\n'
            f'      "pixels": {pixels},\n      "bytes": {nbytes}\n    }}')


def statistics_record(stats):
    return ('"statistics": {\n'
            f'    "files": {stats["files"]},\n'
            f'    "bytes": {stats["bytes"]},\n'
            f'    "entries": {stats["entries"]},\n'
            f'    "textures": {stats["textures"]},\n'
            f'    "pixels": {stats["pixels"]}\n'
            '  }')


def replace_once(text, old, new, label):
    count = text.count(old)
    assert count == 1, f"{label}: expected exactly one match, found {count}"
    return text.replace(old, new)


def patch_manifest(text, old_hed, old_mrg, new_hed, new_mrg):
    """Point the parts records at the new archive and move the totals by the delta."""
    manifest = json.loads(text)
    assert manifest["schema"] == "knife-540p-assets-v1"
    assert manifest["color_contract"] == "encoded-unorm8-v1"
    assert manifest["codecs"] == [1, 2, 7, 9] or KIMG_PNG_UNORM in manifest["codecs"]
    files = {row["path"]: row for row in manifest["files"]}
    old_parts = next(row for row in manifest["archives"] if row["name"] == ARCHIVE)

    entries = len(parse_hed(new_hed))
    textures = entries
    pixels = sum(
        kimg_header(entry_decoded(new_mrg, offset, stored))[3] *
        kimg_header(entry_decoded(new_mrg, offset, stored))[4]
        for offset, stored, _ in parse_hed(new_hed))

    text = replace_once(text, file_record("parts.hed", old_hed), file_record("parts.hed", new_hed), "parts.hed")
    text = replace_once(text, file_record("parts.mrg", old_mrg), file_record("parts.mrg", new_mrg), "parts.mrg")
    text = replace_once(
        text,
        archive_record(ARCHIVE, old_parts["entries"], old_parts["textures"], old_parts["pixels"], old_parts["bytes"]),
        archive_record(ARCHIVE, entries, textures, pixels, len(new_mrg)),
        "parts archive record")

    stats = dict(manifest["statistics"])
    stats["bytes"] += (len(new_hed) + len(new_mrg)) - (files["parts.hed"]["bytes"] + files["parts.mrg"]["bytes"])
    stats["entries"] += entries - old_parts["entries"]
    stats["textures"] += textures - old_parts["textures"]
    stats["pixels"] += pixels - old_parts["pixels"]
    text = replace_once(text, statistics_record(manifest["statistics"]), statistics_record(stats), "statistics")
    return text


# ---------------------------------------------------------------------- verification


def archive_problems(assets, mrg, hed, base):
    """Byte/format checks; `base` is the untouched (mrg, hed) the entries 0..1 must match."""
    problems = []
    entries = parse_hed(hed)
    base_entries = parse_hed(base[1])
    if len(entries) != FIRST_NEW_ENTRY + len(IMAGES):
        problems.append(f"parts.hed has {len(entries)} entries, expected {FIRST_NEW_ENTRY + len(IMAGES)}")
        return problems
    for index, (offset, stored, unpacked) in enumerate(base_entries):
        if entries[index] != (offset, stored, unpacked):
            problems.append(f"entry {index} header changed")
        elif entry_raw(mrg, offset, stored) != entry_raw(base[0], offset, stored):
            problems.append(f"entry {index} bytes changed")

    end = 0
    for index, (offset, stored, _) in enumerate(entries):
        if offset * SECTOR != end:
            problems.append(f"entry {index} does not start at sector {end // SECTOR}")
        end = (offset + stored) * SECTOR
    if end != len(mrg):
        problems.append(f"parts.mrg is {len(mrg)} B but the entries end at {end}")

    for slot, name in enumerate(IMAGES, start=FIRST_NEW_ENTRY):
        offset, stored, unpacked = entries[slot]
        data = entry_decoded(mrg, offset, stored)
        version, logical_w, logical_h, width, height, encoding, payload_len = kimg_header(data)
        payload = data[32:32 + payload_len]
        source = png_clean((HERE / name).read_bytes())
        source_w, source_h = png_header(source)[:2]
        if (version, encoding) != (KIMG_VERSION, KIMG_PNG_UNORM):
            problems.append(f"entry {slot}: KIMG version {version} / codec {encoding}")
        if (logical_w, logical_h, width, height) != (source_w, source_h, source_w, source_h):
            problems.append(f"entry {slot}: KIMG {logical_w}x{logical_h}/{width}x{height}, PNG {source_w}x{source_h}")
        if payload != source:
            problems.append(f"entry {slot}: payload differs from {name}")
        if len(data) != 32 + payload_len:
            problems.append(f"entry {slot}: KIMG length mismatch")
        if ceil_div(len(data), SECTOR) != unpacked:
            problems.append(f"entry {slot}: unpacked sectors {unpacked} != {ceil_div(len(data), SECTOR)}")
    return problems


def manifest_problems(assets, mrg, hed):
    problems = []
    text = (assets / MARKER).read_text(encoding="utf-8")
    manifest = json.loads(text)
    files = {row["path"]: row for row in manifest["files"]}
    for name in ("parts.hed", "parts.mrg"):
        data = (assets / name).read_bytes()
        row = files.get(name)
        if row is None or row["bytes"] != len(data) or row["sha256"] != hashlib.sha256(data).hexdigest():
            problems.append(f"{MARKER}: stale record for {name}")
    archive = next((row for row in manifest["archives"] if row["name"] == ARCHIVE), None)
    if archive is None:
        return problems + [f"{MARKER}: no {ARCHIVE} archive record"]
    entries = parse_hed(hed)
    pixels = sum(kimg_header(entry_decoded(mrg, o, s))[3] * kimg_header(entry_decoded(mrg, o, s))[4]
                 for o, s, _ in entries)
    expected = {"entries": len(entries), "textures": len(entries), "pixels": pixels, "bytes": len(mrg)}
    for key, value in expected.items():
        if archive[key] != value:
            problems.append(f"{MARKER}: {ARCHIVE} {key} {archive[key]} != {value}")
    stats = manifest["statistics"]
    totals = {"files": len(manifest["files"]),
              "bytes": sum(row["bytes"] for row in manifest["files"]),
              "entries": sum(row["entries"] for row in manifest["archives"]),
              "textures": sum(row["textures"] for row in manifest["archives"]),
              "pixels": sum(row["pixels"] for row in manifest["archives"])}
    for key, value in totals.items():
        if stats[key] != value:
            problems.append(f"{MARKER}: statistics {key} {stats[key]} != {value}")
    return problems


def report_of(assets):
    rows = {}
    for name in ("parts.hed", "parts.mrg", MARKER):
        data = (assets / name).read_bytes()
        rows[name] = {"bytes": len(data), "crc32": crc32(data), "sha256": hashlib.sha256(data).hexdigest()}
    return rows


# --------------------------------------------------------------------------------- main


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--assets", type=Path, default=ASSETS, help="assets folder holding parts.hed/parts.mrg")
    parser.add_argument("--check", action="store_true", help="verify without writing")
    parser.add_argument("--backup", type=Path, default=ROOT / ".work/credit-intro/backup-original",
                        help="where the untouched parts.hed/parts.mrg are copied before the first apply")
    args = parser.parse_args(argv)
    assets = args.assets

    for name in IMAGES:
        assert (HERE / name).is_file(), f"missing artwork {HERE / name}"

    hed = (assets / "parts.hed").read_bytes()
    mrg = (assets / "parts.mrg").read_bytes()
    backup_hed, backup_mrg = args.backup / "parts.hed", args.backup / "parts.mrg"
    applied = len(parse_hed(hed)) > FIRST_NEW_ENTRY
    if applied:
        assert backup_hed.is_file() and backup_mrg.is_file(), (
            f"{assets} already has the credit entries but {args.backup} has no original to compare with")
        base_hed, base_mrg = backup_hed.read_bytes(), backup_mrg.read_bytes()
    else:
        base_hed, base_mrg = hed, mrg

    rebuilt_mrg, rebuilt_hed = build_archive(base_mrg, base_hed)
    manifest_text = (assets / MARKER).read_text(encoding="utf-8")
    if applied:
        # Already applied: rebuilding from the same manifest must be a no-op, which also
        # proves the record formatting and the totals were patched in place.
        expected_mrg, expected_hed = rebuilt_mrg, rebuilt_hed
        expected_manifest = patch_manifest(manifest_text, hed, mrg, hed, mrg)
    else:
        expected_mrg, expected_hed = rebuilt_mrg, rebuilt_hed
        expected_manifest = patch_manifest(manifest_text, base_hed, base_mrg, rebuilt_hed, rebuilt_mrg)

    if args.check:
        problems = []
        if not applied:
            problems.append("parts archive is not patched yet")
        if (assets / "parts.mrg").read_bytes() != expected_mrg or (assets / "parts.hed").read_bytes() != expected_hed:
            problems.append("parts archive differs from what this script produces")
        if manifest_text != expected_manifest:
            problems.append(f"{MARKER} differs from what this script produces")
        problems += archive_problems(assets, mrg, hed, (base_mrg, base_hed))
        problems += manifest_problems(assets, mrg, hed)
        if problems:
            print("PROBLEMS:")
            for problem in problems:
                print(" -", problem)
            return 1
        print("parts archive and manifest are up to date and consistent")
        return 0

    if not applied:
        args.backup.mkdir(parents=True, exist_ok=True)
        (args.backup / "parts.hed").write_bytes(base_hed)
        (args.backup / "parts.mrg").write_bytes(base_mrg)
        (assets / "parts.mrg").write_bytes(expected_mrg)
        (assets / "parts.hed").write_bytes(expected_hed)
        (assets / MARKER).write_text(expected_manifest, encoding="utf-8", newline="")
        print(f"wrote {assets / 'parts.hed'}, {assets / 'parts.mrg'} and {MARKER}")
    else:
        print("parts archive already carries the credit entries; nothing written")

    problems = archive_problems(assets, (assets / "parts.mrg").read_bytes(), (assets / "parts.hed").read_bytes(),
                                (base_mrg, base_hed))
    problems += manifest_problems(assets, (assets / "parts.mrg").read_bytes(), (assets / "parts.hed").read_bytes())
    for name, row in report_of(assets).items():
        print(f"{name}: {row['bytes']} B  crc32=0x{row['crc32']:08x}  sha256={row['sha256']}")
    if problems:
        print("\nPROBLEMS:")
        for problem in problems:
            print(" -", problem)
        return 1
    print("\nparts archive and manifest verified")
    print(f"next: {GENERATOR.relative_to(ROOT).as_posix()}  (regenerates {PROFILE.relative_to(ROOT).as_posix()})")
    return 0


if __name__ == "__main__":
    sys.exit(main())

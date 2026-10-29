"""Audit local KIMG archives against the actual native JPEG hardware admission code.

No image conversion or game execution. --profile-library is an explicit locally
built shared object of jpeg_hw.c; archive reads and NX expansion are bounded.
"""
from __future__ import annotations

import argparse
from collections import Counter
import ctypes
import hashlib
import json
import mmap
from pathlib import Path
import struct
import zlib

ARCHIVES = ("allpachdml", "allpacml", "jallpachd", "jallpac", "allui", "parts")
CAP = 32 * 1024 * 1024


class Profile(ctypes.Structure):
    _fields_ = [("width", ctypes.c_uint32), ("height", ctypes.c_uint32), ("sampling", ctypes.c_uint32)]


def digest(p):
    h = hashlib.sha256()
    with p.open("rb") as stream:
        for part in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(part)
    return h.hexdigest()


def payload(mm, record):
    offset, sectors, _ = record
    start, size = offset * 2048, sectors * 2048
    if start + size > len(mm) or size > CAP:
        raise ValueError("Archive read outside bounds")
    raw = mm[start:start + size]
    if raw[:4] in (b"NXCX", b"NXGX"):
        out_size, packed = struct.unpack_from("<II", raw, 4)
        if out_size > CAP or packed + 16 > len(raw):
            raise ValueError("NX expansion outside bounds")
        d = zlib.decompressobj(31 if raw[:4] == b"NXGX" else 15)
        data = d.decompress(raw[16:16 + packed], CAP + 1)
        if len(data) != out_size or not d.eof or d.unused_data:
            raise ValueError("Invalid NX stream")
        return data
    return raw


def images(data):
    if data[:4] == b"KIMG":
        yield "", data
    elif data[:4] == b"KATL":
        version, count = struct.unpack_from("<II", data, 4)
        if version != 1 or count > 10000:
            raise ValueError("Invalid KATL header")
        pos = 12
        for _ in range(count):
            n, size = struct.unpack_from("<II", data, pos)
            pos += 8
            if n > 4096 or size < 32 or pos + n + size > len(data):
                raise ValueError("Invalid KATL range")
            label = data[pos:pos+n].decode("utf-8")
            pos += n
            yield label, data[pos:pos+size]
            pos += size
        if any(data[pos:]):
            raise ValueError("Unexpected KATL trailer")


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("assets", type=Path)
    p.add_argument("--profile-library", type=Path, required=True)
    p.add_argument("--out", type=Path, required=True)
    a = p.parse_args()
    native = ctypes.CDLL(str(a.profile_library.resolve()))
    admission = native.kv_jpeg_hw_profile
    admission.argtypes = (ctypes.c_void_p, ctypes.c_uint32, ctypes.POINTER(Profile))
    admission.restype = ctypes.c_int
    report = {"schema": "knife-vita-jpeg-admission-audit-v1", "assets": str(a.assets),
        "native_admission_library_sha256": digest(a.profile_library),
        "native_source_sha256": digest(Path(__file__).resolve().parents[1] / "knife/vita/jpeg_hw.c"),
        "asset_manifest_sha256": digest(a.assets / "vita-270p-assets.json"),
        "scope": "Complete archive image inventory and exact native header admission only; no actual hardware decode or performance claim",
        "archives": [], "textures": 0, "jpeg_candidates": 0, "jpeg_fallback": 0,
        "sampling": {}, "formats": {}}
    formats, sampling = Counter(), Counter()
    for name in ARCHIVES:
        counts = Counter()
        hed = (a.assets / f"{name}.hed").read_bytes()
        rows = list(struct.iter_unpack("<IHH", hed))
        terminal = next(i for i, row in enumerate(rows) if row[0] == 0xffffffff)
        with (a.assets / f"{name}.mrg").open("rb") as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as mm:
            for record in rows[:terminal]:
                if record[1] == 0:
                    continue
                for _, data in images(payload(mm, record)):
                    magic, version, lw, lh, w, h, codec, size = struct.unpack_from("<4s7I", data)
                    if magic != b"KIMG" or version != 1 or size + 32 > len(data):
                        raise ValueError("Invalid KIMG")
                    counts["textures"] += 1
                    formats[str(codec)] += 1
                    if codec != 1:
                        continue
                    counts["jpeg"] += 1
                    encoded = data[32:32+size]
                    buf, profile = ctypes.create_string_buffer(encoded), Profile()
                    if admission(buf, len(encoded), ctypes.byref(profile)):
                        if (w, h) != (profile.width, profile.height):
                            raise ValueError("KIMG/JPEG dimension mismatch")
                        counts["jpeg_candidates"] += 1
                        sampling["420" if profile.sampling == 0x20202 else "422"] += 1
                    else:
                        counts["jpeg_fallback"] += 1
        report["archives"].append({"archive": name, "entries": terminal, **dict(counts)})
        for key in ("textures", "jpeg_candidates", "jpeg_fallback"):
            report[key] += counts[key]
    report["formats"], report["sampling"] = dict(formats), dict(sampling)
    a.out.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: report[key] for key in ("textures", "formats", "jpeg_candidates", "jpeg_fallback", "sampling")}, ensure_ascii=False))


if __name__ == "__main__":
    main()

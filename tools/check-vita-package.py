"""Validate an engine-only package and its explicit default memory mode."""
import struct
import hashlib
import sys
import os
import zipfile
from pathlib import PurePosixPath
from xml.etree import ElementTree


def parse_sfo(data):
    if data[:4] != b"\0PSF" or len(data) < 20:
        raise ValueError("not a complete SFO header")
    version, keys, values, count = struct.unpack_from("<4I", data, 4)
    if version != 0x101 or 20 + count * 16 > keys or not keys <= values <= len(data):
        raise ValueError("invalid SFO table bounds")
    result = {}
    for i in range(count):
        key_offset, kind, length, capacity, value_offset = struct.unpack_from("<HHIII", data, 20 + i * 16)
        start = keys + key_offset
        end = data.find(b"\0", start, values)
        if not keys <= start < values or end < 0 or length > capacity or values + value_offset + capacity > len(data):
            raise ValueError("SFO entry outside its table")
        key = data[start:end].decode("ascii")
        if key in result:
            raise ValueError("duplicate SFO key")
        raw = data[values + value_offset:values + value_offset + length]
        if kind == 0x404 and length == 4:
            value = struct.unpack("<I", raw)[0]
        elif kind == 0x204 and raw.endswith(b"\0"):
            value = raw[:-1].decode("utf-8")
        else:
            raise ValueError(f"invalid SFO value: {key}")
        result[key] = value
    return result


def check(path, title_id="KNIF00002", release=False):
    # The 540p variant ships with the enlarged MAIN budget; other builds stay at 0.
    # The environment variable still wins, so an A/B can be checked explicitly.
    default_attribute2 = "12" if title_id == "KNIF00540" else "0"
    expected_attribute2 = int(os.environ.get("KNIFE_VITA_ATTRIBUTE2", default_attribute2))
    with zipfile.ZipFile(path) as package:
        names = package.namelist()
        if len(names) != len(set(names)):
            raise ValueError("duplicate package names")
        for name in names:
            parts = PurePosixPath(name).parts
            if not parts or name.startswith("/") or ".." in parts or "\\" in name:
                raise ValueError(f"unsafe package path: {name}")
            if name not in ("eboot.bin", "LICENSE", "sce_module/libshacccg.suprx", "sce_module/SceShaccCgExt-NOTICE.txt") and not name.startswith(("sce_sys/", "licenses/native-dependencies/")):
                raise ValueError(f"unexpected non-engine payload: {name}")
        if "sce_module/libshacccg.suprx" in names:
            compiler = package.read("sce_module/libshacccg.suprx")
            if len(compiler) != 1776523 or hashlib.sha256(compiler).hexdigest() != "0c58b5d3e1cc47d9837b8c59ab6f3c299bfe7adf55a83f547d553f9fbb776552":
                raise ValueError("shader compiler identity mismatch")
        if release and "sce_module/libshacccg.suprx" not in names:
            raise ValueError("release package is missing its startup shader compiler")
        if package.read("eboot.bin")[:4] != b"SCE\0":
            raise ValueError("eboot is not a SELF")
        sfo = parse_sfo(package.read("sce_sys/param.sfo"))
        for key, value in {"TITLE_ID": title_id, "APP_VER": "00.01",
                           "ATTRIBUTE2": expected_attribute2}.items():
            if sfo.get(key) != value:
                raise ValueError(f"incorrect {key}: {sfo.get(key)!r}")
        profile = "540p"
        # TITLE is user-facing and may omit the resolution. The fixed release
        # TITLE_ID binds the 540p profile independently of that display name.
        if release and (sfo["TITLE_ID"] != "KNIF00540" or "DEV" in sfo.get("TITLE", "")):
            raise ValueError(f"release package must identify the {profile} profile")
        if not release and "DEV" not in sfo.get("TITLE", ""):
            raise ValueError("development probe must be identified in title")
        template = ElementTree.fromstring(package.read("sce_sys/livearea/contents/template.xml"))
        if template.tag != "livearea":
            raise ValueError("LiveArea template must have a livearea root")
        for element in template.iter():
            if element.tag not in ("image", "startup-image"):
                continue
            image = (element.text or "").strip()
            image_path = PurePosixPath(image)
            if not image or image_path.is_absolute() or ".." in image_path.parts or "\\" in image:
                raise ValueError(f"invalid LiveArea image reference: {image!r}")
            if "sce_sys/livearea/contents/" + image not in names:
                raise ValueError(f"missing LiveArea image referenced by template: {image}")
        images = [("icon0.png", (128, 128)), ("livearea/contents/bg.png", (840, 500)),
                  ("livearea/contents/startup.png", (280, 158))]
        if "sce_sys/pic0.png" in names:
            images.append(("pic0.png", (960, 544)))
        for name, dimensions in images:
            data = package.read("sce_sys/" + name)
            if len(data) < 33 or data[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack_from(">II", data, 16) != dimensions or data[25] != 3:
                raise ValueError(f"invalid indexed LiveArea image: {name}")
        print(f"PASS: {sfo['TITLE_ID']} ATTRIBUTE2={sfo['ATTRIBUTE2']}; {len(names)} engine-only entries; no game data")
        return sfo


if __name__ == "__main__":
    check(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else "KNIF00002", "--release" in sys.argv[3:])

#!/usr/bin/env python3
"""Convert one default-Alter compact KIMG (or one named KATL member) to Vita GXT.

Writes only under .work/vita. This is an offline CPU/reference conversion, not
proof of GPU sampling. Existing files are never replaced. It does not repack archives.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import io
import json
import math
from pathlib import Path
import struct
import subprocess
import sys

from PIL import DdsImagePlugin, Image, ImageChops, ImageStat
import PIL

ROOT = Path(__file__).resolve().parents[1]
SAFE_ROOT = (ROOT / ".work" / "vita").resolve()
SDK = Path("D:/unity/PSVITA/sdk/host_tools/bin/psp2gxt.exe")
FORMATS = {"dxt1": 0x85000000, "dxt5": 0x87000000}
MAX_KIMG = 32 * 1024 * 1024
MAX_GXT = 16 * 1024 * 1024


def sha(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def file_id(path: Path) -> dict:
    data = path.read_bytes()
    return {"path": str(path.resolve()), "bytes": len(data), "sha256": sha(data)}


def safe_output(path: Path) -> Path:
    p = path.resolve()
    if not p.is_relative_to(SAFE_ROOT):
        raise ValueError(f"output must be within {SAFE_ROOT}")
    if p.suffix.lower() != ".kimg":
        raise ValueError("output must be a .kimg file")
    return p


def kimg(data: bytes) -> dict:
    if len(data) < 32 or len(data) > MAX_KIMG or data[:4] != b"KIMG":
        raise ValueError("invalid or oversized KIMG")
    version, lw, lh, pw, ph, codec, length = struct.unpack_from("<7I", data, 4)
    if version != 1 or not (1 <= pw <= lw <= 32768 and 1 <= ph <= lh <= 32768):
        raise ValueError("invalid KIMG metadata")
    if pw > 960 or ph > 1080 or pw * ph > 960 * 1080:
        raise ValueError("physical extent exceeds Vita compact atlas bounds")
    if codec not in (1, 2):
        raise ValueError("only default-Alter JPEG/PNG KIMG inputs are supported")
    if not length or length > MAX_KIMG - 32 or 32 + length > len(data):
        raise ValueError("truncated/oversized KIMG payload")
    return {"logical": (lw, lh), "physical": (pw, ph), "codec": codec,
            "payload": data[32:32 + length], "padding_bytes": len(data) - 32 - length}


def member(data: bytes, name: str) -> bytes:
    if len(data) < 12 or data[:4] != b"KATL":
        raise ValueError("--texture requires KATL input")
    version, count = struct.unpack_from("<2I", data, 4)
    if version != 1 or count > 4096:
        raise ValueError("unsupported KATL header")
    p, selected = 12, None
    for _ in range(count):
        if p + 8 > len(data):
            raise ValueError("truncated KATL entry header")
        nl, il = struct.unpack_from("<2I", data, p)
        p += 8
        if not (1 <= nl <= 1024) or p + nl + il > len(data):
            raise ValueError("invalid KATL entry bounds")
        entry = data[p:p + nl].decode("utf-8")
        p += nl
        if entry == name:
            if selected is not None:
                raise ValueError("duplicate KATL texture name")
            selected = data[p:p + il]
        p += il
    if p != len(data) or selected is None:
        raise ValueError("KATL trailing data or selected texture absent")
    return selected


def srgb_decode(s: float) -> float:
    return s / 12.92 if s <= 0.04045 else ((s + 0.055) / 1.055) ** 2.4


def srgb_encode(l: float) -> float:
    return 12.92 * l if l <= 0.0031308 else 1.055 * l ** (1.0 / 2.4) - 0.055


def alteration_lut() -> bytes:
    return bytes(max(0, min(255, round(srgb_encode(srgb_decode(i / 255.0) ** (1 / 2.2)) * 255)))
                 for i in range(256))


def offline_alter(image: Image.Image) -> Image.Image:
    # RGBA conversion expands P/PA palettes and tRNS; alpha is copied unchanged.
    rgba = image.convert("RGBA")
    r, g, b, a = rgba.split()
    lut = alteration_lut()
    return Image.merge("RGBA", (r.point(lut), g.point(lut), b.point(lut), a))


def pow2(x: int) -> int:
    return 1 << (x - 1).bit_length()


def validate_gxt(raw: bytes, physical: tuple[int, int], codec: str) -> dict:
    if len(raw) < 64 or len(raw) > MAX_GXT:
        raise ValueError("GXT truncated or above 16MiB")
    tag, ver, count, hoff, hsize, p4, p8, hpad = struct.unpack_from("<8I", raw)
    off, size, palette, flags, kind, fmt, w, h, mip, rpad = struct.unpack_from("<6I2HB3s", raw, 32)
    bw, bh = max(4, pow2(w)), max(4, pow2(h))
    expected = bw * bh if codec == "dxt5" else bw * bh // 2
    stored = (expected + 15) & ~15
    checks = {
        "v3_single": tag == 0x00545847 and ver == 0x10000003 and count == 1,
        "no_palette_padding": p4 == p8 == hpad == flags == 0 and palette == 0xffffffff and rpad == b"\x00" * 3,
        "dimensions": (w, h) == physical,
        "single_mip": mip == 1,
        "format": fmt == FORMATS[codec],
        "type": kind in (0, 0xa0000000) if w & (w - 1) == 0 and h & (h - 1) == 0 else kind == 0xa0000000,
        "exact_range": hoff == off and hsize == size and off >= 64 and off % 16 == 0 and
                       off + size == len(raw) and all(b == 0 for b in raw[64:off]),
        "payload_length": size == stored,
        "zero_alignment_tail": raw[off + expected:off + size] == b"\x00" * (stored - expected),
    }
    if not all(checks.values()):
        raise ValueError(f"GXT verification failed: {checks}")
    return {"format": codec, "format_id": hex(fmt), "physical": [w, h],
            "backing": [bw, bh], "compressed_bytes": expected, "stored_bytes": size,
            "alignment_tail_bytes": stored - expected, "gxt_bytes": len(raw),
            "payload_sha256": sha(raw[off:off + expected]),
            "stored_region_sha256": sha(raw[off:]), "checks": checks}


def metrics(original: Image.Image, decoded: Image.Image) -> dict:
    diff = ImageChops.difference(original, decoded)
    stats = ImageStat.Stat(diff)
    def composite(bg):
        layer = Image.new("RGBA", original.size, (*bg, 255))
        delta = ImageChops.difference(Image.alpha_composite(layer, original), Image.alpha_composite(layer, decoded))
        return ImageStat.Stat(delta).mean[:3]
    return {"rgba_mae": stats.mean, "rgba_rmse": stats.rms,
            "rgba_max": [extent[1] for extent in diff.getextrema()],
            "black_rgb_mae": composite((0, 0, 0)), "white_rgb_mae": composite((255, 255, 255))}


def convert(source: Path, output: Path, codec_request: str, texture: str | None = None) -> dict:
    output = safe_output(output)
    input_raw = source.read_bytes()
    if len(input_raw) > MAX_KIMG:
        raise ValueError("input exceeds 32MiB bound")
    selected = member(input_raw, texture) if texture else input_raw
    meta = kimg(selected)
    expected_format = "JPEG" if meta["codec"] == 1 else "PNG"
    with Image.open(io.BytesIO(meta["payload"])) as loaded:
        if loaded.format != expected_format or loaded.size != meta["physical"]:
            raise ValueError("KIMG codec or physical dimension mismatch")
        loaded.load()
        source_mode = loaded.mode
        rgba = loaded.convert("RGBA")
    altered = offline_alter(rgba)
    alpha_min, alpha_max = rgba.getchannel("A").getextrema()
    fully_opaque = (alpha_min, alpha_max) == (255, 255)
    codec = ("dxt1" if fully_opaque else "dxt5") if codec_request == "auto" else codec_request
    if codec == "dxt1" and not fully_opaque:
        raise ValueError("DXT1 requires every actual pixel to be fully opaque")
    if rgba.getchannel("A").tobytes() != altered.getchannel("A").tobytes():
        raise AssertionError("offline Alter changed alpha")
    names = {"dds": output.with_suffix(".dds"), "gxt": output.with_suffix(".gxt"),
             "json": output.with_suffix(".json"), "rgba": output.with_suffix(".alter.rgba")}
    if output.resolve() == source.resolve() or any(p.exists() for p in [output, *names.values()]):
        raise ValueError("refusing to overwrite input or existing output/sidecar")
    if not SDK.is_file():
        raise FileNotFoundError(f"installed psp2gxt not found: {SDK}")
    output.parent.mkdir(parents=True, exist_ok=True)
    # DDS contains CPU-compressed source in linear memory; SDK converts it to GXM layout.
    altered.save(names["dds"], format="DDS", pixel_format=codec.upper())
    dds = names["dds"].read_bytes()
    if dds[:4] != b"DDS " or dds[84:88] != codec.upper().encode("ascii"):
        raise ValueError("Pillow DDS output format mismatch")
    completed = subprocess.run([str(SDK), "-i", names["dds"].name, "-o", names["gxt"].name],
                               cwd=output.parent, capture_output=True, timeout=120, check=False)
    if completed.returncode != 0 or not names["gxt"].is_file():
        raise ValueError(f"psp2gxt failed rc={completed.returncode}: {completed.stderr.decode(errors='replace')[:800]}")
    gxt_raw = names["gxt"].read_bytes()
    check = validate_gxt(gxt_raw, meta["physical"], codec)
    if len(gxt_raw) > 0xffffffff:
        raise ValueError("GXT payload too large for KIMG")
    with Image.open(names["dds"]) as intermediate:
        intermediate.load()
        decoded = intermediate.convert("RGBA").crop((0, 0, *meta["physical"]))
    if decoded.size != altered.size:
        raise AssertionError("CPU reference dimension changed")
    out_raw = b"KIMG" + struct.pack("<7I", 1, *meta["logical"], *meta["physical"], 8, len(gxt_raw)) + gxt_raw
    if len(out_raw) > MAX_KIMG:
        raise ValueError("output KIMG exceeds bound")
    output.write_bytes(out_raw)
    names["rgba"].write_bytes(altered.tobytes())
    # All hashes are over exact input, intermediate and producer bytes. CPU DDS
    # decode verifies BC pixels, not GXM's swizzled layout or hardware output.
    receipt = {"schema": "vita-game-gxt-conversion-v1", "created_utc": datetime.now(timezone.utc).isoformat(),
               "scope": "one default-Alter KIMG; no archive repack; no GPU/hardware verification",
               "source": file_id(source), "selected_kimg_sha256": sha(selected),
               "selected_texture": texture, "source_payload_sha256": sha(meta["payload"]),
               "source_mode": source_mode, "logical": meta["logical"], "physical": meta["physical"],
               "input_codec": expected_format, "input_padding_bytes": meta["padding_bytes"],
               "alpha_extrema": [alpha_min, alpha_max], "fully_opaque": fully_opaque,
               "codec": codec, "alpha_mode": "straight; original alpha retained before lossy DXT encoding",
               "alter": "srgb_encode(pow(srgb_decode(src_RGB),1/2.2)); alpha=src_alpha",
               "color_assumption": "RGB byte values interpreted as IEC sRGB; ICC chunks not color-managed",
               "tool": {"converter": file_id(Path(__file__)), "psp2gxt": file_id(SDK),
                        "pillow_version": PIL.__version__, "pillow_dds_plugin": file_id(Path(DdsImagePlugin.__file__)),
                        "pillow_native_core": file_id(Path(Image.core.__file__))},
               "gxt": check, "cpu_decode": {"scope": "Pillow DDS intermediate, not swizzled GXT or GPU",
                   "sha256": sha(decoded.tobytes()), "metrics": metrics(altered, decoded)},
               "bytes": {"input_kimg": len(selected), "gxt_payload": len(gxt_raw),
                         "gpu_compressed_payload": check["compressed_bytes"],
                         "linear_rgba_cdram_baseline": ((meta["physical"][0] + 7) & ~7) * meta["physical"][1] * 4,
                         "compressed_cdram_minimum": (check["compressed_bytes"] + 0xfff) & ~0xfff},
               "artifacts": {name: file_id(path) for name, path in {"output": output, **names}.items() if name != "json"}}
    names["json"].write_text(json.dumps(receipt, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return receipt


def self_test() -> dict:
    import tempfile
    test_root = SAFE_ROOT / "game-gxt-converter-validation"
    test_root.mkdir(parents=True, exist_ok=True)
    case_root = Path(tempfile.mkdtemp(prefix="synthetic-", dir=test_root))
    checks = []
    def check(label, cond):
        checks.append({"name": label, "pass": bool(cond)})
    # Distinct palette colors, tRNS and PA (palette RGB with separate alpha).
    p = Image.new("P", (8, 8))
    palette = [0] * 768
    palette[3:9] = [13, 47, 221, 240, 19, 32]
    p.putpalette(palette)
    p.putdata([1, 2] * 32)
    p.info["transparency"] = bytes([255, 0, 173])
    bio = io.BytesIO()
    p.save(bio, format="PNG")
    pal = bio.getvalue()
    with Image.open(io.BytesIO(pal)) as read:
        read.load()
        expanded = read.convert("RGBA")
        check("PLTE RGB preserved", expanded.getpixel((0, 0)) == (13, 47, 221, 0))
        check("tRNS partial alpha preserved", expanded.getpixel((1, 0)) == (240, 19, 32, 173))
    pa = Image.new("PA", (2, 1))
    pa.putpalette(palette)
    pa.putdata([(1, 255), (2, 50)])
    check("PA RGB and alpha", pa.convert("RGBA").getpixel((1, 0)) == (240, 19, 32, 50))
    pa_baked = offline_alter(pa)
    check("PA baked RGB keeps palette entry", pa_baked.getpixel((1, 0))[:3] == tuple(alteration_lut()[v] for v in (240, 19, 32)))
    check("PA baked alpha remains 50", pa_baked.getpixel((1, 0))[3] == 50)
    check("identity endpoint 0", alteration_lut()[0] == 0)
    check("identity endpoint 255", alteration_lut()[255] == 255)
    check("alter known IEC sample 128", alteration_lut()[128] == 187)
    samples = [("palette", pal, (16, 16), "auto", "dxt5"),
               ("opaque", Image.new("RGB", (8, 8), (82, 149, 227)), (8, 8), "auto", "dxt1"),
               ("tiny4", Image.new("RGB", (4, 4), (82, 149, 227)), (4, 4), "auto", "dxt1"),
               ("tiny1", Image.new("RGB", (1, 1), (82, 149, 227)), (1, 1), "auto", "dxt1"),
               ("tiny3x2", Image.new("RGB", (3, 2), (82, 149, 227)), (3, 2), "auto", "dxt1")]
    for label, src, logical, request, wanted in samples:
        if isinstance(src, Image.Image):
            stream = io.BytesIO()
            src.save(stream, format="PNG")
            src = stream.getvalue()
        with Image.open(io.BytesIO(src)) as image:
            physical = image.size
        raw = b"KIMG" + struct.pack("<7I", 1, *logical, *physical, 2, len(src)) + src + b"\x00" * 8
        input_path = case_root / f"{label}-input.kimg"
        input_path.write_bytes(raw)
        out = case_root / f"{label}-output.kimg"
        receipt = convert(input_path, out, request)
        check(f"{label} selected {wanted}", receipt["codec"] == wanted)
        check(f"{label} exact output KIMG", len(out.read_bytes()) == 32 + len((case_root / f"{label}-output.gxt").read_bytes()))
        check(f"{label} GXT validated", all(receipt["gxt"]["checks"].values()))
        if label.startswith("tiny"):
            check(f"{label} SDK padded 8 valid bytes to 16", receipt["gxt"]["compressed_bytes"] == 8
                  and receipt["gxt"]["stored_bytes"] == 16 and receipt["gxt"]["alignment_tail_bytes"] == 8)
        check(f"{label} alpha extraction", receipt["alpha_extrema"] == ([0, 173] if label == "palette" else [255, 255]))
        try:
            convert(input_path, case_root / f"{label}-reject.kimg" if label == "palette" else out, "dxt1" if label == "palette" else request)
        except ValueError:
            check(f"{label} rejects explicit transparent dxt1 or existing output", True)
        else:
            check(f"{label} rejects explicit transparent dxt1 or existing output", False)
    malformed = b"KIMG" + struct.pack("<7I", 1, 8, 8, 8, 8, 2, 100) + b"x"
    try:
        kimg(malformed)
    except ValueError:
        check("truncated KIMG payload rejected", True)
    else:
        check("truncated KIMG payload rejected", False)
    gxt = (case_root / "opaque-output.gxt").read_bytes()
    tiny = bytearray((case_root / "tiny4-output.gxt").read_bytes())
    tiny[72] = 1  # First byte of the 8-byte SDK alignment tail.
    try:
        validate_gxt(bytes(tiny), (4, 4), "dxt1")
    except ValueError:
        check("nonzero alignment tail rejected", True)
    else:
        check("nonzero alignment tail rejected", False)
    # Format is record offset 32+20=52.
    for label, malformed in [("GXT trailing bytes", gxt + b"extra"),
                             ("GXT unknown format", gxt[:52] + struct.pack("<I", 0x12345678) + gxt[56:])]:
        try:
            validate_gxt(malformed, (8, 8), "dxt1")
        except ValueError:
            check(label + " rejected", True)
        else:
            check(label + " rejected", False)
    try:
        convert(input_path, case_root.parent.parent.parent / "unsafe.kimg", "auto")
    except ValueError:
        check("path escape rejected", True)
    else:
        check("path escape rejected", False)
    report = {"schema": "vita-game-gxt-converter-self-test-v1", "case_dir": str(case_root),
              "checks": checks, "passed": sum(c["pass"] for c in checks), "failed": sum(not c["pass"] for c in checks)}
    (case_root / "self-test.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, help="KIMG or KATL path")
    parser.add_argument("--output", type=Path, help="new .work/vita/*.kimg path")
    parser.add_argument("--texture", help="select one named image from KATL, emit only one KIMG")
    parser.add_argument("--codec", choices=("auto", "dxt1", "dxt5"), default="auto")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        result = self_test()
        print(json.dumps(result, ensure_ascii=False, indent=2))
        return 0 if result["failed"] == 0 else 1
    if not args.input or not args.output:
        parser.error("--input and --output are required unless --self-test")
    try:
        receipt = convert(args.input, args.output, args.codec, args.texture)
        print(json.dumps({"output": str(args.output.resolve()), "receipt": str(args.output.with_suffix('.json').resolve()),
                          "output_sha256": receipt["artifacts"]["output"]["sha256"],
                          "gxt_verified": all(receipt["gxt"]["checks"].values())}, indent=2))
    except (ValueError, OSError, subprocess.TimeoutExpired, PIL.UnidentifiedImageError) as exc:
        parser.exit(2, f"conversion failed: {exc}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())









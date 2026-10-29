#!/usr/bin/env python3
"""Build five synthetic, single-level GXT texture fixtures with local tools.

No game image is read. Sony tools are invoked in place and are never copied.
Pillow encodes legacy DXT1/DXT5 DDS; psp2pvrt64 encodes PVRTC-I4 and
psp2gxt performs the GXM layout conversion. Alpha is always straight.
Outputs are intentionally confined to .work/vita/compressed-fixtures.
This records offline conversion/CPU reference evidence, not a GPU test.
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import io
import json
import math
from pathlib import Path
import platform
import struct
import subprocess
import sys
import time
import warnings

from PIL import DdsImagePlugin, Image, ImageChops, ImageDraw, ImageStat
import PIL


ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / ".work" / "vita" / "compressed-fixtures"
SDK_BIN = Path("D:/unity/PSVITA/sdk/host_tools/bin")
FORMATS = {
    "pvrtc4": (0x81000000, "SCE_GXM_TEXTURE_FORMAT_PVRT4BPP_ABGR", 4),
    "dxt1": (0x85000000, "SCE_GXM_TEXTURE_FORMAT_UBC1_ABGR", 4),
    "dxt5": (0x87000000, "SCE_GXM_TEXTURE_FORMAT_UBC3_ABGR", 8),
}


def digest(path: Path) -> dict:
    start = time.perf_counter()
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return {"path": str(path.resolve()), "bytes": path.stat().st_size,
            "sha256": h.hexdigest(), "hash_read_wall_seconds": time.perf_counter() - start}


def save_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False, allow_nan=False) + "\n",
                    encoding="utf-8")


def enclosing_pow2(value: int) -> int:
    if value <= 0:
        raise ValueError("positive extent required")
    return 1 << (value - 1).bit_length()


def classify_alpha(image: Image.Image) -> dict:
    """Classify actual pixels after palette/tRNS/PA expansion, not index bytes.

    A nonopaque palette entry that is unused by the image does not make its
    pixels translucent. DXT1 fixtures are restricted to fully opaque pixels;
    any alpha below 255 requires DXT5. This does not quantize the alpha.
    """
    rgba = image.convert("RGBA")
    low, high = rgba.getchannel("A").getextrema()
    return {"source_mode": image.mode, "expanded_mode": "RGBA", "alpha_min": low,
            "alpha_max": high, "fully_opaque": low == high == 255,
            "dxt_codec": "dxt1" if low == high == 255 else "dxt5"}


def alpha_self_test() -> dict:
    """Exercise real Pillow indexed PNG/tRNS decoding and preserve palette RGB."""
    checks = []

    def check(name: str, actual, expected) -> None:
        checks.append({"name": name, "actual": actual, "expected": expected,
                       "passed": actual == expected})

    indexed = Image.new("P", (3, 1))
    palette = [0] * 768
    palette[3:9] = [13, 47, 221, 240, 19, 32]
    indexed.putpalette(palette)
    indexed.putdata([1, 2, 1])
    indexed.info["transparency"] = bytes([255, 0, 173])
    encoded = io.BytesIO()
    indexed.save(encoded, format="PNG")
    encoded.seek(0)
    with Image.open(encoded) as decoded:
        decoded.load()
        rgba = decoded.convert("RGBA")
        check("indexed PNG tRNS classifies as DXT5", classify_alpha(decoded)["dxt_codec"], "dxt5")
        check("transparent palette RGB retained", rgba.getpixel((0, 0)), (13, 47, 221, 0))
        check("partial palette alpha retained", rgba.getpixel((1, 0)), (240, 19, 32, 173))
        check("indices are not interpreted as grayscale", rgba.getpixel((0, 0))[:3] != (1, 1, 1), True)
        with warnings.catch_warnings(record=True):
            warnings.simplefilter("always")
            check("RGB conversion would incorrectly discard tRNS",
                  classify_alpha(decoded.convert("RGB"))["dxt_codec"], "dxt1")
    indexed.info["transparency"] = 1
    check("single transparent index classifies as DXT5", classify_alpha(indexed)["dxt_codec"], "dxt5")
    indexed.putdata([2, 2, 2])
    check("unused transparent index stays opaque", classify_alpha(indexed)["dxt_codec"], "dxt1")
    pa = Image.new("PA", (2, 1))
    pa.putpalette(palette)
    pa.putdata([(1, 255), (2, 50)])
    check("PA separate alpha classifies as DXT5", classify_alpha(pa)["dxt_codec"], "dxt5")
    check("PA palette RGB retained", pa.convert("RGBA").getpixel((1, 0)), (240, 19, 32, 50))
    check("opaque RGB classifies as DXT1", classify_alpha(Image.new("RGB", (1, 1), (1, 2, 3)))["dxt_codec"], "dxt1")
    check("opaque RGBA classifies as DXT1", classify_alpha(Image.new("RGBA", (1, 1), (1, 2, 3, 255)))["dxt_codec"], "dxt1")
    check("uniform alpha below 255 requires DXT5", classify_alpha(Image.new("RGBA", (1, 1), (1, 2, 3, 254)))["dxt_codec"], "dxt5")
    return {"tests": len(checks), "passed": sum(item["passed"] for item in checks),
            "failed": sum(not item["passed"] for item in checks), "checks": checks}


def make_background() -> Image.Image:
    width, height = 960, 540
    pixels = bytearray(width * height * 3)
    for y in range(height):
        for x in range(width):
            i = (y * width + x) * 3
            pixels[i:i + 3] = bytes((x * 255 // (width - 1),
                                     y * 255 // (height - 1),
                                     (x + y) * 255 // (width + height - 2)))
    image = Image.frombytes("RGB", (width, height), bytes(pixels))
    draw = ImageDraw.Draw(image)
    colors = [(240, 24, 40), (20, 220, 48), (28, 52, 240),
              (240, 220, 24), (24, 220, 220), (220, 32, 220)]
    for i, color in enumerate(colors):
        draw.rectangle((32 + i * 150, 40, 159 + i * 150, 149), fill=color)
    draw.rectangle((30, 356, 929, 509), outline=(255, 255, 255), width=2)
    for x in range(40, 921, 8):
        draw.line((x, 365, x, 500), fill=(255, 255, 255) if x % 16 else (0, 0, 0))
    draw.line((20, 530, 940, 170), fill=(255, 255, 255), width=1)
    draw.line((20, 170, 940, 530), fill=(0, 0, 0), width=3)
    # Unequal, solid corner markers expose flips and red/blue swaps.
    for rect, color in [((0, 0, 15, 15), (255, 0, 0)),
                        ((936, 0, 959, 15), (0, 255, 0)),
                        ((0, 516, 15, 539), (0, 0, 255)),
                        ((936, 516, 959, 539), (255, 255, 255))]:
        draw.rectangle(rect, fill=color)
    return image


def make_portrait() -> Image.Image:
    width, height = 320, 480
    pixels = bytearray(width * height * 4)
    for y in range(height):
        for x in range(width):
            radius = math.hypot(x - 159.5, y - 239.5)
            alpha = max(0, min(255, round((150.0 - radius) * 255.0 / 36.0)))
            i = (y * width + x) * 4
            # RGB remains colored even at alpha=0, exposing premultiplication.
            pixels[i:i + 4] = bytes((40 + x * 200 // (width - 1),
                                     28 + y * 200 // (height - 1),
                                     230 - x * 150 // (width - 1), alpha))
    image = Image.frombytes("RGBA", (width, height), bytes(pixels))
    draw = ImageDraw.Draw(image)
    draw.ellipse((31, 111, 288, 368), outline=(255, 250, 240, 240), width=1)
    draw.ellipse((43, 123, 276, 356), outline=(20, 24, 44, 220), width=2)
    draw.line((64, 180, 255, 300), fill=(255, 255, 255, 140), width=1)
    draw.line((64, 300, 255, 180), fill=(16, 20, 32, 180), width=1)
    for i in range(6):
        draw.rectangle((88 + i * 25, 210, 104 + i * 25, 266),
                       fill=(235 - 25 * i, 40 + 30 * i, 210, 40 + 40 * i))
    return image


def edge_pad(image: Image.Image) -> Image.Image:
    width, height = image.size
    padded_width, padded_height = enclosing_pow2(width), enclosing_pow2(height)
    padded = Image.new("RGBA", (padded_width, padded_height))
    padded.paste(image, (0, 0))
    if padded_width > width:
        edge = image.crop((width - 1, 0, width, height))
        padded.paste(edge.resize((padded_width - width, height), Image.Resampling.NEAREST), (width, 0))
    if padded_height > height:
        edge = padded.crop((0, height - 1, padded_width, height))
        padded.paste(edge.resize((padded_width, padded_height - height), Image.Resampling.NEAREST), (0, height))
    return padded


def run_tool(label: str, executable: Path, arguments: list[str]) -> dict:
    print(f"RUN {label}", flush=True)
    command = [str(executable), *arguments]
    started = time.perf_counter()
    try:
        completed = subprocess.run(command, cwd=OUTPUT, capture_output=True, timeout=300,
                                   check=False)
        stdout, stderr = completed.stdout, completed.stderr
        exit_code, error = completed.returncode, None
    except subprocess.TimeoutExpired as exc:
        stdout, stderr = exc.stdout or b"", exc.stderr or b""
        exit_code, error = None, "tool exceeded the 300 second conversion bound"
    except OSError as exc:
        stdout, stderr, exit_code, error = b"", b"", None, str(exc)
    elapsed = time.perf_counter() - started
    stdout_name, stderr_name = f"{label}.stdout.log", f"{label}.stderr.log"
    (OUTPUT / stdout_name).write_bytes(stdout)
    (OUTPUT / stderr_name).write_bytes(stderr)
    result = {"actualexecuted": exit_code is not None or error is not None,
              "command": command, "cwd": str(OUTPUT), "exit_code": exit_code,
              "wall_seconds": elapsed, "stdout": stdout_name, "stderr": stderr_name,
              "error": error, "succeeded": exit_code == 0}
    print(f"DONE {label}: exit={exit_code}, {elapsed:.3f}s", flush=True)
    return result


def inspect_gxt(path: Path, logical: tuple[int, int], codec: str) -> dict:
    started = time.perf_counter()
    data = path.read_bytes()
    read_seconds = time.perf_counter() - started
    if len(data) < 64:
        raise ValueError("GXT header/texture record is truncated")
    tag, version, count, data_offset, data_size, p4, p8, pad = struct.unpack_from("<8I", data)
    offset, size, palette, flags, kind, fmt, width, height, mips, reserved = struct.unpack_from(
        "<6I2HB3s", data, 32)
    backing = (enclosing_pow2(width), enclosing_pow2(height))
    expected_format, format_name, bpp = FORMATS[codec]
    expected_length = backing[0] * backing[1] * bpp // 8
    expected_kind = 0 if (width, height) == backing else 0xA0000000
    checks = {
        "magic": tag == 0x00545847, "version3": version == 0x10000003,
        "single_texture": count == 1, "no_palettes": p4 == p8 == 0,
        "header_pad_zero": pad == 0, "no_palette": palette == 0xFFFFFFFF,
        "no_flags": flags == 0, "record_pad_zero": reserved == b"\x00\x00\x00",
        "logical_dimensions": (width, height) == logical, "single_level": mips == 1,
        "expected_format": fmt == expected_format, "expected_type": kind == expected_kind,
        "aligned_payload": offset % 16 == 0,
        "header_data_in_file": 64 <= data_offset <= len(data) and data_size <= len(data) - data_offset,
        "texture_in_data_region": data_offset <= offset and offset + size <= data_offset + data_size,
        "expected_padded_payload": size == expected_length,
    }
    return {"header": {"tag": f"0x{tag:08x}", "version": f"0x{version:08x}",
                       "num_textures": count, "data_offset": data_offset, "data_length": data_size,
                       "num_p4_palettes": p4, "num_p8_palettes": p8, "pad": pad},
            "record": {"data_offset": offset, "data_length": size, "palette_index": palette,
                       "flags": flags, "type": f"0x{kind:08x}", "format": f"0x{fmt:08x}",
                       "expected_format_name": format_name, "width": width, "height": height,
                       "mip_count": mips, "reserved_hex": reserved.hex()},
            "logical_dimensions": list(logical), "backing_dimensions": list(backing),
            "data_offset_basis": "start of entire GXT file, for both header and record",
            "payload_sha256": hashlib.sha256(data[offset:offset + size]).hexdigest(),
            "expected_payload_bytes": expected_length, "file_read_wall_seconds": read_seconds,
            "checks": checks, "verified": all(checks.values())}


def image_error(source: Image.Image, decoded: Image.Image) -> dict:
    difference = ImageChops.difference(source, decoded)
    stats = ImageStat.Stat(difference)
    return {"channels": "RGBA", "mae": stats.mean, "rmse": stats.rms,
            "max_absolute_error": [maximum for _, maximum in difference.getextrema()]}


def cpu_reference(name: str, codec: str, source: Image.Image, dds_path: Path) -> dict:
    started = time.perf_counter()
    encoded = dds_path.read_bytes()
    read_seconds = time.perf_counter() - started
    started = time.perf_counter()
    with Image.open(dds_path) as image:
        image.load()
        dimensions = image.size
        decoded = image.convert("RGBA").crop((0, 0, source.width, source.height))
    decode_seconds = time.perf_counter() - started
    if dimensions[0] < source.width or dimensions[1] < source.height:
        raise ValueError("CPU reference DDS is smaller than logical dimensions")
    rgba_name, png_name = f"{name}-{codec}-decoded.rgba", f"{name}-{codec}-decoded.png"
    (OUTPUT / rgba_name).write_bytes(decoded.tobytes())
    decoded.save(OUTPUT / png_name, format="PNG", optimize=False)
    started = time.perf_counter()
    black = Image.new("RGBA", source.size, (0, 0, 0, 255))
    white = Image.new("RGBA", source.size, (255, 255, 255, 255))
    metrics = image_error(source, decoded)
    metrics["composited_over_black"] = image_error(Image.alpha_composite(black, source),
                                                  Image.alpha_composite(black, decoded))
    metrics["composited_over_white"] = image_error(Image.alpha_composite(white, source),
                                                  Image.alpha_composite(white, decoded))
    return {"decoded_dds": dds_path.name, "decoded_dds_dimensions": list(dimensions),
            "decoded_logical_rgba": rgba_name, "decoded_logical_png": png_name,
            "encoded_dds_bytes_read": len(encoded), "file_read_wall_seconds": read_seconds,
            "decode_wall_seconds": decode_seconds,
            "metrics_wall_seconds": time.perf_counter() - started,
            "alpha_extrema": list(decoded.getchannel("A").getextrema()), "metrics": metrics,
            "scope": "CPU decoded compression intermediate, not a GXM GPU observation"}


def generate_source(name: str, constructor) -> tuple[Image.Image, dict]:
    started = time.perf_counter()
    image = constructor()
    generation_seconds = time.perf_counter() - started
    image.save(OUTPUT / f"{name}.png", format="PNG", optimize=False)
    started = time.perf_counter()
    with Image.open(OUTPUT / f"{name}.png") as loaded:
        loaded.load()
        rgba = loaded.convert("RGBA")
    read_convert_seconds = time.perf_counter() - started
    raw = rgba.tobytes()
    (OUTPUT / f"{name}.rgba").write_bytes(raw)
    padded = edge_pad(rgba)
    padded.save(OUTPUT / f"{name}-pvrtc4-input.tga", format="TGA", compression=None)
    checks = {"raw_length": len(raw) == rgba.width * rgba.height * 4,
              "png_raw_exact": image.convert("RGBA").tobytes() == raw,
              "background_opaque": name != "background" or rgba.getchannel("A").getextrema() == (255, 255),
              "portrait_full_alpha_range": name != "portrait" or rgba.getchannel("A").getextrema() == (0, 255),
              "portrait_straight_alpha": name != "portrait" or rgba.getpixel((0, 0)) == (40, 28, 230, 0),
              "padding_no_logical_rescale": padded.crop((0, 0, *rgba.size)).tobytes() == raw}
    if not all(checks.values()):
        raise ValueError(f"synthetic source checks failed: {checks}")
    return rgba, {"name": name, "synthetic": True, "no_game_data": True,
                  "png": f"{name}.png", "png_mode": image.mode, "raw_rgba": f"{name}.rgba",
                  "logical_dimensions": list(rgba.size), "raw_stride": rgba.width * 4,
                  "raw_length": len(raw), "alpha_mode": "straight, never premultiplied",
                  "alpha_extrema": list(rgba.getchannel("A").getextrema()),
                  "alpha_classification": classify_alpha(image),
                  "generation_wall_seconds": generation_seconds,
                  "png_read_and_rgba_conversion_wall_seconds": read_convert_seconds,
                  "pvrtc_input_dimensions": list(padded.size),
                  "pvrtc_padding": "edge extrusion to power-of-two backing, logical pixels unchanged",
                  "reference_color_space": "8-bit display-encoded RGB, no ICC/gamma conversion",
                  "sample_pixels": [{"xy": list(p), "rgba": list(rgba.getpixel(p))}
                                    for p in [(0, 0), (rgba.width - 1, 0),
                                              (0, rgba.height - 1), (rgba.width // 2, rgba.height // 2)]],
                  "checks": checks}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true", help="test palette/tRNS/alpha classification only")
    parser.add_argument("--resume", action="store_true",
                        help="reuse an existing fixture directory; successful conversions are still executed again")
    args = parser.parse_args()
    self_test = alpha_self_test()
    if args.self_test:
        print(json.dumps(self_test, indent=2, ensure_ascii=False), flush=True)
        return 0 if self_test["failed"] == 0 else 1
    if self_test["failed"]:
        raise ValueError(f"alpha self-test failed: {self_test}")
    if OUTPUT.exists() and any(OUTPUT.iterdir()) and not args.resume:
        parser.error(f"output already contains files: {OUTPUT}; use --resume to explicitly replace this tool's fixtures")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    if (OUTPUT / "manifest.json").exists():
        previous = OUTPUT / "previous-runs" / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
        previous.mkdir(parents=True, exist_ok=False)
        for old_path in [OUTPUT / "manifest.json", *OUTPUT.glob("*.log")]:
            (previous / old_path.name).write_bytes(old_path.read_bytes())
    tools = {"psp2pvrt64": SDK_BIN / "psp2pvrt64.exe", "psp2gxt": SDK_BIN / "psp2gxt.exe"}
    manifest = {"schema": "knife-vita-compressed-synthetic-fixtures-v1",
                "created_utc": datetime.now(timezone.utc).isoformat(),
                "synthetic": True, "no_game_data": True, "actualexecuted": True,
                "gpu_verified": False, "hardware_verified": False,
                "scope": "two generated images, five offline conversions; original PNG/RGBA retained",
                "python_version": platform.python_version(), "pillow_version": PIL.__version__,
                "pillow_native_bcn_encoder": hasattr(Image.core, "bcn_encoder"),
                "output_directory": str(OUTPUT), "sources": [], "fixtures": [],
                "tools": {}, "files": [], "failures": []}
    save_json(OUTPUT / "alpha-self-test.json", self_test)
    try:
        for label, path in {**tools, "python": Path(sys.executable), "script": Path(__file__),
                            "pillow_dds_plugin": Path(DdsImagePlugin.__file__),
                            "pillow_native_core": Path(Image.core.__file__)}.items():
            manifest["tools"][label] = digest(path)
        for name, constructor in [("background", make_background), ("portrait", make_portrait)]:
            source, source_record = generate_source(name, constructor)
            manifest["sources"].append(source_record)
            for codec in (["pvrtc4", "dxt1", "dxt5"] if name == "background" else ["pvrtc4", "dxt5"]):
                fixture = {"name": f"{name}-{codec}", "codec": codec,
                           "gxt": f"{name}-{codec}.gxt", "actualexecuted": False,
                           "source_png": f"{name}.png", "commands": [], "verified": False}
                manifest["fixtures"].append(fixture)
                try:
                    started = time.perf_counter()
                    if codec == "pvrtc4":
                        command = run_tool(f"{name}-{codec}-encode", tools["psp2pvrt64"],
                                           ["-i", f"{name}-pvrtc4-input.tga", "-o", f"{name}-pvrtc4.pvr",
                                            "-d", f"{name}-pvrtc4-decoded.dds", "-f", FORMATS[codec][1],
                                            "-m", "0", "-it", "4", "-th", "1"])
                        fixture["commands"].append(command)
                        if not command["succeeded"]:
                            raise ValueError("PVRTC encoder failed; see command logs")
                        input_name = f"{name}-pvrtc4.pvr"
                    else:
                        if codec == "dxt1" and not classify_alpha(source)["fully_opaque"]:
                            raise ValueError("DXT1 is forbidden for any nonopaque source pixel")
                        input_name = f"{name}-{codec}.dds"
                        encode_start = time.perf_counter()
                        source.save(OUTPUT / input_name, format="DDS", pixel_format=codec.upper())
                        fixture["pillow_encode_wall_seconds"] = time.perf_counter() - encode_start
                        header = (OUTPUT / input_name).read_bytes()[:128]
                        if header[:4] != b"DDS " or header[84:88] != codec.upper().encode("ascii"):
                            raise ValueError("Pillow did not emit the requested legacy DDS format")
                        fixture["dds_alpha_mode"] = "straight source, no pre-multiplication transform"
                    layout_arguments = ["-i", input_name, "-o", fixture["gxt"]]
                    # The PVR already contains PoT padding and needs a logical
                    # window. NPOT DDS keeps its own dimensions: supplying a
                    # window there would violate the SDK's enclosing-PoT check.
                    if codec == "pvrtc4":
                        layout_arguments += ["-w", str(source.width), "-h", str(source.height)]
                    command = run_tool(f"{name}-{codec}-layout", tools["psp2gxt"], layout_arguments)
                    fixture["commands"].append(command)
                    fixture["conversion_wall_seconds"] = time.perf_counter() - started
                    fixture["actualexecuted"] = True
                    if not command["succeeded"]:
                        raise ValueError("GXT layout conversion failed; see command logs")
                    fixture["gxt_metadata"] = inspect_gxt(OUTPUT / fixture["gxt"], source.size, codec)
                    decoded_name = f"{name}-pvrtc4-decoded.dds" if codec == "pvrtc4" else input_name
                    fixture["cpu_reference"] = cpu_reference(name, codec, source, OUTPUT / decoded_name)
                    fixture["verified"] = fixture["gxt_metadata"]["verified"]
                    if not fixture["verified"]:
                        raise ValueError("actual GXT header/layout differs from expected contract; inspect checks")
                except Exception as exc:
                    fixture["error"] = f"{type(exc).__name__}: {exc}"
                    manifest["failures"].append({"fixture": fixture["name"], "error": fixture["error"]})
                    print(f"FAIL {fixture['name']}: {fixture['error']}", flush=True)
    except Exception as exc:
        manifest["failures"].append({"stage": "setup/source", "error": f"{type(exc).__name__}: {exc}"})
    manifest["verified"] = len(manifest["fixtures"]) == 5 and all(
        fixture["verified"] for fixture in manifest["fixtures"]) and not manifest["failures"]
    for path in sorted(OUTPUT.rglob("*")):
        if path.is_file() and path not in (OUTPUT / "manifest.json", OUTPUT / "manifest.sha256"):
            record = digest(path)
            record["path"] = path.relative_to(OUTPUT).as_posix()
            manifest["files"].append(record)
    save_json(OUTPUT / "manifest.json", manifest)
    manifest_digest = digest(OUTPUT / "manifest.json")["sha256"]
    (OUTPUT / "manifest.sha256").write_text(manifest_digest + "  manifest.json\n", encoding="ascii")
    print(json.dumps({"verified": manifest["verified"], "fixtures": len(manifest["fixtures"]),
                      "failures": manifest["failures"], "manifest": str(OUTPUT / "manifest.json"),
                      "manifest_sha256": manifest_digest}, ensure_ascii=False), flush=True)
    return 0 if manifest["verified"] else 1


if __name__ == "__main__":
    raise SystemExit(main())

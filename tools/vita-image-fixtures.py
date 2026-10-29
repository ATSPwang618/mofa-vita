"""Generate small original decoder fixtures; never reads or copies game data."""
import json
import struct
import sys
import zlib
from pathlib import Path

from PIL import Image


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def png(width, height, depth, color_type, raw, *, interlace=0, extra=b""):
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, depth, color_type, 0, 0, interlace))
            + extra + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
pixels = bytes([255, 17, 0, 0, 0, 255, 31, 64, 7, 11, 255, 255,
                89, 45, 12, 127, 13, 22, 33, 128, 201, 202, 203, 1])
rgba = png(3, 2, 8, 6, b"\x00" + pixels[:12] + b"\x00" + pixels[12:])
(output / "rgba.png").write_bytes(rgba)
adam = bytearray()
for x0, y0, dx, dy in [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8),
                       (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]:
    if x0 >= 3 or y0 >= 2:
        continue
    for y in range(y0, 2, dy):
        adam.append(0)
        for x in range(x0, 3, dx):
            adam.extend(pixels[(y * 3 + x) * 4:(y * 3 + x + 1) * 4])
(output / "interlaced.png").write_bytes(png(3, 2, 8, 6, bytes(adam), interlace=1))
(output / "rgba.expected").write_bytes(pixels)
(output / "palette.png").write_bytes(png(2, 2, 8, 3, b"\x00\x00\x01\x00\x02\x01",
    extra=chunk(b"PLTE", bytes([255, 0, 0, 0, 255, 0, 0, 0, 255])) + chunk(b"tRNS", bytes([0, 91, 255]))))
(output / "gray-alpha.png").write_bytes(png(2, 1, 8, 4, bytes([0, 17, 91, 201, 255])))
(output / "gray16.png").write_bytes(png(2, 1, 16, 0, bytes([0, 0x12, 0x34, 0xab, 0xcd])))
(output / "oversize-width.png").write_bytes(png(961, 1, 8, 6, bytes([0, 1, 2, 3, 4])))
(output / "oversize-height.png").write_bytes(png(1, 1081, 8, 6, bytes([0, 1, 2, 3, 4])))
(output / "huge-dimensions.png").write_bytes(png(0x7fffffff, 0x7fffffff, 8, 6, bytes([0, 1, 2, 3, 4])))
(output / "strip.png").write_bytes(png(960, 1080, 8, 6, (b"\x00" + bytes([37, 61, 83, 127]) * 960) * 1080))
bad_end = bytearray(rgba)
bad_end[-1] ^= 1
(output / "bad-end-crc.png").write_bytes(bad_end)

rgb = Image.new("RGB", (32, 16))
for y in range(16):
    for x in range(32):
        rgb.putpixel((x, y), (240, 20, 10) if x < 16 else (15, 225, 35))
for progressive in [False, True]:
    rgb.save(output / ("progressive.jpg" if progressive else "rgb.jpg"),
             quality=100, subsampling=0, progressive=progressive)
Image.new("L", (7, 3), 117).save(output / "gray.jpg", quality=100)
Image.new("RGB", (961, 1), (1, 2, 3)).save(output / "oversize.jpg", quality=90)
Image.new("CMYK", (8, 8), (15, 25, 35, 45)).save(output / "cmyk.jpg", quality=90)

# A valid maximum-size progressive image exercises the internal coefficient
# allocation cap. It is allowed to fail with a budget error, never to overrun it.
Image.new("RGB", (960, 1080), (40, 70, 100)).save(output / "large-progressive.jpg",
                                               quality=90, subsampling=0, progressive=True)
Image.new("RGB", (960, 1080), (40, 70, 100)).save(output / "large-baseline.jpg",
                                               quality=90, subsampling=0)
Image.new("RGB", (960, 540), (40, 70, 100)).save(output / "canvas.jpg",
                                              quality=90, subsampling=2)
for sampling in (1, 2):
    Image.new("RGB", (479, 269), (40, 70, 100)).save(
        output / ("hw-420.jpg" if sampling == 2 else "hw-422.jpg"),
        quality=90, subsampling=sampling)
    # Y pitch 464 has independently aligned 240-byte chroma rows, not 232.
    Image.new("RGB", (463, 269), (40, 70, 100)).save(
        output / ("hw-chroma-420.jpg" if sampling == 2 else "hw-chroma-422.jpg"),
        quality=90, subsampling=sampling)
manifest = {"scope": "Original synthetic images; no game data", "files": []}
for item in sorted(output.iterdir()):
    if item.is_file() and item.name != "manifest.json":
        import hashlib
        manifest["files"].append({"name": item.name, "bytes": item.stat().st_size,
                                   "sha256": hashlib.sha256(item.read_bytes()).hexdigest()})
(output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
print(f"Generated {len(manifest['files'])} original fixture files in {output}")

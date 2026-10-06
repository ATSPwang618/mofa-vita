"""Prepare the batch fixture: python font_dirlist.py /path/to/Arial.ttf.

Uses a locally installed font; font bytes are never checked into the repository.
"""
from pathlib import Path
import struct
import sys
import zlib

root = Path("target/font-dirlist")
root.mkdir(parents=True, exist_ok=True)
(root / "MiXeD").mkdir(exist_ok=True)
(root / "UPPER.txt").write_text("not a font")
original = Path(sys.argv[1]).read_bytes()
assert b"\x00A\x00r\x00i\x00a\x00l" in original, "supply Arial.ttf"
def renamed(name):
    return original.replace("Arial".encode("utf-16-be"), name.encode("utf-16-be")).replace(b"Arial", name.encode())
single = renamed("KrkrX")
(root / "single.ttf").write_bytes(single)
offset = 20
parts, offsets = [], []
for name in ("KrkrY", "KrkrZ"):
    data = bytearray(renamed(name))
    for i in range(struct.unpack_from(">H", data, 4)[0]):
        pos = 12 + i * 16 + 8
        struct.pack_into(">I", data, pos, struct.unpack_from(">I", data, pos)[0] + offset)
    offsets.append(offset)
    parts.append(data)
    offset += len(data)
collection = struct.pack(">4sIIII", b"ttcf", 0x10000, 2, *offsets) + b"".join(parts)
(root / "collection.ttc").write_bytes(collection)
def chunk(tag, data):
    return tag + struct.pack("<Q", len(data)) + data
filename = "font.ttf".encode("utf-16-le")
info = struct.pack("<IQQH", 0, len(single), len(single), len(filename)//2) + filename
segment = struct.pack("<IQQQ", 0, 19, len(single), len(single))
index = chunk(b"File", chunk(b"info", info) + chunk(b"segm", segment) +
              chunk(b"adlr", struct.pack("<I", zlib.adler32(single))))
archive = b"XP3\r\n \n\x1a\x8bg\x01" + struct.pack("<Q", 19+len(single))
archive += single + b"\0" + struct.pack("<Q", len(index)) + index
(root / "font.xp3").write_bytes(archive)

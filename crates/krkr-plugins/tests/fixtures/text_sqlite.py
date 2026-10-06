"""Generate this batch's own PNG and read-only SQLite/XP3 fixtures."""
from pathlib import Path
import sqlite3
import struct
import zlib

root = Path('target/text-sqlite')
root.mkdir(parents=True, exist_ok=True)

def png_chunk(tag, data):
    return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', zlib.crc32(tag + data))

png = b'\x89PNG\r\n\x1a\n'
png += png_chunk(b'IHDR', struct.pack('>IIBBBBB', 7, 5, 8, 6, 0, 0, 0))
png += png_chunk(b'IDAT', zlib.compress((b'\x00' + b'\xff\x20\x10\xff' * 7) * 5))
png += png_chunk(b'IEND', b'')
(root / 'icon.png').write_bytes(png)
db = sqlite3.connect(root / 'data.sqlite')
db.executescript("DROP TABLE IF EXISTS fixture; CREATE TABLE fixture(id INTEGER, title TEXT, data BLOB); INSERT INTO fixture VALUES(7, '参照データ', x'0001ff');")
db.close()
data = (root / 'data.sqlite').read_bytes()

def chunk(tag, data):
    return tag + struct.pack('<Q', len(data)) + data

name = 'data.sqlite'.encode('utf-16-le')
info = struct.pack('<IQQH', 0, len(data), len(data), len(name) // 2) + name
segment = struct.pack('<IQQQ', 0, 19, len(data), len(data))
index = chunk(b'File', chunk(b'info', info) + chunk(b'segm', segment) + chunk(b'adlr', struct.pack('<I', zlib.adler32(data))))
(root / 'data.xp3').write_bytes(b'XP3\r\n \n\x1a\x8bg\x01' + struct.pack('<Q', 19 + len(data)) + data + b'\x00' + struct.pack('<Q', len(index)) + index)

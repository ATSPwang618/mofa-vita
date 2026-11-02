"""Decode one original mip-0 texture, matching knife-core's BNTX reader.

Only the BC7/RGBA/BC1/BC3/R8 formats used by selected story art are accepted.
The input is an already extracted/decompressed asset, never an NSP or key file.
"""
import io
import struct

import numpy as np
from PIL import Image


def decode(data):
    if data[:4] != b'BNTX':
        return Image.open(io.BytesIO(data)).convert('RGBA')
    assert data[0x20:0x24] == b'NX  '
    assert struct.unpack_from('<I', data, 0x24)[0] == 1, 'expected a single story texture'
    table = struct.unpack_from('<Q', data, 0x28)[0]
    brti = struct.unpack_from('<Q', data, table)[0]
    assert data[brti:brti + 4] == b'BRTI'
    fmt = struct.unpack_from('<I', data, brti + 0x1c)[0]
    w, h = struct.unpack_from('<II', data, brti + 0x24)
    assert 0 < w <= 16384 and 0 < h <= 16384 and w * h <= 64 * 1024 * 1024
    mode = struct.unpack_from('<H', data, brti + 0x12)[0]
    bh = 1 << (struct.unpack_from('<I', data, brti + 0x34)[0] & 7)
    ptr = struct.unpack_from('<Q', data, brti + 0x70)[0]
    off = struct.unpack_from('<Q', data, ptr)[0]
    bpb = {0x0b01: 4, 0x0b06: 4, 0x0201: 1, 0x1a01: 8, 0x1a06: 8,
           0x1c01: 16, 0x1c06: 16, 0x2001: 16, 0x2006: 16}[fmt]
    block = 1 if fmt in (0x0b01, 0x0b06, 0x0201) else 4
    wb, hb = (w + block - 1) // block, (h + block - 1) // block
    if mode == 1:
        linear = data[off:off + wb * hb * bpb]
        assert len(linear) == wb * hb * bpb
    else:
        y, x = np.indices((hb, wb), dtype=np.uint32)
        xb, gobs = x * bpb, (wb * bpb + 63) // 64
        address = ((y // (8 * bh)) * 512 * bh * gobs + ((y % (8 * bh)) // 8) * 512
                   + (xb // 64) * 512 * bh + ((xb % 64) // 32) * 256
                   + ((y % 8) // 2) * 64 + (y % 2) * 16
                   + ((xb % 32) // 16) * 32 + xb % 16)
        indices = address[:, :, None] + np.arange(bpb, dtype=np.uint32) + off
        assert int(indices.max()) < len(data), 'truncated Tegra texture'
        linear = np.frombuffer(data, dtype=np.uint8)[indices].tobytes()
    if block == 1:
        return Image.frombytes('L' if bpb == 1 else 'RGBA', (w, h), linear).convert('RGBA')
    fourcc = b'DX10' if fmt in (0x2001, 0x2006) else b'DXT1' if bpb == 8 else b'DXT5'
    words = ([124, 0x81007, h, w, len(linear), 0, 0] + [0] * 11
             + [32, 4, int.from_bytes(fourcc, 'little'), 0, 0, 0, 0, 0]
             + [0x1000, 0, 0, 0, 0])
    header = b'DDS ' + struct.pack('<31I', *words)
    if fourcc == b'DX10':
        header += struct.pack('<5I', 98, 3, 0, 1, 0)
    return Image.open(io.BytesIO(header + linear)).convert('RGBA')

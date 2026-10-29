"""Offline helpers for the 960x540 compact build. Never edits input files."""
from __future__ import annotations

import io
import struct
import zlib
from PIL import Image


def compact_image(data: bytes) -> tuple[Image.Image, tuple[int, int], int]:
    magic, version, lw, lh, width, height, encoding, size = struct.unpack_from('<4s7I', data)
    assert (magic, version) == (b'KIMG', 1)
    payload = data[32:32 + size]
    assert len(payload) == size
    if encoding in (3, 4):
        # DDS + DX10 describe the existing linear BC7 blocks; no swizzle or colour conversion.
        words = [124, 0x81007, height, width, len(payload), 0, 1] + [0] * 11
        words += [32, 4, int.from_bytes(b'DX10', 'little'), 0, 0, 0, 0, 0]
        words += [0x1000, 0, 0, 0, 0]
        header = b'DDS ' + struct.pack('<31I', *words) + struct.pack('<5I', 98, 3, 0, 1, 0)
        image = Image.open(io.BytesIO(header + payload)).convert('RGBA')
    else:
        image = Image.open(io.BytesIO(payload)).convert('RGBA')
    assert image.size == (width, height)
    return image, (lw, lh), encoding


def fit_size(width: int, height: int) -> tuple[int, int]:
    factor = min(1.0, 960 / width, 540 / height)
    return max(1, round(width * factor)), max(1, round(height * factor))


def encode_portable(image: Image.Image, logical: tuple[int, int], *, srgb: bool = True,
                    force_png: bool = False, strips: int = 1, quality: int = 88) -> tuple[bytes, dict]:
    """Use lossless PNG for alpha/UI/masks and baseline JPEG for opaque photos."""
    image = image.convert('RGBA')
    assert strips > 0 and image.height % strips == 0
    old_size = image.size
    rows = image.height // strips
    width, height = fit_size(image.width, rows)
    if (width, height) != (image.width, rows):
        result = Image.new('RGBA', (width, height * strips))
        for k in range(strips):
            # Pillow resizes RGBA in premultiplied space, avoiding transparent fringes.
            piece = image.crop((0, k * rows, image.width, (k + 1) * rows))
            result.paste(piece.resize((width, height), Image.Resampling.LANCZOS), (0, k * height))
        image = result
    output = io.BytesIO()
    has_alpha = image.getchannel('A').getextrema() != (255, 255)
    use_png = has_alpha or force_png or not srgb or strips > 1
    if use_png:
        image.save(output, 'PNG', compress_level=6)
        encoding = 2 if srgb else 7
        fmt = 'PNG'
    else:
        image.convert('RGB').save(output, 'JPEG', quality=quality, subsampling=2,
                                  optimize=True, progressive=False)
        encoding, fmt = 1, 'JPEG'
    payload = output.getvalue()
    header = struct.pack('<4s7I', b'KIMG', 1, *logical, *image.size, encoding, len(payload))
    return header + payload, dict(logical=list(logical), before=list(old_size), physical=list(image.size),
                                  strips=strips, encoding=encoding, format=fmt, has_alpha=has_alpha,
                                  force_png=force_png, srgb=srgb, quality=None if use_png else quality,
                                  payload_bytes=len(payload))


def to_portable(data: bytes, strips: int = 1, force_png: bool = False) -> tuple[bytes, dict]:
    image, logical, encoding = compact_image(data)
    return encode_portable(image, logical, srgb=encoding not in (4, 6, 7),
                           force_png=force_png, strips=strips)


def opus_samples(packet: bytes) -> int:
    toc = packet[0]
    config, code = toc >> 3, toc & 3
    per_frame = (480, 960, 1920, 2880)[config & 3] if config < 12 else ((480 << (config & 1)) if config < 16 else (120 << (config & 3)))
    count = 1 if code == 0 else 2 if code < 3 else packet[1] & 63
    assert 0 < count * per_frame <= 5760
    return count * per_frame


def nopus_packets(data: bytes) -> tuple[int, int, list[bytes]]:
    assert struct.unpack_from('<I', data)[0] == 0x80000001 and len(data) >= 32
    channels = data[9]
    rate, offset = struct.unpack_from('<II', data, 12)
    preskip = struct.unpack_from('<I', data, 28)[0]
    assert rate == 48000 and channels in (1, 2) and preskip <= 65535
    assert struct.unpack_from('<I', data, offset)[0] == 0x80000004
    end = offset + 8 + struct.unpack_from('<I', data, offset + 4)[0]
    assert end <= len(data)
    position, packets = offset + 8, []
    while position < end:
        size = struct.unpack_from('>I', data, position)[0]
        position += 8
        packet = data[position:position + size]
        assert len(packet) == size and size > 0 and position + size <= end
        opus_samples(packet)
        packets.append(packet)
        position += size
    assert position == end and packets
    return channels, preskip, packets


def _crc_table() -> tuple[int, ...]:
    table = []
    for byte in range(256):
        value = byte << 24
        for _ in range(8):
            value = ((value << 1) ^ (0x04c11db7 if value & 0x80000000 else 0)) & 0xffffffff
        table.append(value)
    return tuple(table)


CRC_TABLE = _crc_table()


def ogg_crc(data: bytes) -> int:
    value = 0
    for byte in data:
        value = ((value << 8) & 0xffffffff) ^ CRC_TABLE[(value >> 24) ^ byte]
    return value


def nopus_to_ogg(data: bytes) -> tuple[bytes, int, int]:
    """Losslessly remux Nintendo Opus packets, retaining the exact decoded sample count."""
    channels, preskip, packets = nopus_packets(data)
    output, sequence, granule = bytearray(), 0, 0

    def page(content: bytes, gp: int, flags: int = 0) -> None:
        nonlocal sequence
        lacing = bytes([255] * (len(content) // 255) + [len(content) % 255])
        assert len(lacing) <= 255
        header = struct.pack('<4sBBQIIIB', b'OggS', 0, flags, gp, 0x4b4e4946, sequence, 0, len(lacing))
        record = bytearray(header + lacing + content)
        struct.pack_into('<I', record, 22, ogg_crc(record))
        output.extend(record)
        sequence += 1

    page(struct.pack('<8sBBHIhB', b'OpusHead', 1, channels, preskip, 48000, 0, 0), 0, 2)
    vendor = b'knife compact offline converter'
    page(b'OpusTags' + struct.pack('<I', len(vendor)) + vendor + struct.pack('<I', 0), 0)
    for k, packet in enumerate(packets):
        granule += opus_samples(packet)
        page(packet, granule, 4 if k == len(packets) - 1 else 0)
    return bytes(output), channels, granule - preskip


def nx_wrap(data: bytes) -> bytes:
    payload = zlib.compress(data, level=1)
    return struct.pack('<4sIII', b'NXCX', len(data), len(payload), 0) + payload

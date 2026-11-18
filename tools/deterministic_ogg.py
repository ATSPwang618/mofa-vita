"""Preserve encoded audio; reproduce Ogg page serials and checksums exactly.

The recipe contains hashes and a stream serial, never encoded audio. The entire
stream with only serial/CRC fields zeroed must match before anything is changed.
This catches different tags, packets, page boundaries, granules and trim values.
"""
import hashlib
import struct
import zlib

REVERSE = bytes(int(f'{n:08b}'[::-1], 2) for n in range(256))


def crc(data):
    reflected = zlib.crc32(bytes(data).translate(REVERSE), 0xffffffff) ^ 0xffffffff
    return int.from_bytes(reflected.to_bytes(4, 'little').translate(REVERSE), 'big')


def pages(data):
    pos = 0
    serial = None
    sequence = 0
    while pos < len(data):
        if len(data)-pos < 27 or data[pos:pos+5] != b'OggS\0':
            raise ValueError('Invalid/truncated Ogg page')
        count = data[pos+26]
        lace = data[pos+27:pos+27+count]
        end = pos+27+count+sum(lace)
        if len(lace) != count or end > len(data):
            raise ValueError('Truncated Ogg lacing/payload')
        current, seq = struct.unpack_from('<II', data, pos+14)
        if serial is None:
            serial = current
        if current != serial or seq != sequence:
            raise ValueError('Chained/reordered Ogg not supported by this recipe')
        yield pos, end
        pos = end
        sequence += 1
    if not sequence:
        raise ValueError('Empty Ogg')


def canonical(data):
    result = bytearray(data)
    for start, end in pages(data):
        result[start+14:start+18] = bytes(4)
        result[start+22:start+26] = bytes(4)
    return result


def contract(data):
    for start, end in pages(data):
        page = bytearray(data[start:end])
        expected = struct.unpack_from('<I', page, 22)[0]
        page[22:26] = bytes(4)
        if crc(page) != expected:
            raise ValueError('Reference Ogg CRC mismatch')
    return dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest(),
                serial=struct.unpack_from('<I', data, 14)[0],
                canonical_sha256=hashlib.sha256(canonical(data)).hexdigest())


def restore(data, recipe):
    if len(data) != recipe['bytes']:
        raise ValueError('Ogg size differs from locked encoding')
    if hashlib.sha256(data).hexdigest() == recipe['sha256']:
        return data
    result = canonical(data)
    if hashlib.sha256(result).hexdigest() != recipe['canonical_sha256']:
        raise ValueError('Ogg encoding/page layout differs; serial repair cannot hide audio changes')
    for start, end in pages(data):
        struct.pack_into('<I', result, start+14, recipe['serial'])
        struct.pack_into('<I', result, start+22, crc(result[start:end]))
    if hashlib.sha256(result).hexdigest() != recipe['sha256']:
        raise ValueError('Restored Ogg does not match locked output')
    return bytes(result)

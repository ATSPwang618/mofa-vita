"""Write the runtime KAUDMRG1 format, without holding all audio in memory."""
import argparse
import json
from pathlib import Path
import struct
import zlib


def pack(members, output):
    """members: name -> (path, byte offset, byte length); payloads remain unchanged."""
    if not 0 < len(members) <= 16384:
        raise ValueError('Bank member count outside runtime contract')
    index = bytearray()
    cursor = 16 + 40*len(members)
    for name, (path, offset, size) in sorted(members.items()):
        encoded = name.encode('ascii')
        if (not encoded or len(encoded) >= 32 or any(c in name for c in '/\\:\0')
                or name in ('.', '..') or size <= 0 or offset < 0
                or offset + size > path.stat().st_size or cursor + size > 0xffffffff):
            raise ValueError(f'Invalid bank member: {name}')
        index.extend(encoded.ljust(32, b'\0') + struct.pack('<II', cursor, size))
        cursor += size
    with output.open('xb') as destination:
        destination.write(struct.pack('<8sII', b'KAUDMRG1', len(members), zlib.crc32(index)))
        destination.write(index)
        for name, (path, offset, size) in sorted(members.items()):
            with path.open('rb') as source:
                source.seek(offset)
                remaining = size
                while remaining:
                    block = source.read(min(1024*1024, remaining))
                    if not block:
                        raise ValueError(f'Short read: {name}')
                    destination.write(block)
                    remaining -= len(block)
        if destination.tell() != cursor:
            raise ValueError('Bank size mismatch')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--stream', type=Path, required=True)
    parser.add_argument('--names', type=Path, required=True, help='JSON array of exact filenames')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    names = json.loads(args.names.read_text('utf8'))
    if len(set(names)) != len(names):
        raise ValueError('Duplicate member names')
    members = {}
    for name in names:
        path = (args.stream / name).resolve()
        if not path.is_relative_to(args.stream.resolve()):
            raise ValueError('Unsafe path')
        members[name] = path, 0, path.stat().st_size
    pack(members, args.output)


if __name__ == '__main__':
    main()

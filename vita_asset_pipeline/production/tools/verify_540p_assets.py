"""Independently validate every compact archive record against its original input."""
from __future__ import annotations

import argparse
import collections
import csv
import io
import json
import mmap
from pathlib import Path
import struct
import time
import zlib

from PIL import Image

from create_540p import ARCHIVES, DEFAULT_OUT, DEFAULT_SOURCE, digest, write_json

ROOT = Path(__file__).resolve().parents[1]
SECTOR = 2048


def entries(folder: Path, name: str) -> list[tuple[int, int, int]]:
    data = (folder / f'{name}.hed').read_bytes()
    assert len(data) % 8 == 0 and data[-8:] == b'\xff' * 8, f'{name}: HED terminator'
    records = list(struct.iter_unpack('<IHH', data))
    terminal = next(i for i, row in enumerate(records) if row[0] == 0xffffffff)
    assert all(row == (0xffffffff, 65535, 65535) for row in records[terminal:]), f'{name}: HED terminal padding'
    rows = records[:terminal]
    nam_size = len((folder / f'{name}.nam').read_bytes())
    assert nam_size % 32 == 0 and nam_size >= len(rows) * 32, f'{name}: NAM count'
    return rows


def stored(mm: mmap.mmap, entry: tuple[int, int, int], *, missing: bool = False) -> bytes:
    if missing:
        return b''
    offset, sectors, _ = entry
    start, count = offset * SECTOR, sectors * SECTOR
    assert start + count <= len(mm), f'Entry outside MRG: {entry}'
    if mm[start:start + 4] in (b'NXCX', b'NXGX'):
        count = 16 + struct.unpack_from('<I', mm, start + 8)[0]
        assert start + count <= len(mm), f'NX stream outside MRG: {entry}'
    return mm[start:start + count]


def decoded(data: bytes, limit: int | None = None) -> bytes:
    if data[:4] not in (b'NXCX', b'NXGX'):
        return data if limit is None else data[:limit]
    unpacked, size = struct.unpack_from('<II', data, 4)
    assert len(data) == 16 + size
    assert unpacked <= 128 * 1024 * 1024
    decoder = zlib.decompressobj(31 if data[:4] == b'NXGX' else 15)
    output = decoder.decompress(data[16:], unpacked + 1 if limit is None else limit)
    if limit is None:
        assert decoder.eof and not decoder.unused_data and len(output) == unpacked, 'Invalid NX length / checksum'
    return output


def original_size(data: bytes) -> tuple[int, int]:
    head = decoded(data, 65536)
    if head[:4] == b'BNTX':
        assert struct.unpack_from('<I', head, 0x24)[0] == 1
        table = struct.unpack_from('<Q', head, 0x28)[0]
        brti = struct.unpack_from('<Q', head, table)[0]
        assert brti + 0x2c <= len(head)
        return struct.unpack_from('<II', head, brti + 0x24)
    with Image.open(io.BytesIO(head)) as image:
        return image.size


def validate(folder: Path, source: Path, name: str, scale: float, strips: int) -> dict:
    started = time.monotonic()
    before, after = entries(source, name), entries(folder, name)
    names = (source / f'{name}.nam').read_bytes()
    assert names == (folder / f'{name}.nam').read_bytes(), f'{name}: names or order changed'
    assert len(before) == len(after), f'{name}: entry count changed'
    with (folder / f'{name}.compact.tsv').open(encoding='utf-8') as stream:
        report = list(csv.DictReader(stream, delimiter='\t'))
    assert len(report) == len(after), f'{name}: report count'
    counts, screen_sizes = collections.Counter(), collections.Counter()
    samples = []
    with (source / f'{name}.mrg').open('rb') as a, (folder / f'{name}.mrg').open('rb') as b, \
            mmap.mmap(a.fileno(), 0, access=mmap.ACCESS_READ) as old, \
            mmap.mmap(b.fileno(), 0, access=mmap.ACCESS_READ) as new:
        cursor = 0
        for index, (previous, current, row) in enumerate(zip(before, after, report)):
            label = names[index * 32:(index + 1) * 32].split(b'\0')[0].split(b'\r')[0].decode('utf-8')
            assert row['index'] == str(index) and row['name'] == label, f'{name}/{index}: report identity'
            assert current[0] * SECTOR == cursor, f'{name}/{index}: gap / overlap'
            original = stored(old, previous, missing=label == 'NOFILE.EXISTS')
            output = stored(new, current, missing=label == 'NOFILE.EXISTS')
            assert int(row['stored_bytes']) == len(output), f'{name}/{index}: stored length'
            padding = current[1] * SECTOR - len(output)
            assert 0 <= padding < SECTOR
            assert new[cursor + len(output):cursor + current[1] * SECTOR] == bytes(padding), f'{name}/{index}: padding'
            cursor += current[1] * SECTOR
            if row['kind'] == 'original':
                assert original == output, f'{name}/{index}: retained payload changed'
                assert current[2] == previous[2], f'{name}/{index}: retained unpacked size'
                counts['unchanged'] += 1
                continue
            data = decoded(output)
            magic, version, lw, lh, pw, ph, encoding, length = struct.unpack_from('<4s7I', data)
            assert (magic, version) == (b'KIMG', 1)
            assert len(data) == 32 + length and length > 0
            assert (lw, lh) == original_size(original), f'{name}/{index}: logical size changed'
            assert lh % strips == 0 and ph % strips == 0
            expected_width = max(1, int(lw * scale + 0.5))
            rows = lh // strips
            expected_rows = (min(max(1, int(rows * scale / 4 + 0.5)) * 4, rows)
                             if strips > 1 else max(1, int(rows * scale + 0.5)))
            assert (pw, ph) == (expected_width, expected_rows * strips), f'{name}/{index}: physical size'
            assert 0 < pw <= lw and 0 < ph <= lh
            assert current[2] == (len(data) + SECTOR - 1) // SECTOR
            if row['kind'] == 'bc7':
                assert encoding in (3, 4)
                assert length == ((pw + 3) // 4) * ((ph + 3) // 4) * 16
                if strips > 1:
                    assert (ph // strips) % 4 == 0, f'{name}/{index}: shared language BC7 blocks'
            elif row['kind'] == 'jpeg':
                assert encoding == 1
                with Image.open(io.BytesIO(data[32:])) as image:
                    assert image.size == (pw, ph)
                    image.verify()
            else:
                raise AssertionError(f'Unknown converted format: {row}')
            counts[row['kind']] += 1
            screen_sizes[f'{lw}x{lh} -> {pw}x{ph}'] += 1
            if len(samples) < 5:
                samples.append({'index': index, 'name': label, 'logical': [lw, lh], 'physical': [pw, ph]})
        assert cursor == len(new), f'{name}: unindexed trailing data'
    result = {'archive': name, 'entries': len(after), 'counts': dict(counts),
              'all_payloads_checked': True, 'converted_nx_checksums_checked': True,
              'original_logical_dimensions_preserved': True, 'names_and_indices_preserved': True,
              'retained_payloads_byte_identical': True,
              'common_sizes': dict(screen_sizes.most_common(12)), 'samples': samples,
              'sha256': digest(folder / f'{name}.mrg'), 'elapsed_seconds': round(time.monotonic() - started, 2)}
    print(json.dumps(result, ensure_ascii=False), flush=True)
    return result


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=DEFAULT_SOURCE)
    parser.add_argument('--out', type=Path, default=DEFAULT_OUT)
    parser.add_argument('--archives', nargs='+', choices=[a[0] for a in ARCHIVES])
    args = parser.parse_args()
    result = {'source': str(args.source.resolve()), 'output': str(args.out.resolve()), 'archives': [], 'complete': False}
    for name, scale, strips in ARCHIVES:
        if args.archives and name not in args.archives:
            continue
        result['archives'].append(validate(args.out / 'assets', args.source, name, scale, strips))
    result['complete'] = args.archives is None
    report = ROOT / ('doc/540p-assets-validation.json' if result['complete'] else '.work/540p-assets-validation-partial.json')
    write_json(report, result)


if __name__ == '__main__':
    main()

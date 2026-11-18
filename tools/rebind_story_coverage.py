"""Rebind audited coverage locators after repacking; reject changed image bytes.

Changes only indexes in the two existing tables, retaining hashes and rectangles.
Run before building the executable paired with a resized pack.
"""
import argparse
import json
from pathlib import Path
import re
import struct
from resize_story_variants import Reader, ROOT, sha, unpack


def rebind(source, candidate):
    original = json.loads((source / 'vita-540p-assets.json').read_text('utf-8'))
    target = json.loads((candidate / 'vita-540p-assets.json').read_text('utf-8'))
    a = Reader(source, 'jallpac', original.get('story_archive_volumes', {}).get('jallpac', 1))
    b = Reader(candidate, 'jallpac', target.get('story_archive_volumes', {}).get('jallpac', 1))
    names_a = {a.name(i):i for i in range(len(a.rows)-1)}
    names_b = {b.name(i):i for i in range(len(b.rows)-1)}
    updates, evidence = [], []
    try:
        for filename in ('vita_live_alpha_data.rs', 'vita_sparse_coverage.rs'):
            path = ROOT / 'knife/knife-render/src' / filename
            original_bytes = path.read_bytes()
            text = original_bytes.decode('utf-8')
            indexes = []

            def replace(match):
                name = match[2]
                old, new = names_a[name], names_b[name]
                raw = unpack(a.read(old))
                other = unpack(b.read(new))
                assert raw[:4] == b'KIMG' and raw == other, (name, 'coverage image changed')
                length = 32 + struct.unpack_from('<I', raw, 28)[0]
                expected = re.search(r'sha256: b"([a-f0-9]{64})"', text[match.end():])[1]
                assert sha(raw[:length]) == expected, (name, 'coverage hash changed')
                indexes.append(new)
                evidence.append(dict(file=filename, name=name, old=old, new=new, sha256=expected))
                return f'index: {new}, entry: "{name}"'

            changed = re.sub(r'index: (\d+), entry: "(KQ[^"]+)"', replace, text)
            assert indexes == sorted(indexes) and len(indexes) == len(set(indexes))
            updates.append((path, changed.encode('utf-8')))
        # All files validate before the first write. Preserve their line endings.
        for path, data in updates:
            path.write_bytes(data)
    finally:
        a.volume.__exit__(None, None, None)
        b.volume.__exit__(None, None, None)
    return evidence


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    args = parser.parse_args()
    evidence = rebind(args.source, args.candidate)
    (args.candidate.parent / 'coverage-rebind.json').write_text(json.dumps(evidence, indent=2)+'\n', 'utf-8')
    print('Hash-verified coverage locators:', len(evidence))

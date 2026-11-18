"""Verify the resized pack against its untouched source, including every fallback."""
import argparse
import hashlib
import json
from pathlib import Path
import struct

from generate_vita_asset_profile_540 import audio_bank_locations
from resize_story_variants import Reader, unpack
from vita_image_metadata import check_story_rgba_budget


def digest_range(path, offset, size):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        source.seek(offset)
        while size:
            block = source.read(min(size, 1024*1024))
            assert block, path
            digest.update(block)
            size -= len(block)
    return digest.hexdigest()


def verify(source, candidate, report):
    source_marker = (source / 'vita-540p-assets.json').read_bytes()
    assert hashlib.sha256(source_marker).hexdigest() == report['source_manifest_sha256']
    original = json.loads(source_marker)
    manifest = json.loads((candidate / 'vita-540p-assets.json').read_bytes())
    banks = audio_bank_locations(candidate)
    for row in manifest['files']:
        path = candidate / row['path']
        location = (path, 0, path.stat().st_size) if path.is_file() else banks[row['path']]
        assert location[2] == row['bytes'], row['path']
        assert digest_range(*location) == row['sha256'], row['path']
    print('Manifest hashes passed:', len(manifest['files']), flush=True)
    originals = original['story_gxt_variants']['original_entries']
    assert originals == manifest['story_gxt_variants']['original_entries']
    # Check unrelated physical files too: audio-bank headers and data are unchanged.
    unchanged_files = 0
    for path in source.rglob('*'):
        if not path.is_file():
            continue
        relative = path.relative_to(source)
        if str(relative) == 'vita-540p-assets.json' or any(str(relative).startswith(base+'.') for base in originals):
            continue
        other = candidate / relative
        assert other.stat().st_size == path.stat().st_size
        assert digest_range(path, 0, path.stat().st_size) == digest_range(other, 0, other.stat().st_size), relative
        unchanged_files += 1
    base_count, fallback_count, kept = 0, 0, 0
    for base, count in originals.items():
        a = Reader(source, base, original.get('story_archive_volumes', {}).get(base, 1))
        b = Reader(candidate, base, manifest.get('story_archive_volumes', {}).get(base, 1))
        assert a.names[:count*32] == b.names[:count*32]
        for index in range(count):
            assert a.rows[index][1:] == b.rows[index][1:]
            assert a.read(index) == b.read(index), (base, index)
            base_count += 1
        for row in (r for r in report['variants'] if r['archive'] == base):
            old, new = row['old_index'], row['new_index']
            assert a.name(old) == b.name(new) == row['name']
            raw_a, raw_b = unpack(a.read(old)), unpack(b.read(new))
            assert raw_a[:16] == raw_b[:16]  # format/version/logical coordinate contract
            assert list(struct.unpack_from('<II', raw_a, 16)) == row['before']
            assert list(struct.unpack_from('<II', raw_b, 16)) == row['after']
            if not row['keep']:
                assert row['after'] == [max(1, round(n*report['scale'])) for n in row['before']]
            if row['tiled']:
                old_ref = struct.unpack_from('<I', raw_a, 32)[0]
                new_ref = struct.unpack_from('<I', raw_b, 32)[0]
                assert a.read(old_ref) == b.read(new_ref), (base, old, 'fallback')
                fallback_count += 1
                if row['keep']:
                    count_tiles = struct.unpack_from('<I', raw_a, 24)[0]*struct.unpack_from('<I', raw_a, 28)[0]
                    for slot in range(1, count_tiles+1):
                        ai = struct.unpack_from('<I', raw_a, 32+slot*20)[0]
                        bi = struct.unpack_from('<I', raw_b, 32+slot*20)[0]
                        assert a.read(ai) == b.read(bi)
            elif row['keep']:
                assert a.read(old) == b.read(new)
            kept += int(row['keep'])
        a.volume.__exit__(None, None, None)
        b.volume.__exit__(None, None, None)
        print('Original bytes / logical dimensions / fallbacks passed:', base, flush=True)
    budget = check_story_rgba_budget(lambda name:candidate/name, manifest)
    result = dict(manifest_files=len(manifest['files']), unchanged_files=unchanged_files,
                  original_entries_identical=base_count, fallbacks_identical=fallback_count,
                  variants=len(report['variants']), resized=len(report['variants'])-kept,
                  preserved_variants=kept, production_metadata_and_budget=budget,
                  source_bytes=sum(p.stat().st_size for p in source.rglob('*') if p.is_file()),
                  candidate_bytes=sum(p.stat().st_size for p in candidate.rglob('*') if p.is_file()),
                  hardware_tested=False)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--candidate', type=Path, required=True)
    args = parser.parse_args()
    report = json.loads((args.candidate.parent / 'resize-report.json').read_text('utf-8'))
    result = verify(args.source, args.candidate, report)
    (args.candidate.parent / 'verification.json').write_text(json.dumps(result, indent=2)+'\n', 'utf-8')
    print(json.dumps(result, indent=2), flush=True)

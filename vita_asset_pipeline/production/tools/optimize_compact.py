"""Losslessly optimize the retained 540p PNG/JPEG archives and editable PNGs.

No original HD inputs are required. Each archive is staged and its complete HED
mapping is verified before replacement. PNGs retain exact decoded RGBA bytes,
including RGB values below fully transparent pixels. JPEG bytes are untouched.
"""
from __future__ import annotations

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import copy
from datetime import datetime
import hashlib
import io
import json
import mmap
from pathlib import Path
import struct
import time
import zlib

from PIL import Image, ImageChops
from create_540p import digest, write_json
from verify_540p_assets import stored

ROOT = Path(__file__).resolve().parents[1]
RUNTIME = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64'
IMAGE_SOURCE = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-image-source'
ARCHIVES = ('allpachdml', 'allpacml', 'jallpachd', 'jallpac', 'allui', 'parts')
METHOD = 'lossless-png-deflate-dedup-v1'


def read(path):
    return json.loads(path.read_text(encoding='utf-8'))


def sha(data):
    return hashlib.sha256(data).hexdigest()


def optimize_png(payload: bytes) -> bytes:
    with Image.open(io.BytesIO(payload)) as original:
        assert original.format == 'PNG' and not getattr(original, 'is_animated', False)
        rgba = original.convert('RGBA')
    channels = rgba.split()
    opaque = channels[3].getextrema() == (255, 255)
    gray = (ImageChops.difference(channels[0], channels[1]).getbbox() is None
            and ImageChops.difference(channels[0], channels[2]).getbbox() is None)
    # Removing redundant channels is exact; never quantize or premultiply alpha.
    variants = [rgba]
    if opaque:
        variants.append(rgba.convert('RGB'))
    if gray:
        variants.append(channels[0] if opaque else Image.merge('LA', (channels[0], channels[3])))
    best = payload
    for candidate in variants:
        stream = io.BytesIO()
        candidate.save(stream, 'PNG', optimize=True, compress_level=9)
        encoded = stream.getvalue()
        if len(encoded) < len(best):
            best = encoded
    with Image.open(io.BytesIO(best)) as check:
        assert check.size == rgba.size
        assert check.convert('RGBA').tobytes() == rgba.tobytes(), 'PNG RGBA pixel mismatch'
    return best


def textures(data: bytes):
    if data.startswith(b'KIMG'):
        return [('', data)]
    assert data[:4] == b'KATL'
    version, count = struct.unpack_from('<II', data, 4)
    assert version == 1
    offset, result = 12, []
    for _ in range(count):
        n, length = struct.unpack_from('<II', data, offset)
        offset += 8
        label = data[offset:offset + n].decode('utf-8')
        offset += n
        value = data[offset:offset + length]
        offset += length
        assert len(value) == length
        result.append((label, value))
    assert offset == len(data)
    return result


def optimize_portable(data: bytes):
    stats = Counter()
    if data.startswith(b'KIMG'):
        magic, version, lw, lh, w, h, encoding, length = struct.unpack_from('<4s7I', data)
        assert version == 1 and len(data) == length + 32
        payload = data[32:]
        if encoding == 1:
            assert payload[:2] == b'\xff\xd8'
            stats['jpeg_unchanged'] = 1
            return data, stats
        assert encoding in (2, 7) and payload.startswith(b'\x89PNG')
        optimized = optimize_png(payload)
        stats.update(png_verified=1, png_payload_before=len(payload), png_payload_after=len(optimized))
        stats['png_changed'] = int(optimized != payload)
        return data[:28] + struct.pack('<I', len(optimized)) + optimized, stats
    if data.startswith(b'KATL'):
        result = bytearray(data[:12])
        for label, value in textures(data):
            converted, counts = optimize_portable(value)
            stats.update(counts)
            name = label.encode('utf-8')
            result.extend(struct.pack('<II', len(name), len(converted)) + name + converted)
        return bytes(result), stats
    stats['retained_unchanged'] = 1
    return data, stats


def unpack(data):
    if data[:4] in (b'NXCX', b'NXGX'):
        return zlib.decompress(data[16:], 31 if data[:4] == b'NXGX' else 15)
    return data


def wrap_best(raw, original):
    candidates = [original] if unpack(original) == raw else []
    for level in (1, 9):
        stream = zlib.compress(raw, level)
        candidates.append(struct.pack('<4sIII', b'NXCX', len(raw), len(stream), 0) + stream)
    result = min(candidates, key=len)
    assert unpack(result) == raw
    return result


class ArchiveWriter:
    def __init__(self, stream):
        self.stream, self.total, self.duplicates = stream, 0, 0
        self.seen = {}

    def add(self, data, unpacked):
        count = (len(data) + 2047) // 2048
        assert 0 <= count <= 65535 and 0 <= unpacked <= 65535
        key = (sha(data), len(data), unpacked)
        for offset in self.seen.get(key, []):
            self.stream.seek(offset * 2048)
            same = self.stream.read(len(data)) == data
            self.stream.seek(self.total)
            if same:
                self.duplicates += 1
                return struct.pack('<IHH', offset, count, unpacked)
        offset = self.total // 2048
        if data:
            self.stream.seek(self.total)
            self.stream.write(data)
            self.stream.write(bytes(count * 2048 - len(data)))
            self.total += count * 2048
            self.seen.setdefault(key, []).append(offset)
        return struct.pack('<IHH', offset, count, unpacked)


def optimize_archive(runtime, source, name, work, jobs):
    assets = runtime / 'assets'
    manifest_path = assets / (name + '.compact.json')
    manifest = read(manifest_path)
    mrg_path, hed_path = assets / (name + '.mrg'), assets / (name + '.hed')
    assert digest(mrg_path) == manifest['sha256']
    if manifest.get('lossless_optimization', {}).get('method') == METHOD:
        return manifest, manifest['lossless_optimization']
    assert read(source / name / 'manifest.json') == manifest
    stage = work / name
    stage.mkdir(parents=True, exist_ok=True)
    partial, partial_hed = stage / 'optimized.mrg', stage / 'optimized.hed'
    old_hed = hed_path.read_bytes()
    rows = list(struct.iter_unpack('<IHH', old_hed))
    sentinel = next(i for i, row in enumerate(rows) if row[0] == 0xffffffff)
    assert sentinel == len(manifest['records'])
    rows = rows[:sentinel]
    before_sha, before_bytes = manifest['sha256'], mrg_path.stat().st_size
    stats, expected, hed, replacements = Counter(), [], bytearray(), []
    started = time.monotonic()
    with mrg_path.open('rb') as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm, partial.open('w+b') as out, ThreadPoolExecutor(max_workers=jobs) as pool:
        writer = ArchiveWriter(out)

        def convert(index):
            info = copy.deepcopy(manifest['records'][index])
            original = stored(mm, rows[index], missing=info['name'] == 'NOFILE.EXISTS')
            if not original:
                return original, info, Counter(), []
            raw = unpack(original)
            converted, counts = optimize_portable(raw)
            changes = []
            if raw.startswith((b'KIMG', b'KATL')):
                images = textures(converted)
                infos = info.get('textures', [info])
                assert len(images) == len(infos)
                for (_, value), texture in zip(images, infos):
                    payload = value[32:]
                    texture['payload_bytes'] = len(payload)
                    for image in texture['source_files']:
                        path = (source / image['path']).resolve()
                        assert path.is_relative_to(source.resolve())
                        old_image = path.read_bytes()
                        assert len(old_image) == image['bytes'] and sha(old_image) == image['sha256']
                        if image['path'].endswith('.png'):
                            new_image = payload if texture['strips'] == 1 else optimize_png(old_image)
                            with Image.open(io.BytesIO(old_image)) as old, Image.open(io.BytesIO(new_image)) as new:
                                assert old.size == new.size and old.convert('RGBA').tobytes() == new.convert('RGBA').tobytes()
                            counts['source_png_verified'] += 1
                            counts['source_bytes_before'] += len(old_image)
                            counts['source_bytes_after'] += len(new_image)
                            if new_image != old_image:
                                temp = stage / 'images' / image['path']
                                temp.parent.mkdir(parents=True, exist_ok=True)
                                temp.write_bytes(new_image)
                                changes.append((temp, path))
                                image.update(bytes=len(new_image), sha256=sha(new_image))
            result = wrap_best(converted, original) if converted.startswith((b'KIMG', b'KATL')) else original
            info['stored_bytes'] = len(result)
            return result, info, counts, changes

        for start in range(0, len(rows), jobs * 2):
            for data, info, counts, changes in pool.map(convert, range(start, min(start + jobs * 2, len(rows)))):
                # The header's decompressed-sector field retains its established meaning.
                unpacked = (len(unpack(data)) + 2047) // 2048 if data else 0
                hed.extend(writer.add(data, unpacked))
                expected.append((sha(data), len(data)))
                manifest['records'][len(expected) - 1] = info
                stats.update(counts)
                replacements.extend(changes)
            if start % 400 < jobs * 2:
                print(f'{name}: {len(expected)}/{len(rows)}, staged {writer.total / 1048576:.1f} MiB', flush=True)
        hed.extend(old_hed[sentinel * 8:])
        out.flush()
        assert writer.total < before_bytes or name == 'parts', (name, writer.total, before_bytes)
        stats['duplicate_entries_shared'] = writer.duplicates
    partial_hed.write_bytes(hed)
    # Re-read every output entry through the same indexed HED access used by the
    # independent verifier. Repeated and nonmonotonic offsets are intentional.
    output_rows = list(struct.iter_unpack('<IHH', hed))[:sentinel]
    with partial.open('rb') as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm:
        for index, (hash_value, length) in enumerate(expected):
            data = stored(mm, output_rows[index], missing=manifest['records'][index]['name'] == 'NOFILE.EXISTS')
            assert len(data) == length and sha(data) == hash_value, (name, index)
    receipt = dict(method=METHOD, original_sha256=before_sha, sha256=digest(partial),
                   before_bytes=before_bytes, after_bytes=partial.stat().st_size,
                   saved_bytes=before_bytes - partial.stat().st_size, entries_verified=sentinel,
                   statistics=dict(stats), rgba_pixels_exact=True, jpeg_bytes_unchanged=True,
                   labels_indices_logical_coordinates_preserved=True, every_output_entry_hash_verified=True,
                   elapsed_seconds=round(time.monotonic() - started, 2))
    manifest.update(sha256=receipt['sha256'], output_bytes=receipt['after_bytes'], lossless_optimization=receipt)
    # A pending receipt plus staged manifests makes an interrupted commit reviewable.
    write_json(stage / 'manifest.json', manifest)
    write_json(stage / 'commit.json', dict(status='verified-before-replacement', receipt=receipt))
    partial.replace(mrg_path)
    partial_hed.replace(hed_path)
    for temp, destination in replacements:
        temp.replace(destination)
    write_json(manifest_path, manifest)
    write_json(source / name / 'manifest.json', manifest)
    assert digest(mrg_path) == receipt['sha256'] and hed_path.read_bytes() == hed
    write_json(stage / 'commit.json', dict(status='completed', receipt=receipt))
    print(f'{name}: saved {receipt["saved_bytes"] / 1048576:.2f} MiB; all {sentinel} entries verified', flush=True)
    return manifest, receipt


def inventory(assets):
    files = [p for p in assets.rglob('*') if p.is_file()]
    sizes, counts = Counter(), Counter()
    for path in files:
        sizes[path.suffix] += path.stat().st_size
        counts[path.suffix] += 1
    return dict(total_bytes=sum(sizes.values()), files=len(files), bytes_by_extension=dict(sizes), counts_by_extension=dict(counts))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true', help='Apply pixel-verified lossless optimization')
    parser.add_argument('--jobs', type=int, default=8)
    args = parser.parse_args()
    if not args.apply:
        parser.error('Use --apply to optimize the existing retained runtime and image sources')
    assert 1 <= args.jobs <= 16
    work = ROOT / '.work/compact-lossless'
    work.mkdir(parents=True, exist_ok=True)
    validation_path = ROOT / 'doc/compact-assets-validation.json'
    baseline = work / 'before-validation.json'
    if not baseline.exists():
        baseline.write_bytes(validation_path.read_bytes())
        before_runtime = sum(p.stat().st_size for p in RUNTIME.rglob('*') if p.is_file())
        before_source = sum(p.stat().st_size for p in IMAGE_SOURCE.rglob('*') if p.is_file())
        write_json(work / 'before-sizes.json', dict(runtime=before_runtime, image_source=before_source))
        write_json(work / 'save-hashes-before.json', {p.relative_to(RUNTIME).as_posix():digest(p) for folder in ('saves', 'save-backups') for p in (RUNTIME / folder).rglob('*') if p.is_file()})
    images, receipts = [], []
    for name in ARCHIVES:
        manifest, receipt = optimize_archive(RUNTIME, IMAGE_SOURCE, name, work, args.jobs)
        images.append({k:v for k,v in manifest.items() if k != 'records'})
        receipts.append(dict(name=name, **receipt))
        write_json(ROOT / 'doc/compact-lossless-progress.json', dict(method=METHOD, complete=False, archives=receipts))
    write_json(RUNTIME / 'compact-images.json', images)
    top = read(IMAGE_SOURCE / 'manifest.json')
    top['archives'] = images
    write_json(IMAGE_SOURCE / 'manifest.json', top)
    validation = read(baseline)
    for archive, optimized in zip(validation['archives'], images):
        assert archive['name'] == optimized['name']
        archive.update(sha256=optimized['sha256'], output_bytes=optimized['output_bytes'], lossless_optimization=optimized['lossless_optimization'])
    validation['after'] = inventory(RUNTIME / 'assets')
    validation['lossless_optimization'] = dict(method=METHOD, previous_validation_sha256=digest(baseline), archives=receipts, complete=True)
    write_json(validation_path, validation)
    build = read(RUNTIME / 'asset-build.json')
    build.update(images=images, output_bytes=validation['after']['total_bytes'], lossless_optimization=dict(method=METHOD, complete=True))
    write_json(RUNTIME / 'asset-build.json', build)
    for rel, expected in read(work / 'save-hashes-before.json').items():
        assert digest(RUNTIME / rel) == expected, 'Player save changed: ' + rel
    old = read(work / 'before-sizes.json')
    after_runtime = sum(p.stat().st_size for p in RUNTIME.rglob('*') if p.is_file())
    after_source = sum(p.stat().st_size for p in IMAGE_SOURCE.rglob('*') if p.is_file())
    report = dict(as_of=datetime.now().astimezone().isoformat(timespec='seconds'), method=METHOD, complete=True,
                  runtime_before_bytes=old['runtime'], runtime_after_bytes=after_runtime, runtime_saved_bytes=old['runtime'] - after_runtime,
                  image_source_before_bytes=old['image_source'], image_source_after_bytes=after_source, image_source_saved_bytes=old['image_source'] - after_source,
                  formats_preserved=['PNG', 'JPEG'], quantization=False, resolution_changed=False,
                  audio_video_unchanged=True, player_saves_unchanged=True, archives=receipts)
    write_json(ROOT / 'doc/compact-lossless-optimization.json', report)
    write_json(ROOT / 'doc/compact-lossless-progress.json', report)
    print(json.dumps({k:v for k,v in report.items() if k != 'archives'}, ensure_ascii=False, indent=2), flush=True)


if __name__ == '__main__':
    main()

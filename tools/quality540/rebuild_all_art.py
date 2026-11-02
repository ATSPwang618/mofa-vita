#!/usr/bin/env python3
"""Audit and rebuild every full story picture from the user's original RomFS.

Read-only inputs, bounded four-image batches, deterministic archive order.
Language strips, thumbnails, native masks, UI, scripts and audio are retained.
"""
from __future__ import annotations
import argparse
import collections
import concurrent.futures
import json
import math
import shutil
import struct
import zlib
from pathlib import Path

from bntx_image import decode
from rebuild_scene_art import (ARCHIVES, SECTOR, TOOLS, current_image, encode,
                               error, file_sha, json_write, read_entry, sha, target_size, unpack)
from stage_originals import RomFS, names, unpack_original
from PIL import Image, ImageFilter


def original_image(original, rows, archive, index):
    sector, count, _ = rows[archive][index]
    data = original.read_file(archive + '.mrg', sector * SECTOR, count * SECTOR)
    if data[:4] in (b'NXCX', b'NXGX'):
        data = data[:16 + struct.unpack_from('<I', data, 8)[0]]
    data = unpack_original(data)
    fmt = None
    if data[:4] == b'BNTX':
        table = struct.unpack_from('<Q', data, 0x28)[0]
        brti = struct.unpack_from('<Q', data, table)[0]
        fmt = struct.unpack_from('<I', data, brti + 0x1c)[0]
    return data, None if fmt in (0x0201, 0x1d01) else decode(data), fmt


def identity_match(clean, before):
    full = error(clean.resize(before.size, Image.Resampling.LANCZOS), before)
    if full['composite_psnr_db'] > 24 and full['alpha_mae'] < 8:
        return True, dict(full, accepted_by='full-resolution')
    # Fine foliage/gates can lose alpha coverage through the old repeated
    # resampling/BC encodings. Check the underlying composition independently;
    # this is an identity check, not the final full-resolution quality test.
    size = min(128, before.width), min(128, before.height)
    coarse = error(clean.resize(size, Image.Resampling.LANCZOS).filter(ImageFilter.GaussianBlur(.8)),
                   before.resize(size, Image.Resampling.LANCZOS).filter(ImageFilter.GaussianBlur(.8)))
    accepted = coarse['composite_psnr_db'] >= 28 and coarse['alpha_mae'] < 8
    return accepted, dict(full, coarse=coarse, accepted_by='composition' if accepted else None)


def strip_stems(bundle):
    raw = zlib.decompress(bundle.read_bytes())
    assert raw[:4] == b'KXT0'
    offset, segments = 40, []
    for _ in range(struct.unpack_from('<I', raw, 36)[0]):
        address, size = struct.unpack_from('<QI', raw, offset)
        offset += 12
        segments.append((address, raw[offset:offset + size]))
        offset += size
    assert offset == len(raw)
    def read(address, size):
        for start, data in segments:
            if start <= address and address + size <= start + len(data):
                return data[address - start:address - start + size]
        raise ValueError('missing executable image table range')
    result = set()
    for i in range(10821):
        if read(0x2f1068 + 72 * i + 67, 1)[0] & 2:
            result.add(read(0x4049a0 + 32 * i, 32).split(b'\0')[0].decode())
    assert len(result) == 84
    return result


def gpu_bytes(size, alpha):
    w, h = size
    return max(4, 1 << (w - 1).bit_length()) * max(4, 1 << (h - 1).bit_length()) // (1 if alpha else 2)


def dimensions(original, logical, alpha, source_scale, special):
    # The original HD art uses the 1080p coordinate density; half gives 540p.
    # A few confirmed zoom shots use the extra per-image sampling allowance.
    density = 1. if special else source_scale
    desired = (max(1, math.floor(original[0] * density)), max(1, math.floor(original[1] * density)))
    target = target_size(desired, 960 * 1080, 2048, logical)
    cap = (2 if special else 1) * 1024 * 1024
    while gpu_bytes(target, alpha) > cap:
        candidates = []
        for axis in (0, 1):
            extent = target[axis]
            limit = 1 << ((extent - 1).bit_length() - 1)
            if 0 < limit < extent:
                scale = limit / extent
                candidates.append(tuple(max(1, math.floor(v * scale)) for v in target))
        assert candidates
        target = max(candidates, key=lambda p: p[0] * p[1])
    # Avoid doubling the PoT backing solely for a tiny overscan border, e.g.
    # 1050x630 -> 1024x614 retains 95% of pixels in half the GPU allocation.
    for axis in (0, 1):
        extent = target[axis]
        limit = 1 << ((extent - 1).bit_length() - 1)
        if limit <= 0 or limit >= extent:
            continue
        candidate = tuple(max(1, math.floor(v * limit / extent)) for v in target)
        if candidate[0] * candidate[1] >= target[0] * target[1] * .94 and gpu_bytes(candidate, alpha) < gpu_bytes(target, alpha):
            target = candidate
    assert target[0] <= logical[0] and target[1] <= logical[1] and target[0] * target[1] <= 960 * 1080
    return target


def rebuild(assets, output, original, jobs=4, archives=ARCHIVES, only_indices=None):
    assert 1 <= jobs <= 4 and not output.exists()
    assert archives and set(archives) <= set(ARCHIVES)
    source_marker = (assets / 'vita-540p-assets.json').read_bytes()
    manifest = json.loads(source_marker)
    assert manifest['method'] == 'native-540p-encoded-unorm-v2'
    source_names = {a: names(original.read_file(a + '.nam')) for a in ARCHIVES}
    source_index = {a: {} for a in ARCHIVES}
    source_rows = {a: list(struct.iter_unpack('<IHH', original.read_file(a + '.hed'))) for a in ARCHIVES}
    for a in ARCHIVES:
        for i, name in enumerate(source_names[a]):
            source_index[a].setdefault(name, i)
    strips = strip_stems(TOOLS.parents[0] / 'knife/knife/data/exe_tables_witch.bin')
    special = json.loads(Path(__file__).with_name('scenes.json').read_bytes())['stems']
    preserved_details = {(r['archive'], r['index']): r for r in
                         json.loads(Path(__file__).with_name('identity-preserve.json').read_bytes())['entries']}
    output.mkdir(parents=True)
    reports, audits, counts = [], [], collections.Counter()

    def convert(job):
        archive, index, name, stored, old_row = job
        audit = {'archive': archive, 'index': index, 'name': name}
        try:
            raw = unpack(stored)
            if raw[:4] != b'KIMG':
                return stored, old_row[2], None, dict(audit, decision='non-image')
            preserve = preserved_details.get((archive, index))
            if preserve and preserve['name'] == name and preserve['raw_sha256'] == sha(raw):
                return stored, old_row[2], None, dict(audit, decision='reviewed-small-alpha-preserved', reason=preserve['reason'])
            w, h = struct.unpack_from('<II', raw, 16)
            if min(w, h) <= 16:
                return stored, old_row[2], None, dict(audit, decision='tiny-control-texture')
            before, logical, codec = current_image(raw)
            stem = name.split('.')[0]
            if stem in strips or stem.startswith('RULE_') or name == 'NOFILE.EXISTS':
                return stored, old_row[2], None, dict(audit, decision='language-or-mask')
            source = 'jallpachd' if name in source_index['jallpachd'] else archive
            if name not in source_index[source]:
                return stored, old_row[2], None, dict(audit, decision='original-not-present')
            j = source_index[source][name]
            original_bytes, clean, original_format = original_image(original, source_rows, source, j)
            if clean is None:
                return stored, old_row[2], None, dict(audit, decision='native-mask')
            accepted, identity = identity_match(clean, before)
            if identity['accepted_by'] != 'full-resolution' and archive == 'jallpac' and source != archive and name in source_index[archive]:
                normal_index = source_index[archive][name]
                normal_bytes, normal, normal_format = original_image(original, source_rows, archive, normal_index)
                if normal is not None:
                    normal_ok, normal_identity = identity_match(normal, before)
                    if normal_ok and (not accepted or normal_identity['accepted_by'] == 'full-resolution'
                                      or normal_identity['coarse']['composite_psnr_db'] > identity['coarse']['composite_psnr_db'] + 3):
                        source, j, original_bytes, clean, original_format = archive, normal_index, normal_bytes, normal, normal_format
                        accepted, identity = normal_ok, normal_identity
            if not accepted:
                return stored, old_row[2], None, dict(audit, decision='identity-review', source_identity=identity,
                                                    source_archive=source, source_index=j)
            is_special = any(stem == s or stem.startswith(s + '_') for s in special)
            alpha = clean.getchannel('A').getextrema() != (255, 255)
            physical = dimensions(clean.size, logical, alpha, .5 if source == 'jallpachd' else .75, is_special)
            if physical[0] < before.width or physical[1] < before.height:
                # Retain existing sampling extent if already larger, provided
                # it satisfies the established per-image decoder/GPU contract.
                physical = before.size
            target = clean.resize(physical, Image.Resampling.LANCZOS)
            encoded, after, fmt = encode(target)
            old_error = error(target, before.resize(physical, Image.Resampling.BILINEAR))
            new_error = error(target, after)
            if new_error['composite_psnr_db'] <= old_error['composite_psnr_db'] + .05:
                return stored, old_row[2], None, dict(audit, decision='current-quality-preserved',
                                                    old=old_error, proposed=new_error)
            raw_new = b'KIMG' + struct.pack('<7I', 1, *logical, *physical, 9, len(encoded)) + encoded
            compressed = zlib.compress(raw_new, 9)
            data = b'NXCX' + struct.pack('<III', len(raw_new), len(compressed), 0) + compressed
            report = dict(audit, source_archive=source, source_index=j, original_size=list(clean.size),
                          original_sha256=sha(original_bytes), old_raw_sha256=sha(raw), raw_sha256=sha(raw_new),
                          original_format=original_format,
                          logical=list(logical), old_physical=list(before.size), physical=list(physical),
                          old_codec=codec, codec=9, format=fmt, gpu_payload_bytes=len(encoded) - 64,
                          source_identity=identity, old_at_new_size=old_error, new=new_error)
            export = output / 'images' / archive / f'{index:05d}-{stem}.png'
            export.parent.mkdir(parents=True, exist_ok=True)
            after.save(export, compress_level=6)
            report['export'] = export.relative_to(output).as_posix()
            return data, (len(raw_new) + SECTOR - 1) // SECTOR, report, dict(audit, decision='improved')
        except (AssertionError, ValueError, KeyError, OSError) as ex:
            return stored, old_row[2], None, dict(audit, decision='conversion-review', error=repr(ex))

    for archive in archives:
        hed = (assets / (archive + '.hed')).read_bytes()
        rows = list(struct.iter_unpack('<IHH', hed))
        count = next(i for i, row in enumerate(rows) if row[0] == 0xffffffff)
        table = (assets / (archive + '.nam')).read_bytes()
        target_names = names(table)
        cutoff = next((i for i, n in enumerate(target_names) if n.startswith('THUMSP_TOP.')), count)
        relocated, new_hed = {}, bytearray()
        with (assets / (archive + '.mrg')).open('rb') as source, (output / (archive + '.mrg')).open('wb') as result, \
                concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
            for first in range(0, count, jobs):
                group = []
                for index in range(first, min(count, first + jobs)):
                    row = rows[index]
                    stored = read_entry(source, row)
                    if index < cutoff and stored[:4] in (b'NXCX', b'NXGX', b'KIMG') and (only_indices is None or (archive, index) in only_indices):
                        group.append(pool.submit(convert, (archive, index, target_names[index], stored, row)))
                    else:
                        group.append((stored, row[2], None, {'archive': archive, 'index': index,
                                                          'name': target_names[index], 'decision': 'thumbnail-or-marker'}))
                for k, future in enumerate(group):
                    data, unpacked, report, audit = future.result() if hasattr(future, 'result') else future
                    index = first + k
                    counts[audit['decision']] += 1
                    audits.append(audit)
                    if report:
                        reports.append(report)
                    key = ('old', *rows[index]) if report is None else ('new', sha(data), unpacked)
                    if key in relocated:
                        new_row = relocated[key]
                    else:
                        if report is None:
                            row = rows[index]
                            source.seek(row[0] * SECTOR)
                            padded = source.read(row[1] * SECTOR)
                        else:
                            padded = data + bytes((-len(data)) % SECTOR)
                        new_row = (result.tell() // SECTOR, len(padded) // SECTOR, unpacked)
                        result.write(padded)
                        relocated[key] = new_row
                    new_hed += struct.pack('<IHH', *new_row)
                if first % 200 == 0:
                    print(f'{archive} {first}/{count}: {dict(counts)}', flush=True)
            new_hed += hed[count * 8:]
        (output / (archive + '.hed')).write_bytes(new_hed)
        (output / (archive + '.nam')).write_bytes(table)
        archive_info = next(r for r in manifest['archives'] if r['name'] == archive)
        pixel_delta = sum(r['physical'][0] * r['physical'][1] - r['old_physical'][0] * r['old_physical'][1]
                          for r in reports if r['archive'] == archive)
        archive_info['pixels'] += pixel_delta
        archive_info['bytes'] = (output / (archive + '.mrg')).stat().st_size
        manifest['statistics']['pixels'] += pixel_delta
        json_write(output / (archive + '-quality-audit.json'), {'counts': dict(counts), 'entries': audits})
        json_write(output / (archive + '-quality-report.json'), {'images': reports})
    for archive in archives:
        for suffix in ('hed', 'mrg', 'nam'):
            path = archive + '.' + suffix
            row = next(f for f in manifest['files'] if f['path'] == path)
            row.update(bytes=(output / path).stat().st_size, sha256=file_sha(output / path))
    manifest['statistics']['bytes'] = sum(row['bytes'] for row in manifest['files'])
    manifest['verification']['resampled'] = True
    manifest['global_art_quality_refresh'] = {
        'schema': 'knife-global-540p-art-quality-v1', 'date': '2026-11-01', 'render': [960, 540],
        'source_manifest_sha256': sha(source_marker), 'scope': 'all full story art in jallpac and jallpachd',
        'encoder': 'original art -> Lanczos -> one ISPC BC1/BC3 encoding -> native UNORM GXT',
        'jobs': jobs, 'decisions': dict(counts), 'images_improved': len(reports),
        'pixel_budget_per_image': 960 * 1080, 'default_gpu_payload_cap': 1024 * 1024,
        'confirmed_zoom_gpu_payload_cap': 2 * 1024 * 1024,
        'language_strip_names_preserved': len(strips), 'runtime_hd_fallback': False,
        'geometry_and_cache_budgets_preserved': True}
    manifest['global_art_quality_refresh']['storage_compression'] = 'zlib-9; exact identical encoded entries share sectors'
    json_write(output / 'vita-540p-assets.json', manifest)
    json_write(output / 'quality-report.json', {'source_manifest_sha256': sha(source_marker), 'images': reports})
    json_write(output / 'quality-audit.json', {'counts': dict(counts), 'entries': audits})
    return reports


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', required=True, type=Path)
    parser.add_argument('--romfs', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--jobs', type=int, default=4)
    parser.add_argument('--archives', default=','.join(ARCHIVES))
    args = parser.parse_args()
    print('Improved images:', len(rebuild(args.assets, args.output, RomFS(args.romfs), args.jobs,
                                        tuple(args.archives.split(',')))))

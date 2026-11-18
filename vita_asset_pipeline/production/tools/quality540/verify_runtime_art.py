#!/usr/bin/env python3
"""Verify the sparse offline overlay before replacing any installed local data."""
from __future__ import annotations
import argparse
import collections
import gzip
import io
import json
from pathlib import Path
import struct
import time
import zlib

from PIL import Image
from limit_runtime_art import RGBA_CAP, file_sha, json_write, metadata, read_entry, rgba_charge, sha, unpack
from rebuild_scene_art import current_image


def verify(assets, staged, plan):
    started = time.monotonic()
    original = json.loads((assets/'vita-540p-assets.json').read_bytes())
    updated = json.loads((staged/'vita-540p-assets.json').read_bytes())
    report = json.loads(gzip.decompress((staged/'resample-report.json.gz').read_bytes()))
    if sha((assets/'vita-540p-assets.json').read_bytes()) != plan['source_manifest_sha256']:
        raise ValueError('source manifest differs from preflight')
    if sha((staged/'vita-540p-assets.json').read_bytes()) != report['manifest_sha256']:
        raise ValueError('staged manifest differs from conversion report')
    changed = {(r['archive'],r['index']):r for r in report['images']}
    new_files = {r['path']:r for r in updated['files']}
    checks = collections.Counter()
    for row in original['files']:
        if (assets/row['path']).stat().st_size != row['bytes'] or file_sha(assets/row['path']) != row['sha256']:
            raise ValueError(f'source runtime changed: {row["path"]}')
        path = staged/row['path']
        reference = new_files[row['path']]
        if path.exists():
            if path.stat().st_size != reference['bytes'] or file_sha(path) != reference['sha256']:
                raise ValueError(f'staged identity mismatch: {row["path"]}')
        elif row != reference:
            raise ValueError(f'updated manifest lacks a staged file: {row["path"]}')
        checks['runtime_files_hashed'] += 1
    if set(new_files) != {r['path'] for r in original['files']}:
        raise ValueError('runtime file set changed')
    for archive in sorted({a for a,_ in changed}):
        old_hed = (assets/f'{archive}.hed').read_bytes()
        new_hed = (staged/f'{archive}.hed').read_bytes()
        if len(old_hed) != len(new_hed) or (assets/f'{archive}.nam').read_bytes() != (staged/f'{archive}.nam').read_bytes():
            raise ValueError(f'archive names/order changed: {archive}')
        with (assets/f'{archive}.mrg').open('rb') as old, (staged/f'{archive}.mrg').open('rb') as new:
            for index, (before,after) in enumerate(zip(struct.iter_unpack('<IHH',old_hed),struct.iter_unpack('<IHH',new_hed))):
                if before[0] == 0xffffffff:
                    if old_hed[index*8:] != new_hed[index*8:]:
                        raise ValueError('archive terminator changed')
                    break
                row = changed.get((archive,index))
                if row is None:
                    old.seek(before[0]*2048); new.seek(after[0]*2048)
                    if before[1:] != after[1:] or old.read(before[1]*2048) != new.read(after[1]*2048):
                        raise ValueError(f'untargeted sector bytes changed: {archive}:{index}')
                    checks['unselected_entries_byte_identical'] += 1
                    continue
                raw_old = unpack(read_entry(old,before))[:row['before']['envelope_bytes']]
                raw_new = unpack(read_entry(new,after))
                if sha(raw_old) != row['raw_sha256'] or sha(raw_new) != row['raw_sha256_after']:
                    raise ValueError(f'entry content differs from report: {archive}:{index}')
                info = metadata.image_info(raw_new)
                if info['logical'] != row['before']['logical'] or info['physical'] != row['after']['physical']:
                    raise ValueError('logical/physical metadata mismatch')
                if rgba_charge(*info['physical']) > RGBA_CAP:
                    raise ValueError('RGBA working set still exceeds the policy')
                decoded, logical, codec = current_image(raw_new)
                crc = f'{zlib.crc32(decoded.tobytes()):08x}'
                if crc != row['after']['decoded_rgba_crc32']:
                    raise ValueError(f'round-trip pixel CRC mismatch: {archive}:{index}')
                checks['resized_entries_decoded'] += 1
                checks['logical_sizes_preserved'] += 1
                checks['decoded_pixels'] += decoded.width*decoded.height
        print(f'VERIFIED {archive}: {dict(checks)}',flush=True)
    if checks['resized_entries_decoded'] != len(changed):
        raise ValueError('a resized entry was not verified')
    result=dict(schema='knife-offline-story-memory-verification-v1', checks=dict(checks),
        source_manifest_sha256=plan['source_manifest_sha256'],manifest_sha256=report['manifest_sha256'],
        images_resized=len(changed),max_rgba_allocation=max(r['after']['rgba_allocation_bytes'] for r in report['images']),
        elapsed_seconds=round(time.monotonic()-started,2),on_device_test=False)
    json_write(staged/'verification.json',result)
    return result


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--assets',type=Path,required=True)
    p.add_argument('--staged',type=Path,required=True)
    p.add_argument('--plan',type=Path,required=True)
    a=p.parse_args()
    print(json.dumps(verify(a.assets,a.staged,json.loads(gzip.decompress(a.plan.read_bytes())))))

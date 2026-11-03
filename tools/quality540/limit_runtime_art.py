#!/usr/bin/env python3
"""Preprocess every potentially large story texture BEFORE deployment.

All story colour textures are treated as possible CPU-grade/raw/strip inputs;
the selection never depends on a previously observed crash or scene traversal.
Writes a sparse staging overlay, not a second complete asset pack. The editable
PNG project is an immutable higher resolution input. No runtime resize is added.
"""
from __future__ import annotations

import argparse
import collections
import concurrent.futures
import gzip
import io
import json
import math
from pathlib import Path
import struct
import sys
import time
import zlib

from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import vita_image_metadata as metadata
from rebuild_scene_art import encode, file_sha, json_write, read_entry, sha, unpack
from rebuild_all_art import strip_stems

RGBA_CAP = 1024 * 1024
STORY_ARCHIVES = {'jallpac', 'jallpachd', 'allpacml', 'allpachdml'}
SCHEMA = 'knife-offline-story-memory-budget-v1'


def rgba_charge(w, h):
    """Actual native RGBA pitch, plus the large-allocation page granule."""
    payload = metadata.align(w, 8) * h * 4
    return metadata.align(payload, 64 if payload < 256 * 1024 else 256 * 1024)


def fit_size(w, h, strips=1):
    if not (0 < w <= 2048 and 0 < h <= 2048 and strips in (1, 4) and h >= strips):
        raise ValueError(f'invalid physical shape/strips: {w}x{h}/{strips}')
    if w <= 960 and h <= 540 * strips and rgba_charge(w, h) <= RGBA_CAP:
        return w, h
    # Bound the complete owner too, not just one selected language. Native PNG
    # decode allocates the full image before taking a borrowed strip view.
    rows = h // strips
    high = min(1.0, 960 / w, 540 / rows)
    def shape(scale):
        # Restore an integer boundary lost only to floating-point division.
        return (min(w, 960, max(1, math.floor(w * scale + 1e-9))),
                min(rows, 540, max(1, math.floor(rows * scale + 1e-9))) * strips)
    if rgba_charge(*shape(high)) <= RGBA_CAP:
        return shape(high)
    low = 0.0
    for _ in range(60):
        scale = (low + high) / 2
        tw, th = shape(scale)
        if rgba_charge(tw, th) <= RGBA_CAP:
            low = scale
        else:
            high = scale
    tw, th = shape(low)
    if not (tw <= 960 and th <= 540 * strips and rgba_charge(tw, th) <= RGBA_CAP):
        raise ValueError('offline size policy exceeded its allocation bound')
    return tw, th


def resize_strips(image, size, strips):
    if strips == 1:
        return image.resize(size, Image.Resampling.LANCZOS)
    if size[1] % strips:
        raise ValueError('output language rows must be independently addressable')
    out = Image.new('RGBA', size)
    # Match the engine's floor(k*h/n), floor(h/n) source rectangles exactly;
    # never let Lanczos read a neighbouring language's pixels.
    rows, target_rows = image.height // strips, size[1] // strips
    for k in range(strips):
        top = k * image.height // strips
        part = image.crop((0, top, image.width, top + rows))
        out.paste(part.resize((size[0], target_rows), Image.Resampling.LANCZOS), (0, k * target_rows))
    return out


def encode_image(image, logical, codec, strips):
    size = fit_size(*image.size, strips)
    target = resize_strips(image, size, strips)
    if codec == 9:
        payload, expected, fmt = encode(target)
    elif codec in (1, 2, 7):
        # Preserve PNG UNORM/sRGB metadata. JPEG is written losslessly as PNG,
        # avoiding another JPEG generation after the dimension reduction.
        codec = 7 if codec == 7 else 2
        stream = io.BytesIO()
        target.save(stream, format='PNG', compress_level=9)
        payload, expected, fmt = stream.getvalue(), target, 'PNG_UNORM' if codec == 7 else 'PNG'
    else:
        raise ValueError(f'unsupported resample codec {codec}')
    raw = b'KIMG' + struct.pack('<7I', 1, *logical, *size, codec, len(payload)) + payload
    info = metadata.image_info(raw)
    info.update(rgba_allocation_bytes=rgba_charge(*size),
                decoded_rgba_crc32=f'{zlib.crc32(expected.tobytes()):08x}', output_format=fmt)
    return raw, expected, info


def checked_project_file(project, archive, row):
    path = (project / archive / row['path']).resolve()
    if not path.is_relative_to((project / archive).resolve()):
        raise ValueError(f'project path escapes its archive: {path}')
    return path


def scan(assets, project, strips):
    marker = (assets / 'vita-540p-assets.json').read_bytes()
    manifest = json.loads(marker)
    if manifest['method'] != 'native-540p-encoded-unorm-v2':
        raise ValueError('expected the native encoded UNORM asset ABI')
    expected = {r['path']: r for r in manifest['files']}
    records, counts = [], collections.Counter()
    for archive_row in manifest['archives']:
        archive = archive_row['name']
        for suffix in ('hed', 'mrg', 'nam'):
            name = f'{archive}.{suffix}'
            if name not in expected:
                if suffix == 'nam':
                    continue
                raise ValueError(f'missing source contract: {name}')
            path = assets / name
            if path.stat().st_size != expected[name]['bytes'] or file_sha(path) != expected[name]['sha256']:
                raise ValueError(f'asset source identity changed: {name}')
        path = project / archive / 'manifest.json'
        exported = {e['index']: e for e in json.loads(path.read_bytes())['entries']} if path.exists() else {}
        hed = list(struct.iter_unpack('<IHH', (assets / f'{archive}.hed').read_bytes()))
        names = (assets / f'{archive}.nam').read_bytes() if (assets / f'{archive}.nam').exists() else b''
        cached = {}
        with (assets / f'{archive}.mrg').open('rb') as source:
            for index, row in enumerate(hed):
                if row[0] == 0xffffffff:
                    break
                name = names[index*32:(index+1)*32].split(b'\0')[0].split(b'\r')[0].decode() if names else ''
                base = dict(archive=archive, index=index, name=name, locator=list(row))
                if not row[1]:
                    counts['empty'] += 1
                    records.append(dict(base, decision='empty'))
                    continue
                key = tuple(row)
                if key not in cached:
                    raw = unpack(read_entry(source, row))
                    members = metadata.images_in(raw)
                    if raw[:4] == b'KIMG':
                        raw = raw[:members[0][1]['envelope_bytes']]
                    cached[key] = (sha(raw), members)
                raw_sha, members = cached[key]
                if not members:
                    counts['non-image'] += 1
                    records.append(dict(base, decision='non-image', raw_sha256=raw_sha))
                    continue
                files = [f for f in exported.get(index, {}).get('files', [])
                         if Path(f['path']).suffix.lower() in ('.png', '.jpg', '.jpeg')]
                for ordinal, (member, info) in enumerate(members):
                    w, h = info['physical']
                    n = 4 if name.split('.')[0].upper() in strips else 1
                    if archive not in STORY_ARCHIVES:
                        decision = 'protected-ui-or-boot'
                    elif info.get('png_color_type') in (0, 4) or name.upper().startswith('RULE_'):
                        decision = 'protected-native-mask'
                    elif min(w, h) <= 16:
                        decision = 'protected-control'
                    else:
                        decision = 'resize' if fit_size(w, h, n) != (w, h) else 'already-bounded'
                    record = dict(base, member=member, ordinal=ordinal, raw_sha256=raw_sha,
                        before=info, strips=n, decision=decision,
                        before_rgba_allocation_bytes=rgba_charge(w, h))
                    if decision == 'resize':
                        if len(members) != 1 or len(files) != 1:
                            raise ValueError(f'ambiguous editable colour source {archive}:{index}:{name}')
                        record['source'] = files[0]
                        record['physical'] = list(fit_size(w, h, n))
                    counts[decision] += 1
                    records.append(record)
        print(f'SCAN {archive}: {len(hed)-1} entries, {dict(counts)}', flush=True)
    return dict(schema=SCHEMA, source_manifest_sha256=sha(marker),
        policy=dict(rgba_allocation_cap=RGBA_CAP, maximum_width=960, maximum_height_per_strip=540,
            scope='all story colour textures, irrespective of previously observed runtime use',
            language_strip_names=len(strips), whole_owner_also_bounded=True),
        counts=dict(counts), records=records)


def rebuild(assets, project, output, plan, jobs=4):
    if plan['schema'] != SCHEMA or sha((assets/'vita-540p-assets.json').read_bytes()) != plan['source_manifest_sha256']:
        raise ValueError('scan belongs to another asset manifest')
    if not 1 <= jobs <= 4:
        raise ValueError('use 1..4 bounded conversion workers')
    output.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((assets/'vita-540p-assets.json').read_bytes())
    selected = collections.defaultdict(dict)
    for r in plan['records']:
        if r['decision'] == 'resize':
            selected[r['archive']][r['index']] = r
    reports, changed_files = [], []
    started = time.monotonic()

    def convert(row, stored):
        raw = unpack(stored)
        raw = raw[:row['before']['envelope_bytes']]
        if sha(raw) != row['raw_sha256']:
            raise ValueError(f'archive entry changed: {row["archive"]}:{row["index"]}')
        path = checked_project_file(project, row['archive'], row['source'])
        source = path.read_bytes()
        if sha(source) != row['source']['sha256']:
            raise ValueError(f'editable source changed: {path}')
        with Image.open(io.BytesIO(source)) as original:
            if list(original.size) != row['before']['physical']:
                raise ValueError(f'editable source shape mismatch: {path}')
            image = original.convert('RGBA')
        new, expected_pixels, info = encode_image(image, row['before']['logical'], row['before']['codec'], row['strips'])
        if info['physical'] != row['physical'] or info['rgba_allocation_bytes'] > RGBA_CAP:
            raise ValueError('conversion differs from its preflight policy')
        old_alpha = image.getchannel('A').getextrema()
        if old_alpha == (255, 255) and expected_pixels.getchannel('A').getextrema() != (255, 255):
            raise ValueError('opaque picture unexpectedly became transparent')
        compressed = zlib.compress(new, 9)
        packed = b'NXCX' + struct.pack('<III', len(new), len(compressed), 0) + compressed
        result = dict(row, after=info, raw_sha256_after=sha(new),
            source_image_sha256=sha(source), old_alpha_extrema=list(old_alpha),
            new_alpha_extrema=list(expected_pixels.getchannel('A').getextrema()))
        return packed, (len(new)+2047)//2048, result

    for archive, selection in selected.items():
        hed = (assets/f'{archive}.hed').read_bytes()
        rows = list(struct.iter_unpack('<IHH', hed))
        count = next(i for i, r in enumerate(rows) if r[0] == 0xffffffff)
        new_hed, relocated = bytearray(), {}
        with (assets/f'{archive}.mrg').open('rb') as source, (output/f'{archive}.mrg').open('wb') as dest, \
                concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
            for first in range(0, count, jobs):
                pending = []
                for index in range(first, min(count, first+jobs)):
                    old = rows[index]
                    stored = read_entry(source, old)
                    pending.append(pool.submit(convert, selection[index], stored) if index in selection
                        else (stored, old[2], None))
                for k, job in enumerate(pending):
                    data, decoded, report = job.result() if hasattr(job, 'result') else job
                    index = first+k
                    key = ('new', sha(data), decoded) if report else ('old', *rows[index])
                    if key not in relocated:
                        if report:
                            padded = data + bytes(-len(data)%2048)
                        else:
                            source.seek(rows[index][0]*2048)
                            padded = source.read(rows[index][1]*2048)
                        row = (dest.tell()//2048, len(padded)//2048, decoded)
                        dest.write(padded)
                        relocated[key] = row
                    new_hed += struct.pack('<IHH', *relocated[key])
                    if report:
                        reports.append(report)
                if first % 400 == 0:
                    print(f'PACK {archive} {first}/{count}; changed={len(reports)} elapsed={time.monotonic()-started:.1f}s', flush=True)
            new_hed += hed[count*8:]
        (output/f'{archive}.hed').write_bytes(new_hed)
        (output/f'{archive}.nam').write_bytes((assets/f'{archive}.nam').read_bytes())
        delta = sum(r['after']['physical'][0]*r['after']['physical'][1]
            - r['before']['physical'][0]*r['before']['physical'][1] for r in reports if r['archive']==archive)
        archive_row = next(r for r in manifest['archives'] if r['name']==archive)
        archive_row['pixels'] += delta
        archive_row['bytes'] = (output/f'{archive}.mrg').stat().st_size
        manifest['statistics']['pixels'] += delta
        for suffix in ('hed','mrg','nam'):
            name = f'{archive}.{suffix}'
            f = next(r for r in manifest['files'] if r['path']==name)
            f.update(bytes=(output/name).stat().st_size, sha256=file_sha(output/name))
            changed_files.append(dict(f))
    manifest['statistics']['bytes'] = sum(r['bytes'] for r in manifest['files'])
    manifest['offline_story_memory_budget'] = dict(schema=SCHEMA, **plan['policy'],
        date='2026-11-02', source_manifest_sha256=plan['source_manifest_sha256'],
        images_resized=len(reports), all_story_colour_images_considered=True,
        logical_coordinates_preserved=True, runtime_resampling=False,
        editable_project='higher resolution input retained unchanged; apply this offline policy when repacking')
    json_write(output/'vita-540p-assets.json', manifest)
    report = dict(schema=SCHEMA, source_manifest_sha256=plan['source_manifest_sha256'],
        manifest_sha256=file_sha(output/'vita-540p-assets.json'), policy=plan['policy'],
        scan_counts=plan['counts'], images=reports, files=changed_files,
        source_project_modified=False, elapsed_seconds=round(time.monotonic()-started, 2))
    (output/'resample-report.json.gz').write_bytes(gzip.compress(json.dumps(report,ensure_ascii=False).encode(),mtime=0))
    verification = ''.join(f'{r["archive"]}\t{r["index"]}\t{r["after"]["codec"]}\t'
        f'{r["after"]["physical"][0]}\t{r["after"]["physical"][1]}\t'
        f'{r["before"]["logical"][0]}\t{r["before"]["logical"][1]}\t'
        f'{r["after"]["decoded_rgba_crc32"]}\n' for r in reports)
    (output/'native-verify.tsv').write_text(verification, encoding='utf-8')
    print(f'STAGED images={len(reports)} manifest={report["manifest_sha256"]}', flush=True)
    return report


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--assets', type=Path, required=True)
    p.add_argument('--project', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--plan', type=Path)
    p.add_argument('--scan-only', action='store_true')
    p.add_argument('--jobs', type=int, default=4)
    p.add_argument('--tables', type=Path, default=Path(__file__).resolve().parents[2]/'knife/knife/data/exe_tables_witch.bin')
    args = p.parse_args()
    assets, project, output = args.assets.resolve(), args.project.resolve(), args.output.resolve()
    if output.is_relative_to(assets) or output.is_relative_to(project) or assets.is_relative_to(output) or project.is_relative_to(output):
        raise ValueError('staging output must be separate from runtime and editable inputs')
    plan = json.loads(gzip.decompress(args.plan.read_bytes())) if args.plan else scan(assets,project,strip_stems(args.tables))
    if args.scan_only:
        output.mkdir(parents=True,exist_ok=False)
        (output/'scan.json.gz').write_bytes(gzip.compress(json.dumps(plan,ensure_ascii=False).encode(),mtime=0))
        json_write(output/'scan-summary.json', {k:v for k,v in plan.items() if k!='records'})
    else:
        rebuild(assets,project,output,plan,args.jobs)


if __name__=='__main__':
    main()

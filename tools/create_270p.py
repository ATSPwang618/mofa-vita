"""Build a 480x270 pixel profile from the verified bilingual 540p files.

Every KIMG physical dimension is halved (rounding up), with independent language
strips. Logical coordinates, atlas labels, codecs and archive indices survive.
Only the user's retained local PNG/JPEG assets are required. No HD fallback.
"""
from __future__ import annotations

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import hashlib
import io
import json
import mmap
import os
from pathlib import Path
import shutil
import struct
import time
import zlib

from PIL import Image, ImageChops

from create_540p import digest, write_json
from optimize_compact import ARCHIVES, ArchiveWriter, textures
from verify_540p_assets import decoded, stored

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets'
OUTPUT = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-270p-Vita/assets'
SCHEMA = 'knife-270p-assets-v1'
METHOD = 'half-physical-preserve-logical-codec-strips-v1'
LANGUAGES = 'knife-languages-v1\n1\ten\tEnglish\n2\tzc\t中文（澄空）'


def sha(data):
    return hashlib.sha256(data).hexdigest()


def convert_portable(data, info):
    if data.startswith(b'KATL'):
        members = textures(data)
        infos = info['textures']
        if len(members) != len(infos):
            raise ValueError('Atlas member count mismatch')
        output, rows = bytearray(data[:12]), []
        for (label, member), meta in zip(members, infos):
            if 'name' in meta and meta['name'] != label:
                raise ValueError('Atlas label mismatch')
            converted, records = convert_portable(member, meta)
            name = label.encode('utf-8')
            output.extend(struct.pack('<II', len(name), len(converted)) + name + converted)
            records[0]['label'] = label
            rows.extend(records)
        return bytes(output), rows
    if not data.startswith(b'KIMG'):
        raise ValueError('Expected a portable KIMG/KATL image')
    magic, version, lw, lh, w, h, codec, length = struct.unpack_from('<4s7I', data)
    strips = info.get('strips', 1)
    if (version != 1 or length + 32 != len(data) or codec not in (1, 2, 7)
            or strips not in (1, 2) or w < 1 or h < 1 or h % strips):
        raise ValueError(f'Unsupported KIMG header / language strips: {w}x{h}, codec={codec}, strips={strips}')
    if info.get('physical', [w, h]) != [w, h] or info.get('logical', [lw, lh]) != [lw, lh]:
        raise ValueError('KIMG dimensions do not match the source manifest')
    with Image.open(io.BytesIO(data[32:])) as original:
        if original.size != (w, h) or original.format != ('JPEG' if codec == 1 else 'PNG'):
            raise ValueError('KIMG payload dimensions / format mismatch')
        rgba = original.convert('RGBA')
    row_height = h // strips
    nw, strip_height = max(1, (w + 1) // 2), max(1, (row_height + 1) // 2)
    resized = Image.new('RGBA', (nw, strip_height * strips))
    opaque = rgba.getchannel('A').getextrema() == (255, 255)
    for index in range(strips):
        part = rgba.crop((0, index * row_height, w, (index + 1) * row_height))
        # Premultiplied filtering avoids dark/colored fringes on split sprites.
        part = (part if opaque else part.convert('RGBa')).resize(
            (nw, strip_height), Image.Resampling.LANCZOS).convert('RGBA')
        resized.paste(part, (0, index * strip_height))
    encoded = io.BytesIO()
    if codec == 1:
        resized.convert('RGB').save(encoded, 'JPEG', quality=88, subsampling=2, optimize=True)
    else:
        image = resized.convert('RGB') if opaque else resized
        if codec == 7 and opaque:
            red, green, blue, _ = resized.split()
            if not ImageChops.difference(red, green).getbbox() and not ImageChops.difference(red, blue).getbbox():
                image = red
        # Do not quantize masks/transparent edges to an indexed color palette.
        image.save(encoded, 'PNG', compress_level=6)
    payload = encoded.getvalue()
    output = struct.pack('<4s7I', magic, version, lw, lh, nw, strip_height * strips, codec, len(payload)) + payload
    with Image.open(io.BytesIO(payload)) as check:
        if check.size != resized.size:
            raise ValueError('Encoded dimensions mismatch')
        output_pixels = check.convert('RGBA')
        if codec != 1 and output_pixels.tobytes() != resized.tobytes():
            raise ValueError('PNG failed exact resized pixel verification')
        decoded_sha = sha(output_pixels.tobytes())
    return output, [dict(logical=[lw, lh], before=[w, h], physical=list(resized.size),
                         codec=codec, strips=strips, source_kimg_sha256=sha(data),
                         output_kimg_sha256=sha(output), decoded_rgba_sha256=decoded_sha)]


def identity(filename):
    return dict(bytes=filename.stat().st_size, sha256=digest(filename))


def read_json(filename):
    return json.loads(filename.read_text(encoding='utf-8'))


def archive_rows(filename):
    hed = filename.read_bytes()
    if len(hed) % 8 or hed[-8:] != b'\xff' * 8:
        raise ValueError('Invalid archive terminator')
    rows = list(struct.iter_unpack('<IHH', hed))
    terminal = next(i for i, row in enumerate(rows) if row[0] == 0xffffffff)
    if any(row != (0xffffffff, 65535, 65535) for row in rows[terminal:]):
        raise ValueError('Invalid archive terminal padding')
    return rows[:terminal], hed[terminal * 8:]


def convert_archive(source, output, name, jobs):
    started = time.monotonic()
    source_mrg, source_hed = source / (name + '.mrg'), source / (name + '.hed')
    audit = source / (name + '.compact.json')
    manifest = read_json(audit)
    inputs = {p.name: identity(p) for p in (source_mrg, source_hed, audit)}
    nam = source / (name + '.nam')
    if nam.exists():
        inputs[nam.name] = identity(nam)
    if inputs[source_mrg.name]['sha256'] != manifest['sha256']:
        raise ValueError(f'{name}: source is not the audited 540p file')
    receipt_path = output / (name + '.270p.json')
    if receipt_path.exists():
        receipt = read_json(receipt_path)
        if (receipt['inputs'] == inputs and receipt['method'] == METHOD
                and receipt['converter_sha256'] == digest(Path(__file__))
                and all(identity(output / file) == expected for file, expected in receipt['outputs'].items())):
            print(f'{name}: reuse complete verified 270p conversion', flush=True)
            return receipt
        raise ValueError(f'{name}: existing output does not match this conversion')
    rows, terminal = archive_rows(source_hed)
    if len(rows) != len(manifest['records']):
        raise ValueError(f'{name}: manifest record count mismatch')
    records, hed, expected, stats = [], bytearray(), [], Counter()
    partial = output / (name + '.mrg.partial')
    with source_mrg.open('rb') as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm, \
            partial.open('w+b') as out, ThreadPoolExecutor(max_workers=jobs) as pool:
        writer = ArchiveWriter(out)

        def convert(index):
            meta = manifest['records'][index]
            before = stored(mm, rows[index], missing=meta['name'] == 'NOFILE.EXISTS')
            raw = decoded(before)
            kind = meta['kind']
            if raw.startswith((b'KIMG', b'KATL')):
                converted, image_rows = convert_portable(raw, meta)
                packed = zlib.compress(converted, 6)
                data = struct.pack('<4sIII', b'NXCX', len(converted), len(packed), 0) + packed
                unpacked = (len(converted) + 2047) // 2048
            else:
                if kind not in ('retained-data', 'removed-language'):
                    raise ValueError(f'{name}/{index}: unexpected non-image {kind}')
                data, image_rows, unpacked = before, [], rows[index][2]
            return data, unpacked, dict(index=index, name=meta['name'], kind=kind, textures=image_rows)

        # The batch bound limits decoded images and encoder outputs in flight.
        for start in range(0, len(rows), jobs * 2):
            for data, unpacked, record in pool.map(convert, range(start, min(start + jobs * 2, len(rows)))):
                hed.extend(writer.add(data, unpacked))
                expected.append((len(data), sha(data)))
                records.append(record)
                stats['entries'] += 1
                stats['textures'] += len(record['textures'])
                for image in record['textures']:
                    stats['pixels_before'] += image['before'][0] * image['before'][1]
                    stats['pixels_after'] += image['physical'][0] * image['physical'][1]
                    stats[f'codec_{image["codec"]}'] += 1
                    stats['language_strips'] += int(image['strips'] > 1)
            if start % 400 < jobs * 2:
                print(f'{name}: {len(records)}/{len(rows)}, {writer.total / 1048576:.1f} MiB', flush=True)
        hed.extend(terminal)
        out.flush()
        stats['shared_duplicate_entries'] = writer.duplicates
    # Reopen every sector mapping, check its hash, and reopen each encoded image.
    output_rows = list(struct.iter_unpack('<IHH', hed))[:len(rows)]
    with partial.open('rb') as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm:
        for index, row in enumerate(output_rows):
            record = records[index]
            data = stored(mm, row, missing=record['name'] == 'NOFILE.EXISTS')
            if (len(data), sha(data)) != expected[index]:
                raise ValueError(f'{name}/{index}: sector mapping verification failed')
            raw = decoded(data)
            if record['textures']:
                members = textures(raw)
                if len(members) != len(record['textures']):
                    raise ValueError('Reopened atlas count mismatch')
                for (label, member), image in zip(members, record['textures']):
                    _, _, lw, lh, w, h, codec, size = struct.unpack_from('<4s7I', member)
                    if ([lw, lh] != image['logical'] or [w, h] != image['physical']
                            or codec != image['codec'] or len(member) != size + 32
                            or sha(member) != image['output_kimg_sha256'] or label != image.get('label', '')):
                        raise ValueError('Reopened texture metadata/hash mismatch')
                    with Image.open(io.BytesIO(member[32:])) as check:
                        if check.size != (w, h) or sha(check.convert('RGBA').tobytes()) != image['decoded_rgba_sha256']:
                            raise ValueError('Reopened image pixels mismatch')
    target_mrg = output / (name + '.mrg')
    if target_mrg.exists():
        raise FileExistsError(target_mrg)
    partial.replace(target_mrg)
    (output / source_hed.name).write_bytes(hed)
    if nam.exists():
        shutil.copy2(nam, output / nam.name)
    outputs = {file: identity(output / file) for file in (name + '.mrg', name + '.hed')}
    if nam.exists():
        outputs[nam.name] = identity(output / nam.name)
    receipt = dict(schema='knife-270p-archive-v1', method=METHOD, converter_sha256=digest(Path(__file__)),
                   name=name, inputs=inputs, outputs=outputs, statistics=dict(stats), records=records,
                   verification=dict(all_entries_reopened=True, decoded_pixels_verified=True,
                                     logical_coordinates_preserved=True, codecs_preserved=True,
                                     labels_and_indices_preserved=True, strips_resized_separately=True),
                   seconds=round(time.monotonic() - started, 2))
    write_json(receipt_path, receipt)
    print(f'{name}: VERIFIED, {stats["textures"]} textures, {receipt["seconds"]}s', flush=True)
    return receipt


def build(source, output, jobs):
    source, output = source.resolve(), output.resolve()
    if source == output or source in output.parents or output in source.parents:
        raise ValueError('270p output must be separate from the 540p input')
    if (source / 'language-profile.txt').read_text('utf-8').strip() != LANGUAGES:
        raise ValueError('Expected the bilingual English / Chengkong source')
    output.mkdir(parents=True, exist_ok=True)
    archive_reports = [convert_archive(source, output, name, jobs) for name in ARCHIVES]
    private_names = {name + suffix for name in ARCHIVES for suffix in ('.hed', '.nam', '.mrg')}
    files, loose_images = [], []
    for file in sorted(source.rglob('*')):
        if not file.is_file():
            continue
        relative = file.relative_to(source).as_posix()
        if relative in private_names or relative.endswith(('.compact.json', '.compact.tsv')) or relative == 'asset-build.json':
            continue
        if file.is_symlink():
            raise ValueError(f'Source symlink unsupported: {relative}')
        dest = output / relative
        dest.parent.mkdir(parents=True, exist_ok=True)
        suffix = file.suffix.lower()
        if suffix in ('.png', '.jpg', '.jpeg', '.tga'):
            with Image.open(file) as image:
                before = image.size
                size = tuple(max(1, (n + 1) // 2) for n in before)
                image.resize(size, Image.Resampling.LANCZOS).save(dest)
            loose_images.append(dict(path=relative, before=list(before), physical=list(size)))
            mode = 'private-resized-loose-image'
        else:
            if dest.exists():
                if identity(dest) != identity(file):
                    raise FileExistsError(dest)
            else:
                try:
                    os.link(file, dest)
                except OSError:
                    shutil.copy2(file, dest)
            mode = 'unchanged-link-or-copy'
        files.append(dict(path=relative, mode=mode, **identity(dest)))
    summaries = []
    for report in archive_reports:
        for name, record in report['outputs'].items():
            files.append(dict(path=name, mode='private-270p-archive', **record))
        filename = report['name'] + '.270p.json'
        files.append(dict(path=filename, mode='conversion-audit', **identity(output / filename)))
        summaries.append({k: report[k] for k in ('name', 'statistics', 'verification', 'seconds')})
    summary = Counter()
    for report in archive_reports:
        summary.update(report['statistics'])
    result = dict(schema=SCHEMA, method=METHOD, source_assets=str(source), output_assets=str(output),
                  converter_sha256=digest(Path(__file__)), render=[480, 270], display=[960, 540],
                  scale=dict(numerator=1, denominator=2, rounding='ceil-per-language-strip'),
                  languages=['en', 'zc'], codecs=[1, 2, 7], archives=summaries, loose_images=loose_images,
                  statistics=dict(summary), total_bytes=sum(f['bytes'] for f in files), files=files,
                  verification=dict(all_pass=True, archives=6, all_textures_resized=True,
                                    all_entries_reopened=True, logical_coordinates_preserved=True,
                                    decoded_pixels_verified=True, codecs_preserved=True,
                                    strips_resized_separately=True, hd_fallback=False))
    write_json(output / 'vita-270p-assets.json', result)
    print(json.dumps({k: result[k] for k in ('schema', 'statistics', 'total_bytes', 'verification')}, ensure_ascii=False), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=SOURCE)
    parser.add_argument('--out', type=Path, default=OUTPUT)
    parser.add_argument('--jobs', type=int, default=4)
    args = parser.parse_args()
    if not 1 <= args.jobs <= 16:
        parser.error('--jobs must be 1..16')
    build(args.source, args.out, args.jobs)


if __name__ == '__main__':
    main()

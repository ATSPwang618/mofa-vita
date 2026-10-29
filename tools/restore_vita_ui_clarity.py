"""Restore only undersampled UI from the retained 540p pack, with a pixel audit.

Scene archives stay byte-identical. Small UI members already dense enough stay
byte-identical too. No runtime fallback or external high-resolution dependency.
"""
from __future__ import annotations

import argparse
from collections import Counter
import io
import json
import mmap
from pathlib import Path
import shutil
import struct
import time
import zlib

from PIL import Image, ImageChops

from create_270p import SOURCE, OUTPUT, archive_rows, identity, read_json, sha
from create_540p import digest, write_json
from optimize_compact import ArchiveWriter, textures
from verify_540p_assets import stored, decoded

ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / '.work/vita/audio-ui-clarity-20261028'
METHOD = 'ui-selective-native-half-v1'
GLOBAL_METHOD = '270p-scenes-540p-ui-preserve-logical-v1'


def target_size(logical, baseline, current, strips):
    if (strips not in (1, 2) or any(len(s) != 2 for s in (logical, baseline, current))
            or any(type(n) is not int or n <= 0 for s in (logical, baseline, current) for n in s)
            or baseline[1] % strips or current[1] % strips
            or any(a > b for a, b in zip(current, baseline))):
        raise ValueError('Invalid UI dimensions / language strips')
    w = max(current[0], min(baseline[0], (logical[0] + 1) // 2))
    h = max(current[1] // strips,
            min(baseline[1] // strips, (logical[1] + 2 * strips - 1) // (2 * strips))) * strips
    if w > 960 or h // strips > 540:
        raise ValueError('UI restoration exceeds its 960x540 per-strip admission budget')
    return w, h


def restore_portable(baseline, current, infos):
    bm, cm = textures(baseline), textures(current)
    if len(bm) != len(cm) or len(bm) != len(infos):
        raise ValueError('UI atlas count mismatch')
    result, rows = [], []
    for (label, before), (actual_label, old), info in zip(bm, cm, infos):
        if label != actual_label:
            raise ValueError('UI atlas label mismatch')
        b = struct.unpack_from('<4s7I', before)
        c = struct.unpack_from('<4s7I', old)
        if (b[:4] != c[:4] or b[0] != b'KIMG' or b[1] != 1 or b[6] != c[6]
                or b[6] not in (1, 2, 7) or b[7] + 32 != len(before) or c[7] + 32 != len(old)):
            raise ValueError('UI logical coordinates, codec or KIMG header mismatch')
        _, _, lw, lh, bw, bh, codec, _ = b
        _, _, _, _, cw, ch, _, _ = c
        strips = info.get('strips', 1)
        if (info.get('before', [bw, bh]) != [bw, bh]
                or info.get('physical', [cw, ch]) != [cw, ch]
                or info.get('logical', [lw, lh]) != [lw, lh]
                or info.get('source_kimg_sha256', sha(before)) != sha(before)
                or info.get('output_kimg_sha256', sha(old)) != sha(old)):
            raise ValueError('UI input does not match its existing pixel audit')
        size = target_size([lw, lh], [bw, bh], [cw, ch], strips)
        changed = size != (cw, ch)
        if not changed:
            out = old
        elif size == (bw, bh):
            out = before  # Retain exact original encoded bytes at native density.
        else:
            with Image.open(io.BytesIO(before[32:])) as image:
                if image.size != (bw, bh) or image.format != ('JPEG' if codec == 1 else 'PNG'):
                    raise ValueError('UI source encoding mismatch')
                rgba = image.convert('RGBA')
            opaque = rgba.getchannel('A').getextrema() == (255, 255)
            resized = Image.new('RGBA', size)
            for i in range(strips):
                part = rgba.crop((0, i * (bh // strips), bw, (i + 1) * (bh // strips)))
                part = (part if opaque else part.convert('RGBa')).resize(
                    (size[0], size[1] // strips), Image.Resampling.LANCZOS).convert('RGBA')
                resized.paste(part, (0, i * (size[1] // strips)))
            payload = io.BytesIO()
            if codec == 1:
                resized.convert('RGB').save(payload, 'JPEG', quality=88, subsampling=2, optimize=True)
            else:
                image = resized.convert('RGB') if opaque else resized
                if codec == 7 and opaque:
                    r, g, b, _ = resized.split()
                    if not ImageChops.difference(r, g).getbbox() and not ImageChops.difference(r, b).getbbox():
                        image = r
                image.save(payload, 'PNG', compress_level=6)
            encoded = payload.getvalue()
            out = struct.pack('<4s7I', b'KIMG', 1, lw, lh, *size, codec, len(encoded)) + encoded
            if codec != 1:
                with Image.open(io.BytesIO(encoded)) as check:
                    if check.convert('RGBA').tobytes() != resized.tobytes():
                        raise ValueError('Restored UI PNG pixels changed during encoding')
        with Image.open(io.BytesIO(out[32:])) as check:
            if check.size != size or check.format != ('JPEG' if codec == 1 else 'PNG'):
                raise ValueError('UI output encoding mismatch')
            pixel_sha = sha(check.convert('RGBA').tobytes())
        rows.append(dict(label=label, logical=[lw, lh], before=[bw, bh], previous_physical=[cw, ch],
                         physical=list(size), codec=codec, strips=strips, restored=changed,
                         source_kimg_sha256=sha(before), previous_kimg_sha256=sha(old),
                         output_kimg_sha256=sha(out), decoded_rgba_sha256=pixel_sha))
        result.append((label, out))
    if baseline[:4] == b'KIMG' and current[:4] == b'KIMG':
        if len(result) != 1:
            raise ValueError('Invalid standalone KIMG')
        return result[0][1], rows
    if baseline[:12] != current[:12] or baseline[:4] != b'KATL':
        raise ValueError('UI container header mismatch')
    output = bytearray(current[:12])
    for label, out in result:
        name = label.encode('utf-8')
        output += struct.pack('<II', len(name), len(out)) + name + out
    return bytes(output), rows


def build(apply=False):
    started = time.monotonic()
    names = ['allui.mrg', 'allui.hed', 'allui.270p.json', 'vita-270p-assets.json']
    global_old = read_json(OUTPUT / names[-1])
    if global_old['schema'] != 'knife-270p-assets-v1':
        raise ValueError('Expected frozen v1 input; refusing to convert an already modified profile')
    audit_old = read_json(OUTPUT / 'allui.270p.json')
    baseline_meta = read_json(SOURCE / 'allui.compact.json')
    for name, expected in audit_old['inputs'].items():
        if identity(SOURCE / name) != expected:
            raise ValueError('Retained 540p source identity mismatch: ' + name)
    if digest(SOURCE / 'allui.mrg') != baseline_meta['sha256']:
        raise ValueError('Retained 540p UI is not its verified baseline')
    input_ids = {name: identity(OUTPUT / name) for name in names}
    for f in global_old['files']:
        if f['path'] in names and identity(OUTPUT / f['path']) != {k: f[k] for k in ('bytes', 'sha256')}:
            raise ValueError('Current UI input changed: ' + f['path'])
    stage, backup = WORK / 'staged-assets', WORK / 'previous-assets'
    stage.mkdir(parents=True, exist_ok=False)
    backup.mkdir(parents=True, exist_ok=False)
    for name in names:
        shutil.copy2(OUTPUT / name, backup / name)
        if identity(backup / name) != input_ids[name]:
            raise ValueError('Backup verification failed')
    baseline_rows, baseline_terminal = archive_rows(SOURCE / 'allui.hed')
    old_rows, terminal = archive_rows(OUTPUT / 'allui.hed')
    if len(old_rows) != len(baseline_rows) or terminal != baseline_terminal:
        raise ValueError('UI archive layout mismatch')
    records, expected, hed, stats = [], [], bytearray(), Counter()
    with (SOURCE / 'allui.mrg').open('rb') as bf, (OUTPUT / 'allui.mrg').open('rb') as cf, \
            mmap.mmap(bf.fileno(), 0, access=mmap.ACCESS_READ) as bm, \
            mmap.mmap(cf.fileno(), 0, access=mmap.ACCESS_READ) as cm, (stage / 'allui.mrg').open('w+b') as out:
        writer = ArchiveWriter(out)
        for i, old in enumerate(audit_old['records']):
            missing = old['name'] == 'NOFILE.EXISTS'
            previous = stored(cm, old_rows[i], missing=missing)
            base = stored(bm, baseline_rows[i], missing=missing)
            meta = baseline_meta['records'][i]
            if any(old[k] != meta[k] for k in ('index', 'name', 'kind')):
                raise ValueError('UI record mapping changed')
            raw, base_raw = decoded(previous), decoded(base)
            if old['textures']:
                converted, images = restore_portable(base_raw, raw, old['textures'])
                packed = zlib.compress(converted, 6)
                data = struct.pack('<4sIII', b'NXCX', len(converted), len(packed), 0) + packed
                unpacked = (len(converted) + 2047) // 2048
            else:
                data, images, unpacked = previous, [], old_rows[i][2]
            hed += writer.add(data, unpacked)
            expected.append((len(data), sha(data)))
            records.append(dict(index=i, name=old['name'], kind=old['kind'], textures=images))
            stats['entries'] += 1
            stats['textures'] += len(images)
            for image in images:
                stats['pixels_before'] += image['before'][0] * image['before'][1]
                stats['pixels_after'] += image['physical'][0] * image['physical'][1]
                stats['ui_restored'] += int(image['restored'])
                stats['byte_identical_small_ui'] += int(not image['restored'])
                stats['language_strips'] += int(image['strips'] > 1)
                stats['codec_' + str(image['codec'])] += 1
        stats['shared_duplicate_entries'] = writer.duplicates
        hed += terminal
    (stage / 'allui.hed').write_bytes(hed)
    rows, _ = archive_rows(stage / 'allui.hed')
    with (stage / 'allui.mrg').open('rb') as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as mm:
        for i, row in enumerate(rows):
            record = records[i]
            data = stored(mm, row, missing=record['name'] == 'NOFILE.EXISTS')
            if (len(data), sha(data)) != expected[i]:
                raise ValueError('UI sector mapping failed verification')
            if not record['textures']:
                continue
            for (label, member), image in zip(textures(decoded(data)), record['textures'], strict=True):
                head = struct.unpack_from('<4s7I', member)
                if (label != image['label'] or list(head[2:4]) != image['logical']
                        or list(head[4:6]) != image['physical'] or head[6] != image['codec']
                        or sha(member) != image['output_kimg_sha256']):
                    raise ValueError('Reopened UI header / encoded bytes mismatch')
                with Image.open(io.BytesIO(member[32:])) as check:
                    if list(check.size) != image['physical'] or sha(check.convert('RGBA').tobytes()) != image['decoded_rgba_sha256']:
                        raise ValueError('Reopened UI pixels mismatch')
    verification = dict(audit_old['verification'], selective_native_ui_verified=True, unchanged_small_ui_verified=True)
    outputs = {'allui.mrg': identity(stage / 'allui.mrg'), 'allui.hed': identity(stage / 'allui.hed'),
               'allui.nam': identity(OUTPUT / 'allui.nam')}
    audit = dict(schema='knife-270p-archive-v2', name='allui', method=METHOD,
                 converter_sha256=digest(Path(__file__)), inputs=audit_old['inputs'],
                 previous_inputs=input_ids, outputs=outputs, statistics=dict(stats), records=records,
                 verification=verification, seconds=round(time.monotonic() - started, 2))
    write_json(stage / 'allui.270p.json', audit)
    new = dict(global_old)
    new.update(schema='knife-270p-assets-v2', method=GLOBAL_METHOD, composite=[960, 540],
               ui_policy=dict(archive='allui', method=METHOD, max_physical=[960, 540],
                              retain_sufficient_small_ui=True, runtime_hd_fallback=False),
               ui_converter_sha256=digest(Path(__file__)))
    new['archives'] = [{k: audit[k] for k in ('name', 'statistics', 'verification', 'seconds')}
                       if a['name'] == 'allui' else a for a in global_old['archives']]
    total = Counter()
    for a in new['archives']:
        total.update(a['statistics'])
    new['statistics'] = dict(total)
    new['verification'] = dict(global_old['verification'], selective_native_ui_verified=True)
    new['files'] = [dict(f, **identity(stage / f['path'])) if (stage / f['path']).is_file() else f
                    for f in global_old['files']]
    new['total_bytes'] = sum(f['bytes'] for f in new['files'])
    write_json(stage / 'vita-270p-assets.json', new)
    # Guard all four destinations immediately before replacing only those files.
    if any(identity(OUTPUT / name) != input_ids[name] for name in names):
        raise ValueError('Live assets changed during staging; refusing replacement')
    after = {name: identity(stage / name) for name in names}
    report = dict(schema='knife-vita-selective-ui-restore-v1', before=input_ids, after=after,
                  changed_files=names, statistics=dict(stats), other_archives_changed=False,
                  full_reopen_pixel_verification=True, applied=False)
    write_json(WORK / 'restore-report.json', report)
    if apply:
        for name in names:
            shutil.copy2(stage / name, OUTPUT / (name + '.ui-new'))
            if identity(OUTPUT / (name + '.ui-new')) != after[name]:
                raise ValueError('Staged copy failed verification')
        for name in names:
            (OUTPUT / (name + '.ui-new')).replace(OUTPUT / name)
        report['applied'] = True
        write_json(WORK / 'restore-report.json', report)
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true')
    build(parser.parse_args().apply)

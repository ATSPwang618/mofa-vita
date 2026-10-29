"""Retain English and Chengkong assets without changing their original save IDs."""
from __future__ import annotations

import io
import struct
import argparse
import copy
import mmap
import re
from collections import Counter
from datetime import datetime
from PIL import Image
from PIL import ImageDraw, ImageFont
from optimize_compact import (optimize_png, read, sha, unpack, wrap_best, textures,
    ArchiveWriter, inventory, ROOT, RUNTIME, IMAGE_SOURCE, ARCHIVES)
from create_540p import digest, write_json
from verify_540p_assets import stored

KEEP = (1, 2)


def slim_kimg(data, count=4):
    magic, version, lw, lh, w, h, encoding, size = struct.unpack_from('<4s7I', data)
    assert magic == b'KIMG' and version == 1 and len(data) == 32 + size
    assert encoding in (2, 7), 'Language strips must remain lossless PNG'
    assert h % count == 0 and lh % count == 0
    with Image.open(io.BytesIO(data[32:])) as source:
        reduced = source.crop((0, h // count, w, 3 * h // count))
        out = io.BytesIO()
        reduced.save(out, 'PNG', optimize=True, compress_level=9)
        payload = optimize_png(out.getvalue())
        with Image.open(io.BytesIO(payload)) as check:
            assert check.convert('RGBA').tobytes() == reduced.convert('RGBA').tobytes()
    return struct.pack('<4s7I', magic, version, lw, lh // count * 2, w, h // count * 2,
                       encoding, len(payload)) + payload


def sysmes_rows(data):
    languages, count = struct.unpack_from('<II', data)
    result = []
    for lang in range(languages):
        table = struct.unpack_from('<Q', data, 16 + 8 * lang)[0]
        rows = []
        for i in range(count):
            offset = struct.unpack_from('<Q', data, table + 8 * i)[0]
            rows.append(data[offset:data.index(0, offset)])
        result.append(rows)
    return result


def slim_sysmes(data):
    rows = sysmes_rows(data)
    assert len(rows) == 4
    count = len(rows[0])
    output = bytearray(data[:16] + bytes(2 * 8 + 2 * count * 8))
    struct.pack_into('<I', output, 0, 2)
    for slot, lang in enumerate(KEEP):
        table = 16 + 2 * 8 + slot * count * 8
        struct.pack_into('<Q', output, 16 + slot * 8, table)
        for i, value in enumerate(rows[lang]):
            struct.pack_into('<Q', output, table + 8 * i, len(output))
            output.extend(value + bytes([0]))
    assert sysmes_rows(output) == [rows[1], rows[2]]
    return bytes(output)


def unpack_mzp(data):
    assert data[:6] == b'mrgd00'
    count = struct.unpack_from('<H', data, 6)[0]
    base = 8 + count * 8
    result = []
    for i in range(count):
        sector, offset, size_sectors, size_low = struct.unpack_from('<4H', data, 8 + i * 8)
        start = base + (sector << 11) + (offset & 4095) + ((offset >> 12) << 27)
        size = size_low if size_sectors == 0 else ((size_sectors - 1) // 32) * 65536 + size_low
        assert start + size <= len(data)
        result.append(data[start:start + size])
    return result


def pack_mzp(blocks):
    header = bytearray(b'mrgd00' + struct.pack('<H', len(blocks)) + bytes(len(blocks) * 8))
    payload = bytearray()
    for i, block in enumerate(blocks):
        offset, size = len(payload), len(block)
        assert offset < 134217728 and size < 134217728
        large, low = divmod(size, 65536)
        struct.pack_into('<4H', header, 8 + i * 8, offset // 2048, offset % 2048,
                         large * 32 + 1 if large else 0, low)
        payload.extend(block)
    output = bytes(header + payload)
    assert unpack_mzp(output) == blocks
    return output


def language_plate(data):
    magic, version, lw, lh, w, h, encoding, _ = struct.unpack_from('<4s7I', data)
    assert (lw, lh, w, h, encoding) == (552, 440, 552, 440, 2)
    with Image.open(io.BytesIO(data[32:])) as image:
        plate = image.convert('RGBA').crop((0, 88, w, 264))
    plate.paste((0, 0, 0, 0), (0, 88, w, 176))
    draw = ImageDraw.Draw(plate)
    font = ImageFont.truetype(str(RUNTIME / 'assets/zc/arshanghaisonggbpro_lt.otf'), 44)
    draw.text((w // 2, 132), '中文（澄空）', font=font, fill=(255, 255, 255, 255), anchor='mm')
    output = io.BytesIO()
    plate.save(output, 'PNG', optimize=True)
    payload = optimize_png(output.getvalue())
    return struct.pack('<4s7I', magic, version, lw, 176, w, 176, encoding, len(payload)) + payload


def rebuild_archive(name, work):
    assets = RUNTIME / 'assets'
    manifest_path = assets / f'{name}.compact.json'
    manifest = read(manifest_path)
    if manifest.get('bilingual', {}).get('complete'):
        assert digest(assets / f'{name}.mrg') == manifest['sha256']
        return manifest, manifest['bilingual']
    assert read(IMAGE_SOURCE / name / 'manifest.json') == manifest
    stage = work / name
    stage.mkdir(parents=True, exist_ok=True)
    mrg, hed = assets / f'{name}.mrg', assets / f'{name}.hed'
    assert digest(mrg) == manifest['sha256']
    rows = list(struct.iter_unpack('<IHH', hed.read_bytes()))
    assert rows[len(manifest['records'])][0] == 0xffffffff
    removed, changed, expected, new_hed, aliases = [], [], [], bytearray(), []
    old_sha, old_bytes = manifest['sha256'], mrg.stat().st_size
    pending_images = []
    names = bytearray((assets / f'{name}.nam').read_bytes())
    with mrg.open('rb') as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm, (stage / 'new.mrg').open('w+b') as out:
        writer = ArchiveWriter(out)
        for i, record in enumerate(manifest['records']):
            original_name = record['name']
            original = stored(mm, rows[i], missing=original_name == 'NOFILE.EXISTS')
            raw = unpack(original)
            if name == 'allui' and re.search(r'_JA\.NXZ$', original_name):
                base = original_name[:-7]
                aliases.extend((base, t['name']) for t in record['textures'])
            if name == 'allui' and re.search(r'_(JA|ZT|KO)\.NXZ$', original_name):
                paths = [s for t in record['textures'] for s in t['source_files']]
                removed.append(dict(index=i, name=original_name, source_files=paths))
                for s in paths:
                    path = (IMAGE_SOURCE / s['path']).resolve()
                    assert path.is_relative_to(IMAGE_SOURCE.resolve()) and digest(path) == s['sha256']
                record.clear()
                record.update(index=i, name='NOFILE.EXISTS', kind='removed-language', textures=[], removed_name=original_name, stored_bytes=0)
                names[i * 32:(i + 1) * 32] = b'NOFILE.EXISTS' + bytes(17) + b'\r\n'
                result = b''
            else:
                converted = raw
                if raw.startswith((b'KIMG', b'KATL')):
                    converted_images = []
                    infos = record.get('textures', [record])
                    for j, ((label, value), info) in enumerate(zip(textures(raw), infos)):
                        mode = None
                        if info.get('strips') == 4:
                            value, mode = slim_kimg(value), 'strip'
                        elif name == 'allui' and ((i == 49 and j in (170, 247, 256, 260)) or (i == 65 and j in range(128, 134))):
                            value, mode = slim_kimg(value), 'shared-thumbnail'
                        elif name == 'allui' and i in (1, 2) and j == 21:
                            value, mode = language_plate(value), 'language-menu'
                        if mode:
                            _, _, lw, lh, w, h, _, size = struct.unpack_from('<4s7I', value)
                            old_sources = copy.deepcopy(info['source_files'])
                            sources = [s for s in old_sources if s.get('language') in ('en', 'zc')] if mode == 'strip' else old_sources
                            if mode == 'strip':
                                info['strips'] = 2
                                for s in old_sources:
                                    if s not in sources:
                                        removed.append(dict(source_files=[s], kind='strip-image'))
                            info.update(logical=[lw, lh], physical=[w, h], payload_bytes=size, source_files=sources)
                            for s in sources:
                                path = (IMAGE_SOURCE / s['path']).resolve()
                                assert path.is_relative_to(IMAGE_SOURCE.resolve()) and digest(path) == s['sha256']
                                new = path.read_bytes() if mode == 'strip' else value[32:]
                                with Image.open(io.BytesIO(new)) as check:
                                    s.update(physical=list(check.size), bytes=len(new), sha256=sha(new))
                                if mode != 'strip':
                                    temp = stage / 'images' / s['path']
                                    temp.parent.mkdir(parents=True, exist_ok=True)
                                    temp.write_bytes(new)
                                    pending_images.append((temp, path))
                            changed.append(dict(index=i, texture=j, name=label, operation=mode))
                        converted_images.append((label, value))
                    assert len(converted_images) == len(infos)
                    if raw.startswith(b'KIMG'):
                        converted = converted_images[0][1]
                    else:
                        converted = raw[:12] + b''.join(struct.pack('<II', len(label.encode()), len(value)) + label.encode() + value for label, value in converted_images)
                elif name == 'allui' and i == 67:
                    converted = slim_sysmes(raw)
                    changed.append(dict(index=i, operation='system-text-4-to-2'))
                elif name == 'allui' and i == 68:
                    count = struct.unpack_from('<H', raw)[0]
                    converted = raw[:8] + raw[8 + count * 8:8 + count * 8 * 3]
                    assert len(converted) == 8 + count * 8 * 2
                    changed.append(dict(index=i, operation='guide-uv-4-to-2'))
                result = original if converted == raw else wrap_best(converted, original)
                record['stored_bytes'] = len(result)
            new_hed.extend(writer.add(result, (len(unpack(result)) + 2047) // 2048 if result else 0))
            expected.append((len(result), sha(result)))
        new_hed.extend(hed.read_bytes()[len(manifest['records']) * 8:])
    with (stage / 'new.mrg').open('rb') as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm:
        for i, (length, hashed) in enumerate(expected):
            data = stored(mm, struct.unpack_from('<IHH', new_hed, i * 8), missing=manifest['records'][i]['name'] == 'NOFILE.EXISTS')
            assert len(data) == length and sha(data) == hashed
    receipt = dict(complete=True, original_sha256=old_sha, sha256=digest(stage / 'new.mrg'), before_bytes=old_bytes,
        after_bytes=(stage / 'new.mrg').stat().st_size, entries_verified=len(expected), removed=removed, changed=changed,
        retained_language_pixels_exact_except_menu_label=True, stable_archive_indices=True)
    manifest.update(sha256=receipt['sha256'], output_bytes=receipt['after_bytes'], bilingual=receipt)
    if manifest['language_strips'] == 4:
        manifest['language_strips'] = 2
    write_json(stage / 'pending-manifest.json', manifest)
    (stage / 'new.mrg').replace(mrg)
    hed.write_bytes(new_hed)
    (assets / f'{name}.nam').write_bytes(names)
    for temp, path in pending_images:
        temp.replace(path)
    for row in removed:
        for s in row['source_files']:
            path = (IMAGE_SOURCE / s['path']).resolve()
            assert path.is_relative_to(IMAGE_SOURCE.resolve()) and digest(path) == s['sha256']
            path.unlink()
    if aliases:
        (assets / 'ui-aliases.tsv').write_text(''.join(f'{base}\t{label}\n' for base, label in aliases), encoding='utf-8')
    write_json(manifest_path, manifest)
    write_json(IMAGE_SOURCE / name / 'manifest.json', manifest)
    print(f'{name}: {len(changed)} texture/text edits; {len(removed)} removed language records; saved {(old_bytes-receipt["after_bytes"])/1048576:.2f} MiB', flush=True)
    return manifest, receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true')
    assert parser.parse_args().apply, 'Use --apply'
    assets = RUNTIME / 'assets'
    work = ROOT / '.work/compact-bilingual'
    work.mkdir(parents=True, exist_ok=True)
    previous = ROOT / 'doc/compact-assets-validation-before-bilingual.json'
    current = ROOT / 'doc/compact-assets-validation.json'
    if not previous.exists():
        previous.write_bytes(current.read_bytes())
        write_json(work / 'before-sizes.json', {label:sum(p.stat().st_size for p in folder.rglob('*') if p.is_file()) for label, folder in [('runtime', RUNTIME), ('image_source', IMAGE_SOURCE)]})
    validation = read(previous)
    receipts = []
    for name in ('allpachdml', 'allpacml', 'allui'):
        _, receipt = rebuild_archive(name, work)
        receipts.append(dict(name=name, **receipt))
    script = assets / 'script_text.mrg'
    if not (work / 'script.json').exists():
        data = script.read_bytes()
        blocks = unpack_mzp(data)
        assert len(blocks) == 10, f'Expected five original language pairs, got {len(blocks)} blocks'
        selected = blocks[2:6]
        output = pack_mzp(selected)
        receipt = dict(path='script_text.mrg', original_sha256=sha(data), sha256=sha(output), original_blocks=len(blocks), blocks=4, selected_block_sha256=[sha(b) for b in selected], text_bytes_unchanged=True)
        script.write_bytes(output)
        write_json(work / 'script.json', receipt)
    assert digest(script) == read(work / 'script.json')['sha256']
    remove = ['ja/FOT-MatissePro-B.otf', 'zt/SungASC-Medium.TTF', 'nintendo_udjxh-db_zh-tw_003.bfttf', 'saveicon_ja.tga', 'saveicon_zt.tga', 'movie/opening_ja.mp4', 'movie/opening_zt.mp4']
    old_files = {r['path']:r for r in validation['retained_files'] + validation['movies']}
    for rel in remove:
        path = (assets / rel).resolve()
        assert path.is_relative_to(assets.resolve())
        if path.exists():
            assert digest(path) == old_files[rel]['sha256']
            path.unlink()
    for folder in ('ja', 'zt'):
        path = assets / folder
        if path.exists():
            path.rmdir()
    (assets / 'language-profile.txt').write_text('knife-languages-v1\n1\ten\tEnglish\n2\tzc\t中文（澄空）\n', encoding='utf-8')
    summaries, formats = [], Counter()
    for archive in validation['archives']:
        name = archive['name']
        m = read(assets / f'{name}.compact.json')
        images = [s for r in m['records'] for t in r.get('textures', [r]) for s in t.get('source_files', [])]
        f = Counter('PNG' if s['path'].endswith('.png') else 'JPEG' for s in images)
        formats.update(f)
        kinds = Counter(r['kind'] for r in m['records'])
        image_objects = sum(len(r.get('textures', [r])) for r in m['records'] if r['kind'] in ('portable-image', 'portable-atlas'))
        archive.update(sha256=m['sha256'], output_bytes=m['output_bytes'], formats=dict(f), image_views_including_languages=len(images),
            kinds=dict(kinds), image_objects=image_objects, atlas_objects=kinds['portable-atlas'],
            common_physical_sizes=dict(Counter(f'{s["physical"][0]}x{s["physical"][1]}' for s in images).most_common(12)))
        if name in ('allpachdml', 'allpacml', 'allui'):
            archive['bilingual'] = m['bilingual']
        summaries.append({k:v for k,v in m.items() if k != 'records'})
    write_json(RUNTIME / 'compact-images.json', summaries)
    top = read(IMAGE_SOURCE / 'manifest.json')
    top.update(archives=summaries, languages=['en', 'zc'])
    write_json(IMAGE_SOURCE / 'manifest.json', top)
    validation['movies'] = [r for r in validation['movies'] if r['path'] not in remove]
    retained = []
    for row in validation['retained_files']:
        if row['path'] not in remove:
            path = assets / row['path']
            row.update(bytes=path.stat().st_size, sha256=digest(path))
            retained.append(row)
    for rel in ('language-profile.txt', 'ui-aliases.tsv'):
        path = assets / rel
        retained.append(dict(path=rel, bytes=path.stat().st_size, sha256=digest(path)))
    validation.update(retained_files=retained, after=inventory(assets))
    receipt = dict(as_of=datetime.now().astimezone().isoformat(timespec='seconds'), complete=True,
        languages=[dict(id=1, resource_slot=0, code='en', label='English'), dict(id=2, resource_slot=1, code='zc', label='中文（澄空）')],
        original_save_ids_preserved=True, fallback_ids={'0':1, '3':2}, previous_validation_sha256=digest(previous),
        archives=receipts, removed_runtime_files=remove, script=read(work / 'script.json'), image_formats=dict(formats),
        japanese_voice_retained=True, player_saves_modified=False, before=read(work / 'before-sizes.json'))
    validation['bilingual'] = receipt
    write_json(current, validation)
    build = read(RUNTIME / 'asset-build.json')
    build.update(images=summaries, movies=validation['movies'], output_bytes=validation['after']['total_bytes'], bilingual=dict(complete=True, languages=['en', 'zc']))
    write_json(RUNTIME / 'asset-build.json', build)
    receipt['after'] = {label:sum(p.stat().st_size for p in folder.rglob('*') if p.is_file()) for label, folder in [('runtime', RUNTIME), ('image_source', IMAGE_SOURCE)]}
    write_json(ROOT / 'doc/compact-bilingual-optimization.json', receipt)
    print({k:v for k,v in receipt.items() if k not in ('archives','script')}, flush=True)


if __name__ == '__main__':
    main()

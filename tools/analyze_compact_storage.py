"""Measure storage savings on the retained compact assets without changing them."""
from __future__ import annotations

from collections import Counter, defaultdict
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

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets'
SOURCE = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-image-source'
ARCHIVES = ('allpachdml', 'allpacml', 'jallpachd', 'jallpac', 'allui', 'parts')


def png_candidates(payload: bytes):
    with Image.open(io.BytesIO(payload)) as opened:
        rgba = opened.convert('RGBA')
        candidates = [('rgba', rgba)]
        r, g, b, a = rgba.split()
        opaque = a.getextrema() == (255, 255)
        gray = ImageChops.difference(r, g).getbbox() is None and ImageChops.difference(r, b).getbbox() is None
        if opaque:
            candidates.append(('rgb', rgba.convert('RGB')))
        if gray:
            candidates.append(('gray' if opaque else 'gray_alpha', r if opaque else Image.merge('LA', (r, a))))
        results = []
        for label, picture in candidates:
            out = io.BytesIO()
            picture.save(out, 'PNG', optimize=True, compress_level=9)
            encoded = out.getvalue()
            with Image.open(io.BytesIO(encoded)) as decoded:
                assert decoded.convert('RGBA').tobytes() == rgba.tobytes()
            results.append((len(encoded), label, encoded))
        return min(results, key=lambda item: item[0])


def main():
    start = time.monotonic()
    report = dict(as_of=datetime.now().astimezone().isoformat(timespec='seconds'), image_formats_unchanged=True, assets_before_bytes=sum(p.stat().st_size for p in ASSETS.rglob('*') if p.is_file()), archives=[], png_sample=[])
    all_hashes = {}
    source_hashes = {}
    duplicates_by_format = Counter()
    cross_savings = 0
    samples = []
    for name in ARCHIVES:
        manifest = json.loads((ASSETS / (name + '.compact.json')).read_text(encoding='utf8'))
        entries = list(struct.iter_unpack('<IHH', (ASSETS / (name + '.hed')).read_bytes()))
        local_hashes = {}
        saved = count = padding = 0
        examples = []
        with (ASSETS / (name + '.mrg')).open('rb') as stream, mmap.mmap(stream.fileno(), 0, access=mmap.ACCESS_READ) as data:
            for row, record in zip(entries, manifest['records']):
                assert row[0] != 0xffffffff
                if record['name'] == 'NOFILE.EXISTS' or row[1] == 0:
                    continue
                offset = row[0] * 2048
                length = row[1] * 2048
                if data[offset:offset+4] in (b'NXCX', b'NXGX'):
                    length = 16 + struct.unpack_from('<I', data, offset+8)[0]
                content = data[offset:offset+length]
                digest = hashlib.sha256(content).hexdigest()
                allocated = ((length+2047)//2048)*2048
                padding += allocated-length
                if digest in local_hashes:
                    saved += allocated
                    count += 1
                    if len(examples) < 5:
                        examples.append([local_hashes[digest],record['name'],allocated])
                else:
                    local_hashes[digest] = record['name']
                if digest in all_hashes and all_hashes[digest] != name:
                    cross_savings += allocated
                else:
                    all_hashes[digest] = name
                for texture in record.get('textures', [record]):
                    for image in texture.get('source_files', []):
                        key = image['sha256']
                        if key in source_hashes:
                            duplicates_by_format[Path(image['path']).suffix] += image['bytes']
                        else:
                            source_hashes[key] = image['path']
                            if image['path'].lower().endswith('.png'):
                                samples.append(image)
        result = dict(name=name, bytes_before=(ASSETS/(name+'.mrg')).stat().st_size, entries=len(manifest['records']), duplicate_entries=count, exact_duplicate_savings=saved, sector_padding_bytes=padding, examples=examples)
        report['archives'].append(result)
        print(json.dumps(result,ensure_ascii=False),flush=True)
    report['exact_duplicate_savings_total'] = sum(r['exact_duplicate_savings'] for r in report['archives'])
    report['cross_archive_duplicate_bytes_not_independently_additive'] = cross_savings
    report['editable_exact_duplicate_bytes'] = dict(duplicates_by_format)
    samples.sort(key=lambda x:x['bytes'])
    selected = {x['path']:x for x in [samples[i*(len(samples)-1)//31] for i in range(32)]+samples[-8:]}
    for sample in selected.values():
        payload = (SOURCE / sample['path']).read_bytes()
        after, mode, _ = png_candidates(payload)
        report['png_sample'].append(dict(path=sample['path'], before=len(payload), after=min(len(payload),after), mode=mode, rgba_pixels_identical=True))
    report['png_sample_before'] = sum(s['before'] for s in report['png_sample'])
    report['png_sample_after'] = sum(s['after'] for s in report['png_sample'])
    report['elapsed_seconds'] = round(time.monotonic()-start,2)
    (ROOT / 'doc/compact-storage-analysis.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n',encoding='utf8')
    print(json.dumps({k:v for k,v in report.items() if k not in ('archives','png_sample')},ensure_ascii=False),flush=True)


if __name__ == '__main__':
    main()

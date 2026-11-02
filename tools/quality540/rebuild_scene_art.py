#!/usr/bin/env python3
"""Rebuild only selected zoomed story art from staged original images.

Required: Pillow, numpy, ispc_texcomp==1.0.1. No keys or commercial input images
are part of this tool. Stage originals as described in README.md. Writes a
separate output directory; it never changes input archives or source images.
"""
from __future__ import annotations

import argparse
import hashlib
import importlib.util
import io
import json
import math
import shutil
import struct
import sys
import zlib
from pathlib import Path

import numpy as np
from PIL import Image
import ispc_texcomp

from bntx_image import decode

TOOLS = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('native_gxt', TOOLS / 'migrate-vita-native-color.py')
gxt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gxt)
SECTOR = 2048
ARCHIVES = ('jallpac', 'jallpachd')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def file_sha(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def json_write(path, obj):
    path.write_text(json.dumps(obj, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def target_size(size, budget, maximum, logical):
    w, h = size
    scale = min(1., maximum / w, maximum / h, logical[0] / w, logical[1] / h,
                math.sqrt(budget / (w * h)))
    result = max(1, math.floor(w * scale)), max(1, math.floor(h * scale))
    assert result[0] * result[1] <= budget and max(result) <= maximum
    assert result[0] <= logical[0] and result[1] <= logical[1]
    return result


def encode(image):
    w, h = image.size
    bw, bh = max(4, gxt.pow2(w)), max(4, gxt.pow2(h))
    padded = Image.fromarray(np.pad(np.asarray(image), ((0, bh - h), (0, bw - w), (0, 0)), mode='edge'))
    opaque = image.getchannel('A').getextrema() == (255, 255)
    name, fmt, bpe = ('DXT1', 0x85000000, 8) if opaque else ('DXT5', 0x87000000, 16)
    surface = ispc_texcomp.RGBASurface(padded.tobytes(), bw, bh)
    linear = (ispc_texcomp.compress_blocks_bc1(surface) if opaque
              else ispc_texcomp.compress_blocks_bc3(surface))
    # ispc_texcomp_py 1.0.1 uses py_compress<CompressBlocksBC1, 1>:
    # it allocates w*h bytes although BC1 writes w*h/2. Discard that unused
    # allocation tail; only the complete documented BC1 blocks may leave PC.
    expected = bw * bh * bpe // 16
    if opaque and len(linear) == expected * 2:
        linear = linear[:expected]
    assert len(linear) == expected, (name, len(linear), bw, bh)
    payload = gxt.swizzle(linear, bw // 4, bh // 4, bpe)
    assert len(payload) <= 2 * 1024 * 1024, 'selected art exceeded its GPU allocation cap'
    header = (struct.pack('<8I', 0x00545847, 0x10000003, 1, 64, len(payload), 0, 0, 0)
              + struct.pack('<6IHHB3s', 64, len(payload), 0xffffffff, 0, 0xa0000000, fmt, w, h, 1, b'\0' * 3))
    encoded = header + payload
    check, physical, _, _ = gxt.gxt_decode(encoded)
    assert physical == (w, h)
    return encoded, check.crop((0, 0, w, h)), name


def unpack(entry):
    return gxt.unpack(entry)


def current_image(raw):
    version, lw, lh, w, h, codec, count = struct.unpack_from('<7I', raw, 4)
    assert raw[:4] == b'KIMG' and version == 1 and count + 32 == len(raw)
    if codec == 9:
        image, physical, _, _ = gxt.gxt_decode(raw[32:])
        assert physical == (w, h)
        image = image.crop((0, 0, w, h))
    else:
        assert codec in (1, 2, 7), 'do not overwrite an unknown color representation'
        image = Image.open(io.BytesIO(raw[32:])).convert('RGBA')
    assert image.size == (w, h)
    return image, (lw, lh), codec


def error(reference, actual):
    ref, got = np.asarray(reference, dtype=np.float32), np.asarray(actual, dtype=np.float32)
    delta_alpha = got[:, :, 3] - ref[:, :, 3]
    pixels = delta_alpha.size
    # Exact mean of the black/white composite errors, without constructing two
    # complete composites. Float64 reductions keep the quality gate stable.
    if np.all(ref[:, :, 3] == 255) and np.all(got[:, :, 3] == 255):
        delta = ref - got
        mse = float(np.einsum('ijk,ijk->', delta, delta, dtype=np.float64) / (3 * pixels))
    else:
        delta = (ref[:, :, :3] * ref[:, :, 3:4] - got[:, :, :3] * got[:, :, 3:4]) / 255
        mse = float((np.einsum('ijk,ijk->', delta, delta, dtype=np.float64)
                     + np.einsum('ijk,ij->', delta, delta_alpha, dtype=np.float64)
                     + 1.5 * np.einsum('ij,ij->', delta_alpha, delta_alpha, dtype=np.float64)) / (3 * pixels))
    return {'composite_psnr_db': 10 * math.log10(255 ** 2 / max(mse, 1e-9)),
            'alpha_mae': float(np.mean(abs(delta_alpha)))}


def read_entry(stream, row):
    sector, count, _ = row
    stream.seek(sector * SECTOR)
    entry = stream.read(count * SECTOR)
    assert len(entry) == count * SECTOR
    if entry[:4] in (b'NXCX', b'NXGX'):
        entry = entry[:16 + struct.unpack_from('<I', entry, 8)[0]]
    elif entry[:4] == b'KIMG':
        entry = entry[:32 + struct.unpack_from('<I', entry, 28)[0]]
    return entry


def rebuild(assets, staged, output, selection):
    assert not output.exists(), 'output must be a new directory'
    source_manifest = (assets / 'vita-540p-assets.json').read_bytes()
    manifest = json.loads(source_manifest)
    assert manifest['method'] == 'native-540p-encoded-unorm-v2'
    stage = json.loads((staged / 'manifest.json').read_text(encoding='utf-8'))
    assert stage['source_manifest_sha256'] == sha(source_manifest), 'staged against another pack'
    assert stage['selection_sha256'] == sha(selection.read_bytes())
    config = json.loads(selection.read_bytes())
    assert config['render'] == [960, 540] and config['pixel_budget'] <= 960 * 1080
    assert 0 < config['maximum_axis'] <= 2048
    output.mkdir(parents=True)
    reports = []
    for archive in ARCHIVES:
        entries = [row for row in stage['images'] if row['archive'] == archive]
        hed = (assets / (archive + '.hed')).read_bytes()
        all_rows = list(struct.iter_unpack('<IHH', hed))
        count = next(i for i, row in enumerate(all_rows) if row[0] == 0xffffffff)
        names = (assets / (archive + '.nam')).read_bytes()
        replacements = {}
        with (assets / (archive + '.mrg')).open('rb') as old:
            for row in entries:
                index, name = row['index'], row['name']
                assert index < count and names[index * 32:(index + 1) * 32].split(b'\0')[0].split(b'\r')[0].decode() == name
                stem = name.split('.')[0]
                assert any(stem == s or (config['include_variants'] and stem.startswith(s + '_')) for s in config['stems'])
                encoded_old = read_entry(old, all_rows[index])
                raw = unpack(encoded_old)
                assert sha(raw) == row['old_raw_sha256'], 'target entry changed after source staging'
                before, logical, codec = current_image(raw)
                original_path = (staged / row['file']).resolve()
                assert original_path.is_relative_to(staged.resolve())
                original_bytes = original_path.read_bytes()
                assert sha(original_bytes) == row['sha256'], 'original input fingerprint mismatch'
                original = decode(original_bytes)
                assert list(original.size) == row['original_size']
                reference_old = original.resize(before.size, Image.Resampling.LANCZOS)
                identity = error(reference_old, before)
                assert identity['composite_psnr_db'] > 25 and identity['alpha_mae'] < 6, (name, identity)
                size = target_size(original.size, config['pixel_budget'], config['maximum_axis'], logical)
                target = original.resize(size, Image.Resampling.LANCZOS)
                encoded, after, fmt = encode(target)
                raw_new = b'KIMG' + struct.pack('<7I', 1, *logical, *size, 9, len(encoded)) + encoded
                compressed = zlib.compress(raw_new, 1)
                replacements[index] = (b'NXCX' + struct.pack('<III', len(raw_new), len(compressed), 0) + compressed,
                                       (len(raw_new) + SECTOR - 1) // SECTOR)
                report = {**row, 'old_physical': list(before.size), 'physical': list(size), 'logical': list(logical),
                          'old_codec': codec, 'codec': 9, 'format': fmt, 'source_identity': identity,
                          'old_at_new_size': error(target, before.resize(size, Image.Resampling.BILINEAR)),
                          'new': error(target, after), 'raw_sha256': sha(raw_new),
                          'gpu_payload_bytes': len(encoded) - 64, 'payload_bytes': len(replacements[index][0])}
                report['comparison_improved'] = report['new']['composite_psnr_db'] > report['old_at_new_size']['composite_psnr_db']
                assert report['comparison_improved'], (name, report['new'], report['old_at_new_size'])
                export = output / 'images' / archive / f'{index:05d}-{stem}.png'
                export.parent.mkdir(parents=True, exist_ok=True)
                after.save(export, compress_level=6)
                report['export'] = export.relative_to(output).as_posix()
                reports.append(report)
                print(f"{archive}:{index}:{name} {before.size}->{size} {fmt} "
                      f"PSNR {report['old_at_new_size']['composite_psnr_db']:.2f}->{report['new']['composite_psnr_db']:.2f}", flush=True)
        # Preserve every unselected sector byte and every existing shared locator.
        relocated, new_hed = {}, bytearray()
        with (assets / (archive + '.mrg')).open('rb') as old, (output / (archive + '.mrg')).open('wb') as new:
            for index, row in enumerate(all_rows[:count]):
                key = ('old', *row) if index not in replacements else ('new', index)
                if key in relocated:
                    new_row = relocated[key]
                else:
                    if index in replacements:
                        payload, unpacked = replacements[index]
                        padded = payload + bytes((-len(payload)) % SECTOR)
                    else:
                        old.seek(row[0] * SECTOR)
                        padded, unpacked = old.read(row[1] * SECTOR), row[2]
                        assert len(padded) == row[1] * SECTOR
                    new_row = (new.tell() // SECTOR, len(padded) // SECTOR, unpacked)
                    new.write(padded)
                    relocated[key] = new_row
                new_hed += struct.pack('<IHH', *new_row)
        new_hed += hed[count * 8:]
        (output / (archive + '.hed')).write_bytes(new_hed)
        shutil.copyfile(assets / (archive + '.nam'), output / (archive + '.nam'))
        archive_report = next(r for r in manifest['archives'] if r['name'] == archive)
        delta = sum(r['physical'][0] * r['physical'][1] - r['old_physical'][0] * r['old_physical'][1]
                    for r in reports if r['archive'] == archive)
        archive_report['pixels'] += delta
        archive_report['bytes'] = (output / (archive + '.mrg')).stat().st_size
        manifest['statistics']['pixels'] += delta
    for suffix in ('hed', 'mrg', 'nam'):
        for archive in ARCHIVES:
            name = archive + '.' + suffix
            row = next(f for f in manifest['files'] if f['path'] == name)
            row.update(bytes=(output / name).stat().st_size, sha256=file_sha(output / name))
    manifest['statistics']['bytes'] = sum(f['bytes'] for f in manifest['files'])
    manifest['verification']['resampled'] = True
    manifest['scene_quality_refresh'] = {
        'schema': 'knife-540p-scene-quality-v1', 'render': [960, 540],
        'original_source_recovered': True, 'scope': 'selected entries only; remaining migration records unchanged',
        'source_manifest_sha256': sha(source_manifest), 'selection_sha256': sha(selection.read_bytes()),
        'encoder': 'ispc_texcomp 1.0.1 BC1/BC3, one encoding, native encoded UNORM',
        'pixel_budget_per_image': config['pixel_budget'], 'gpu_payload_cap': 2 * 1024 * 1024,
        'logical_coordinates_preserved': True, 'runtime_hd_fallback': False,
        'images': len(reports),
        'entries': [{k: r[k] for k in ('archive', 'index', 'name', 'logical', 'old_physical', 'physical', 'format', 'gpu_payload_bytes')}
                    for r in reports]}
    json_write(output / 'vita-540p-assets.json', manifest)
    json_write(output / 'quality-report.json', {'source_manifest_sha256': sha(source_manifest), 'images': reports})
    return reports


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', type=Path, required=True)
    parser.add_argument('--original-images', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--selection', type=Path, default=Path(__file__).with_name('scenes.json'))
    args = parser.parse_args()
    print('Completed images:', len(rebuild(args.assets, args.original_images, args.output, args.selection)))

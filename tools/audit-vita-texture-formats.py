#!/usr/bin/env python3
"""Read-only alpha/palette audit of editable images and their runtime formats.

Never quantizes, rewrites, or copies game images. RGBA tuples, including alpha,
must fit the palette for a pixel-exact candidate. GPU savings are hypothetical
payload estimates, not measurements of the current allocator or whole scenes.
"""
from __future__ import annotations

import argparse
import collections
import concurrent.futures
import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import struct
import time
import zlib

from PIL import Image
import vita_image_metadata as metadata


def sha(data):
    return hashlib.sha256(data).hexdigest()


def runtime_index(assets, archives):
    result = {}
    for archive in archives:
        cached = {}
        with (assets / (archive + '.mrg')).open('rb') as source:
            extent = source.seek(0, 2)
            for index, (sector, count, _) in enumerate(struct.iter_unpack(
                    '<IHH', (assets / (archive + '.hed')).read_bytes())):
                if sector == 0xffffffff:
                    break
                if not count:
                    continue
                key = (sector, count)
                if key not in cached:
                    assert (sector + count) * 2048 <= extent
                    source.seek(sector * 2048)
                    raw = source.read(count * 2048)
                    if raw[:4] in (b'NXCX', b'NXGX'):
                        length, compressed = struct.unpack_from('<II', raw, 4)
                        assert 0 < length <= metadata.CAP and compressed + 16 <= len(raw)
                        decoder = zlib.decompressobj(31 if raw[:4] == b'NXGX' else 15)
                        raw = decoder.decompress(raw[16:16 + compressed], length + 1)
                        assert decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail
                        assert len(raw) == length
                    cached[key] = metadata.images_in(raw)
                result[(archive, index)] = (key, cached[key])
        print(f'runtime metadata: {archive}', flush=True)
    return result


def inspect_image(task):
    path, expected_sha, record = task
    encoded = path.read_bytes()
    assert sha(encoded) == expected_sha, f'project image changed: {path}'
    with Image.open(io.BytesIO(encoded)) as original:
        record = dict(record, png_mode=original.mode, physical=list(original.size),
                      file_bytes=len(encoded), sha256=expected_sha)
        rgba = original.convert('RGBA')
    alpha = rgba.getchannel('A')
    histogram = alpha.histogram()
    pixels = rgba.width * rgba.height
    if histogram[255] == pixels:
        kind = 'opaque'
    elif histogram[0] == pixels:
        kind = 'fully_transparent'
    elif histogram[0] + histogram[255] == pixels:
        kind = 'binary_alpha'
    else:
        kind = 'partial_alpha'
    colors = rgba.getcolors(256)
    exact = len(colors) if colors is not None else None
    # Report separately: preserving visible appearance permits discarding RGB
    # below alpha zero, but this is not a pixel-identical representation.
    visible = exact
    if exact is None and histogram[0]:
        rgba.paste((0, 0, 0, 0), mask=alpha.point([255] + [0] * 255))
        colors = rgba.getcolors(256)
        visible = len(colors) if colors is not None else None
    record.update(alpha=kind, alpha_values=sum(bool(v) for v in histogram),
                  rgba_colors_le256=exact, visible_colors_le256=visible,
                  rgba_pixel_bytes=pixels * 4)
    return record


def inspect_unexported(assets, missing):
    """Check archive-only members too, without exporting any new image files."""
    spec = importlib.util.spec_from_file_location('audit_native_gxt',
        Path(__file__).with_name('migrate-vita-native-color.py'))
    gxt = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gxt)
    results = []
    for row in missing:
        archive, sector, count, ordinal = row['runtime_owner']
        with (assets / (archive + '.mrg')).open('rb') as source:
            source.seek(sector * 2048)
            raw = source.read(count * 2048)
        if raw[:4] in (b'NXCX', b'NXGX'):
            raw = raw[:16 + struct.unpack_from('<I', raw, 8)[0]]
        raw = gxt.unpack(raw)
        if raw[:4] == b'KATL':
            p = 12
            for i in range(ordinal + 1):
                nl, il = struct.unpack_from('<II', raw, p)
                p += 8 + nl
                image = raw[p:p + il]
                p += il
        else:
            assert ordinal == 0
            image = raw
        info = metadata.image_info(image)
        assert info == row['runtime']
        payload = image[32:info['envelope_bytes']]
        if info['codec'] in (8, 9):
            pixels, physical, _, _ = gxt.gxt_decode(payload)
            pixels = pixels.crop((0, 0, *physical))
        else:
            pixels = Image.open(io.BytesIO(payload)).convert('RGBA')
        assert list(pixels.size) == info['physical']
        results.append(dict(row, alpha_extrema=list(pixels.getchannel('A').getextrema()),
            rgba_colors_le256=(len(colors) if (colors := pixels.getcolors(256)) is not None else None),
            kimg_sha256=sha(image[:info['envelope_bytes']])))
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', type=Path, required=True)
    parser.add_argument('--project', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--jobs', type=int, default=4)
    args = parser.parse_args()
    assert 1 <= args.jobs <= 16
    assets, project = args.assets.resolve(), args.project.resolve()
    output = args.output.resolve()
    assert not output.is_relative_to(assets) and not output.is_relative_to(project)
    started = time.monotonic()
    manifests = {p.parent.name: json.loads(p.read_bytes())
                 for p in sorted(project.glob('*/manifest.json'))}
    runtime = runtime_index(assets, manifests)
    tasks, unmatched, skipped = [], [], collections.Counter()
    seen = set()
    for archive, manifest in manifests.items():
        for entry in manifest['entries']:
            index = entry['index']
            locator, members = runtime.get((archive, index), (None, []))
            for file in entry.get('files', []):
                rel = Path(file['path'])
                if rel.suffix.lower() not in ('.png', '.jpg', '.jpeg'):
                    skipped[rel.suffix.lower()] += 1
                    continue
                path = (project / archive / rel).resolve()
                assert path.is_relative_to(project / archive)
                identity = path.relative_to(project).as_posix()
                assert identity not in seen, f'duplicate image path: {identity}'
                seen.add(identity)
                info, member_index = None, None
                if len(members) == 1:
                    member_index, info = 0, members[0][1]
                else:
                    stem = Path(entry['name']).stem
                    # The editable export replaces unsafe/legacy characters
                    # and numbers collisions. Names alone are not identity:
                    # original atlases also contain repeated texture names.
                    aliases, occurrences = [], collections.Counter()
                    for ordinal, (name, value) in enumerate(members):
                        safe = ''.join(c if c.isascii() and (c.isalnum() or c in '_.-')
                                       else '_' for c in name)
                        base = stem + '__' + safe
                        occurrences[base] += 1
                        alias = base + (f'__{occurrences[base]}' if occurrences[base] > 1 else '')
                        aliases.append((ordinal, alias, value))
                    matches = [(ordinal, value) for ordinal, alias, value in aliases if rel.stem == alias]
                    if len(matches) == 1:
                        member_index, info = matches[0]
                record = dict(path=identity, archive=archive, index=index, entry=entry['name'])
                if info is not None:
                    record.update(runtime=info, runtime_member_index=member_index,
                                  runtime_owner=[archive, *locator, member_index])
                else:
                    unmatched.append(identity)
                tasks.append((path, file['sha256'], record))
    records = []
    # Bound queued work as well as decoded image buffers.
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for start in range(0, len(tasks), 128):
            records.extend(pool.map(inspect_image, tasks[start:start + 128]))
            if start % 1024 == 0:
                print(f'alpha/palette: {len(records)}/{len(tasks)}', flush=True)
    categories = collections.defaultdict(collections.Counter)
    modes, formats, opaque_dxt5 = collections.Counter(), collections.Counter(), []
    exact_candidates, normalized_candidates = [], []
    unique = {}
    for row in records:
        cat = categories[row['alpha']]
        cat['images'] += 1
        modes[row['png_mode']] += 1
        exact, visible = row['rgba_colors_le256'], row['visible_colors_le256']
        if exact is not None:
            cat['exact_p8'] += 1
            cat['exact_p4'] += exact <= 16
            cat['rgba_pixel_bytes_of_exact_p8'] += row['rgba_pixel_bytes']
            exact_candidates.append(row['path'])
        elif visible is not None:
            cat['p8_after_zero_alpha_rgb_normalization'] += 1
            normalized_candidates.append(row['path'])
        info = row.get('runtime')
        if info is None:
            continue
        formats[info['format']] += 1
        shape_matches = info['physical'] == row['physical']
        row['runtime_shape_matches_export'] = shape_matches
        if row['alpha'] == 'opaque' and info['format'] == 'dxt5' and shape_matches:
            opaque_dxt5.append(row['path'])
        unique.setdefault(tuple(row['runtime_owner']), row)
    unique_formats = collections.Counter(row['runtime']['format'] for row in unique.values())
    missing = {}
    for (archive, index), (locator, members) in runtime.items():
        for ordinal, (name, info) in enumerate(members):
            owner = (archive, *locator, ordinal)
            if owner not in unique:
                missing.setdefault(owner, dict(archive=archive, index=index, member_index=ordinal,
                    name=name, runtime=info, runtime_owner=list(owner)))
    unexported = inspect_unexported(assets, list(missing.values()))
    eligible = [r for r in unique.values() if r['rgba_colors_le256'] is not None
                and r['runtime_shape_matches_export']]
    eligible_formats = collections.Counter(r['runtime']['format'] for r in eligible)
    candidates = []
    for row in eligible:
        info = row['runtime']
        w, h = row['physical']
        bw, bh = max(4, 1 << (w - 1).bit_length()), max(4, 1 << (h - 1).bit_length())
        p8 = bw * bh + 1024
        p4 = bw * bh // 2 + 64 if row['rgba_colors_le256'] <= 16 else None
        best = min(p8, p4) if p4 is not None else p8
        candidates.append(dict(path=row['path'], alpha=row['alpha'],
            colors=row['rgba_colors_le256'], current_format=info['format'],
            physical=[w, h], current_payload_bytes=info['gpu_payload_bytes'],
            hypothetical_p8_pot_bytes=p8, hypothetical_p4_pot_bytes=p4,
            possible_payload_saving_bytes=max(0, info['gpu_payload_bytes'] - best)))
    candidates.sort(key=lambda x: x['possible_payload_saving_bytes'], reverse=True)
    summary = dict(schema='knife-alpha-palette-audit-v1', assets_manifest_sha256=sha(
        (assets / 'vita-540p-assets.json').read_bytes()),
        project_manifest_sha256=sha((project / 'manifest.json').read_bytes()),
        scanned_images=len(records), image_hashes_verified=True, skipped_nonimages=dict(skipped),
        categories=dict(categories), modes=dict(modes), runtime_formats_by_export=dict(formats),
        runtime_unique_owners_with_exports=len(unique), runtime_unique_formats=dict(unique_formats),
        runtime_owners_without_export=unexported,
        runtime_unique_owners_checked=len(unique) + len(unexported),
        unmatched_exports=unmatched,
        runtime_shape_mismatches=[r['path'] for r in records
                                 if r.get('runtime_shape_matches_export') is False],
        opaque_dxt5=opaque_dxt5,
        unexported_opaque_dxt5=[row for row in unexported
                              if row['runtime']['format'] == 'dxt5' and row['alpha_extrema'] == [255, 255]],
        exact_p8_images=len(exact_candidates),
        additional_p8_after_zero_alpha_rgb_normalization=len(normalized_candidates),
        exact_p8_unique_runtime_owners=len(eligible),
        exact_p8_unique_runtime_formats=dict(eligible_formats),
        hypothetical_payload_saving_bytes=sum(c['possible_payload_saving_bytes'] for c in candidates),
        candidate_payload_estimates=candidates,
        estimate_limits='PoT backing, no mipmaps; palette stored separately, excluded allocator rounding; '
            'CPU grades/strip/raw and render targets still require runtime implementation changes; '
            'estimates summed across the library are not concurrent GPU residency or measured savings.',
        pixel_exact_rule='At most 256 distinct RGBA tuples; P4 at most 16. Alpha is counted. '
            'Zero-alpha RGB normalization is reported separately and is not pixel identical.',
        elapsed_seconds=round(time.monotonic() - started, 2))
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    detail = output.with_suffix('.records.json.gz')
    with gzip.open(detail, 'wt', encoding='utf-8') as stream:
        json.dump(records, stream, ensure_ascii=False, separators=(',', ':'))
    print(json.dumps({k: v for k, v in summary.items()
                      if k not in ('candidate_payload_estimates', 'unmatched_exports',
                                   'runtime_shape_mismatches', 'opaque_dxt5')},
                     ensure_ascii=False, indent=2), flush=True)
    print(f'report={output}\nrecords={detail}', flush=True)


if __name__ == '__main__':
    main()

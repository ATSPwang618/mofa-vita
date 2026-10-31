"""Audit physical image dimensions, every decoded Ogg, MP4 frames and retained data."""
from __future__ import annotations

import argparse
import collections
from concurrent.futures import ProcessPoolExecutor, as_completed
import csv
import hashlib
import io
import json
import mmap
from tool_paths import tool_path
from pathlib import Path
import struct
import subprocess
import time
import zlib

from PIL import Image

from compact_media import compact_image, nopus_packets, opus_samples
from create_compact import PROFILE
from create_540p import digest, write_json

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-x64/assets'
OUTPUT = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64'
ARCHIVES = [('allpachdml', 4), ('allpacml', 4), ('jallpachd', 1), ('jallpac', 1), ('allui', 1), ('parts', 1)]
SECTOR = 2048


def read_json(path: Path):
    return json.loads(path.read_text(encoding='utf-8'))


def rows(folder: Path, name: str) -> list[tuple[int, int, int]]:
    data = (folder / f'{name}.hed').read_bytes()
    assert len(data) % 8 == 0 and data[-8:] == b'\xff' * 8, f'{name}: HED terminator'
    result = list(struct.iter_unpack('<IHH', data))
    first = next(i for i, row in enumerate(result) if row[0] == 0xffffffff)
    assert all(row == (0xffffffff, 65535, 65535) for row in result[first:])
    return result[:first]


def entry(mm: mmap.mmap, row: tuple[int, int, int]) -> bytes:
    start, length = row[0] * SECTOR, row[1] * SECTOR
    assert start + length <= len(mm), 'MRG range exceeds file'
    if mm[start:start + 4] in (b'NXCX', b'NXGX'):
        length = 16 + struct.unpack_from('<I', mm, start + 8)[0]
        assert length <= row[1] * SECTOR, 'NX payload exceeds its own sector range'
    return mm[start:start + length]


def unpack(data: bytes, *, strict: bool = False) -> bytes:
    if data[:4] not in (b'NXCX', b'NXGX'):
        return data
    declared, length = struct.unpack_from('<II', data, 4)
    assert len(data) == 16 + length and declared <= 128 * 1024 * 1024
    decoder = zlib.decompressobj(31 if data[:4] == b'NXGX' else 15)
    value = decoder.decompress(data[16:], 128 * 1024 * 1024 + 1)
    assert decoder.eof and not decoder.unconsumed_tail
    # Original NX may include a rounded declared length. Converted NX must be exact.
    if strict:
        assert not decoder.unused_data
        assert len(value) == declared
    else:
        # Original allui entry 2 includes 25,538 zero bytes after its zlib end.
        assert not decoder.unused_data or set(decoder.unused_data) == {0}
    return value


def bntx_textures(data: bytes) -> list[tuple[str, int, int, bool]]:
    assert data[:4] == b'BNTX' and len(data) >= 48
    count, table = struct.unpack_from('<IQ', data, 36)
    assert 0 < count <= 4096 and table + count * 8 <= len(data)
    result = []
    for index in range(count):
        offset = struct.unpack_from('<Q', data, table + index * 8)[0]
        assert data[offset:offset + 4] == b'BRTI' and offset + 104 <= len(data)
        fmt = struct.unpack_from('<I', data, offset + 28)[0]
        width, height = struct.unpack_from('<II', data, offset + 36)
        label = struct.unpack_from('<Q', data, offset + 96)[0]
        length = struct.unpack_from('<H', data, label)[0]
        assert label + 2 + length <= len(data) and width > 0 and height > 0
        name = data[label + 2:label + 2 + length].decode('utf-8', errors='replace')
        result.append((name, width, height, fmt & 255 == 6))
    return result


def kimg_header(data: bytes) -> tuple[tuple[int, int], tuple[int, int], bool, bytes]:
    magic, version, lw, lh, width, height, encoding, length = struct.unpack_from('<4s7I', data)
    assert (magic, version) == (b'KIMG', 1) and encoding in (1, 2, 3, 4, 7)
    assert 0 < width <= lw <= 32768 and 0 < height <= lh <= 32768
    assert 0 < length and len(data) == 32 + length
    return (lw, lh), (width, height), encoding not in (4, 7), data[32:]


def expected_size(data: bytes) -> tuple[tuple[int, int], tuple[int, int], bool]:
    if data[:4] == b'KIMG':
        logical, size, srgb, _ = kimg_header(data)
        return logical, size, srgb
    if data[:4] == b'BNTX':
        textures = bntx_textures(data)
        assert len(textures) == 1
        _, width, height, srgb = textures[0]
        return (width, height), (width, height), srgb
    with Image.open(io.BytesIO(data)) as image:
        return image.size, image.size, True


def check_image(data: bytes, source: tuple[tuple[int, int], tuple[int, int], bool], strips: int,
                manifest: dict, dimensions: collections.Counter, original: Image.Image,
                image_source: Path, formats: collections.Counter) -> None:
    logical, size, srgb, payload = kimg_header(data)
    encoding = struct.unpack_from('<I', data, 24)[0]
    assert encoding in (1, 2, 7), 'Converted pixels must use PNG or JPEG'
    expected_format = 'JPEG' if encoding == 1 else 'PNG'
    assert manifest['format'] == expected_format and manifest['encoding'] == encoding
    expected_logical, previous, expected_srgb = source
    assert logical == expected_logical and srgb == expected_srgb
    assert previous[1] % strips == 0 and size[1] % strips == 0
    factor = min(1.0, 960 / previous[0], 540 / (previous[1] // strips))
    expected = (max(1, round(previous[0] * factor)), max(1, round(previous[1] // strips * factor)) * strips)
    assert size == expected, (size, expected)
    assert size[0] <= 960 and size[1] // strips <= 540
    assert tuple(manifest['logical']) == logical and tuple(manifest['physical']) == size
    assert tuple(manifest['before']) == previous and manifest['strips'] == strips
    assert manifest['payload_bytes'] == len(payload)
    expected_pixels = Image.new('RGBA', size)
    original = original.convert('RGBA')
    height = original.height // strips
    for k in range(strips):
        part = original.crop((0, k * height, original.width, (k + 1) * height))
        if part.size != (size[0], size[1] // strips):
            part = part.resize((size[0], size[1] // strips), Image.Resampling.LANCZOS)
        expected_pixels.paste(part, (0, k * (size[1] // strips)))
    has_alpha = expected_pixels.getchannel('A').getextrema() != (255, 255)
    assert manifest['has_alpha'] == has_alpha
    assert expected_format == ('PNG' if has_alpha or manifest['force_png'] or not srgb or strips > 1 else 'JPEG')
    # Decode the complete bitstream, rather than trusting its container header.
    with Image.open(io.BytesIO(payload)) as image:
        assert image.format == expected_format and image.size == size
        image.load()
        actual = image.convert('RGBA')
        if expected_format == 'PNG':
            assert actual.tobytes() == expected_pixels.tobytes(), 'Lossless PNG pixels changed'
        else:
            assert image.mode == 'RGB' and not image.info.get('progressive') and not image.info.get('progression')
            assert not has_alpha and actual.getchannel('A').getextrema() == (255, 255)
        assert len(manifest['source_files']) == strips
        for k, record in enumerate(manifest['source_files']):
            path = image_source / record['path']
            assert path.resolve().is_relative_to(image_source.resolve())
            assert path.suffix == ('.jpg' if expected_format == 'JPEG' else '.png')
            assert path.stat().st_size == record['bytes'] and digest(path) == record['sha256']
            with Image.open(path) as exported:
                assert exported.format == expected_format and exported.size == (size[0], size[1] // strips)
                assert exported.width <= 960 and exported.height <= 540
                assert list(exported.size) == record['physical']
                exported.load()
                if strips == 1:
                    assert path.read_bytes() == payload, 'Editable image differs from runtime payload'
                else:
                    assert exported.convert('RGBA').tobytes() == actual.crop((0, k * exported.height, exported.width, (k + 1) * exported.height)).tobytes()
    formats[expected_format] += strips
    dimensions[f'{size[0]}x{size[1] // strips}'] += strips


def check_atlas(data: bytes, original: bytes, record: dict, dimensions: collections.Counter,
                cache: Path, image_source: Path, formats: collections.Counter) -> int:
    assert data[:4] == b'KATL'
    version, count = struct.unpack_from('<II', data, 4)
    expected = bntx_textures(original)
    assert version == 1 and count == len(expected) == len(record['textures'])
    position = 12
    for k, (source, report) in enumerate(zip(expected, record['textures'])):
        name_length, image_length = struct.unpack_from('<II', data, position)
        position += 8
        assert 0 < name_length <= 1024 and position + name_length + image_length <= len(data)
        label = data[position:position + name_length].decode('utf-8')
        position += name_length
        assert label == source[0] == report['name'], 'Atlas names / order changed'
        assert report['force_png'] and report['format'] == 'PNG'
        with Image.open(cache / f'{record["index"]}_{k}.png') as pixels:
            check_image(data[position:position + image_length], ((source[1], source[2]), (source[1], source[2]), source[3]),
                        1, report, dimensions, pixels, image_source, formats)
        position += image_length
    assert position == len(data), 'Trailing atlas bytes'
    return count


def check_archive(source: Path, assets: Path, image_source: Path, name: str, strips: int) -> dict:
    report = read_json(assets / f'{name}.compact.json')
    assert report['source_sha256'] == digest(source / f'{name}.mrg')
    assert report['sha256'] == digest(assets / f'{name}.mrg')
    assert report['profile'] == PROFILE
    assert read_json(image_source / name / 'manifest.json') == report
    cache = ROOT / '.work/portable-bntx' / f'{name}-{report["source_sha256"][:16]}'
    before, after = rows(source, name), rows(assets, name)
    assert len(before) == len(after) == len(report['records']) == report['entries']
    names = (source / f'{name}.nam').read_bytes() if (source / f'{name}.nam').exists() else None
    if names is not None:
        assert names == (assets / f'{name}.nam').read_bytes() and len(names) >= len(after) * 32
    kinds, sizes, formats = collections.Counter(), collections.Counter(), collections.Counter()
    count = atlas_count = 0
    with (source / f'{name}.mrg').open('rb') as a, (assets / f'{name}.mrg').open('rb') as b, \
            mmap.mmap(a.fileno(), 0, access=mmap.ACCESS_READ) as old, mmap.mmap(b.fileno(), 0, access=mmap.ACCESS_READ) as new:
        cursor = 0
        for index, (old_row, new_row, record) in enumerate(zip(before, after, report['records'])):
            label = names[index * 32:(index + 1) * 32].split(b'\0')[0].split(b'\r')[0].decode('utf-8') if names else f'#{index}'
            assert record['index'] == index and record['name'] == label
            assert new_row[0] * SECTOR == cursor, f'{name}/{index}: archive gap or overlap'
            original, output = entry(old, old_row), entry(new, new_row)
            assert len(output) == record['stored_bytes']
            padding = new_row[1] * SECTOR - len(output)
            assert 0 <= padding < SECTOR and new[cursor + len(output):cursor + new_row[1] * SECTOR] == bytes(padding)
            cursor += new_row[1] * SECTOR
            data, previous = unpack(output, strict=record['kind'].startswith('portable')), unpack(original)
            assert new_row[2] == (len(data) + SECTOR - 1) // SECTOR
            kind = record['kind']
            kinds[kind] += 1
            if kind == 'portable-image':
                # Only KIMG replacements in *ml contain four stacked languages;
                # small original BNTX entries there are individual textures.
                views = strips if previous.startswith(b'KIMG') else 1
                assert record['force_png'] == (name == 'allui' or label.upper().startswith('RULE_'))
                if previous.startswith(b'KIMG'):
                    pixels, _, _ = compact_image(previous)
                elif previous.startswith(b'BNTX'):
                    pixels = Image.open(cache / f'{index}_0.png')
                else:
                    pixels = Image.open(io.BytesIO(previous))
                with pixels:
                    check_image(data, expected_size(previous), views, record, sizes, pixels, image_source, formats)
                count += 1
            elif kind == 'portable-atlas':
                count += check_atlas(data, previous, record, sizes, cache, image_source, formats)
                atlas_count += 1
            else:
                assert kind in ('retained-data', 'empty'), 'Every image must use the portable PNG/JPEG profile'
                assert output == original, f'{name}/{index}: preserved bytes changed'
                if kind in ('retained-bntx', 'retained-lut'):
                    textures = bntx_textures(data)
                    for _, width, height, _ in textures:
                        assert width <= 960 and height <= 540, f'{name}/{index}: large retained texture'
                        sizes[f'{width}x{height}'] += 1
                    count += len(textures)
                elif kind == 'retained-image':
                    with Image.open(io.BytesIO(data)) as image:
                        assert image.width <= 960 and image.height <= 540
                        image.load()
                        sizes[f'{image.width}x{image.height}'] += 1
                    count += 1
                else:
                    assert kind in ('retained-data', 'empty')
                    assert not data.startswith((b'KIMG', b'KATL', b'BNTX', b'\x89PNG', b'\xff\xd8'))
            if index and index % 2000 == 0:
                print(f'Images: {name} {index}/{len(after)} decoded and checked', flush=True)
        assert cursor == len(new), f'{name}: unindexed trailing data'
    result = dict(name=name, entries=len(after), kinds=dict(kinds), image_objects=count,
                  atlas_objects=atlas_count, image_views_including_languages=sum(sizes.values()),
                  largest_width=max(int(s.split('x')[0]) for s in sizes),
                  largest_height=max(int(s.split('x')[1]) for s in sizes), common_physical_sizes=dict(sizes.most_common(12)),
                  source_bytes=report['source_bytes'], output_bytes=report['output_bytes'], sha256=report['sha256'],
                  formats=dict(formats), all_png_and_jpeg_fully_decoded=True,
                  no_webp_or_bntx_images_remaining=True, all_png_pixels_match_resized_source=True,
                  all_jpeg_baseline_and_opaque=True, all_editable_image_files_verified=True,
                  all_language_views_at_most_960x540=True,
                  atlas_names_and_original_layout_preserved=True, retained_payloads_byte_identical=True)
    print(f'Images: {name}: {count} objects, all dimensions within the limit', flush=True)
    return result


def check_audio(source: Path, output: Path, decoded: Path) -> dict:
    records = read_json(output / 'compact-audio.json')
    with decoded.open(encoding='utf-8', newline='') as stream:
        rows = list(csv.DictReader(stream, delimiter='\t'))
    observed = {row['name']: (int(row['samples']), int(row['channels'])) for row in rows}
    assert len(rows) == len(observed) == len(records)
    expected_names = {p.relative_to(source).as_posix() for p in source.rglob('*.opus')}
    assert {record['original'] for record in records} == expected_names
    assert not list((output / 'assets').rglob('*.opus'))
    samples = 0
    channels_count = collections.Counter()
    for record in records:
        original, final = source / record['original'], output / 'assets' / record['path']
        channels, skip, packets = nopus_packets(original.read_bytes())
        expected_samples = sum(opus_samples(packet) for packet in packets) - skip
        assert (record['samples'], record['channels'], record['sample_rate']) == (expected_samples, channels, 48000)
        assert observed[final.name] == (expected_samples, channels), final.name
        assert final.stat().st_size == record['output_bytes'] and original.stat().st_size == record['input_bytes']
        assert final.read_bytes()[:4] == b'OggS' and digest(final) == record['sha256']
        samples += expected_samples
        channels_count[channels] += 1
    result = dict(files=len(records), codec='Ogg Opus', rate=48000, channels=dict(channels_count),
                  decoded_samples_per_channel=samples, source_bytes=sum(r['input_bytes'] for r in records),
                  output_bytes=sum(r['output_bytes'] for r in records), all_fully_decoded_by_native_kdu=True,
                  all_original_sample_counts_and_channels_matched=True, all_file_sha256_matched=True,
                  decoded_audit_sha256=digest(decoded))
    print(f'Audio: {len(records)} complete decodes and exact sample counts checked', flush=True)
    return result


def check_movies(source: Path, output: Path, ffmpeg: Path) -> list[dict]:
    ffprobe = ffmpeg.with_name('ffprobe.exe' if ffmpeg.suffix.lower() == '.exe' else 'ffprobe')
    results = []
    records = read_json(output / 'compact-movies.json')
    assert {r['path'] for r in records} == {p.relative_to(source).as_posix() for p in source.rglob('*.mp4')}
    def probe(path):
        return json.loads(subprocess.check_output([str(ffprobe), '-v', 'error', '-show_streams', '-show_format', '-of', 'json', str(path)]))
    for record in records:
        src, final = source / record['path'], output / 'assets' / record['path']
        assert digest(final) == record['sha256'] and final.stat().st_size == record['output_bytes']
        before, after = probe(src), probe(final)
        previous = next(s for s in before['streams'] if s['codec_type'] == 'video')
        video = next(s for s in after['streams'] if s['codec_type'] == 'video')
        assert (video['codec_name'], video['width'], video['height'], video['pix_fmt']) == ('h264', 960, 540, 'yuv420p')
        assert video['nb_frames'] == previous['nb_frames'] and video['avg_frame_rate'] == previous['avg_frame_rate']
        assert abs(float(video['duration']) - float(previous['duration'])) < 0.05
        old_audio = [s for s in before['streams'] if s['codec_type'] == 'audio']
        new_audio = [s for s in after['streams'] if s['codec_type'] == 'audio']
        assert len(old_audio) == len(new_audio)
        audio_checks = []
        for audio_index, (a, b) in enumerate(zip(old_audio, new_audio)):
            assert all(a[k] == b[k] for k in ('codec_name', 'sample_rate', 'channels'))
            def audio_packets(path):
                info = json.loads(subprocess.check_output([str(ffprobe), '-v', 'error', '-select_streams', f'a:{audio_index}',
                                  '-show_packets', '-show_data_hash', 'sha256', '-of', 'json', str(path)]))
                return [{k: packet.get(k) for k in ('pts_time', 'dts_time', 'duration_time', 'size', 'data_hash')}
                        for packet in info['packets']]
            source_packets, final_packets = audio_packets(src), audio_packets(final)
            assert source_packets == final_packets, f'Audio packet data or timestamps changed: {record["path"]}'
            def pcm_identity(path):
                decoded = subprocess.run([str(ffmpeg), '-v', 'error', '-xerror', '-nostdin', '-i', str(path),
                                          '-map', f'0:a:{audio_index}', '-f', 's16le', '-'], capture_output=True, check=True)
                assert not decoded.stderr
                return len(decoded.stdout), hashlib.sha256(decoded.stdout).hexdigest()
            original_pcm, final_pcm = pcm_identity(src), pcm_identity(final)
            assert original_pcm == final_pcm, f'Decoded movie audio changed: {record["path"]}'
            audio_checks.append(dict(packet_count=len(final_packets),
                                     source_declared_frames=a.get('nb_frames'), output_declared_frames=b.get('nb_frames'),
                                     decoded_samples_per_channel=final_pcm[0] // (2 * int(b['channels'])),
                                     pcm_s16le_sha256=final_pcm[1], original_packets_timestamps_and_pcm_preserved=True))
        proc = subprocess.run([str(ffmpeg), '-v', 'error', '-xerror', '-nostdin', '-threads', '1', '-i', str(final),
                               '-f', 'null', '-'], capture_output=True)
        assert proc.returncode == 0 and not proc.stderr, proc.stderr.decode(errors='replace')
        results.append(dict(path=record['path'], source_bytes=src.stat().st_size, output_bytes=final.stat().st_size,
                            sha256=record['sha256'], codec=video['codec_name'], physical=[960, 540],
                            frames=int(video['nb_frames']), duration=float(video['duration']), full_decode_passed=True,
                            original_frame_count_and_frame_rate_preserved=True,
                            original_audio_packets_and_decoded_samples_preserved=True, audio_checks=audio_checks))
        print(f'Movie: {final.name}: every frame decoded', flush=True)
    return results


def check_retained(source: Path, assets: Path) -> list[dict]:
    replaced = {f'{name}.{ext}' for name, _ in ARCHIVES for ext in ('hed', 'nam', 'mrg', 'compact.tsv')}
    results = []
    for path in source.rglob('*'):
        if not path.is_file():
            continue
        rel = path.relative_to(source)
        if rel.as_posix() in replaced or path.suffix.lower() in ('.mp4', '.opus'):
            continue
        dst = assets / rel
        before = digest(path)
        assert before == digest(dst), f'Preserved file changed: {rel}'
        record = dict(path=rel.as_posix(), bytes=path.stat().st_size, sha256=before)
        if path.suffix.lower() == '.tga':
            with Image.open(dst) as image:
                image.load()
                assert image.width <= 960 and image.height <= 540
                record.update(image_format=image.format, physical=list(image.size), fully_decoded=True)
        results.append(record)
    return results


def size_summary(folder: Path) -> dict:
    counts, sizes = collections.Counter(), collections.Counter()
    for path in folder.rglob('*'):
        if path.is_file():
            counts[path.suffix.lower()] += 1
            sizes[path.suffix.lower()] += path.stat().st_size
    return dict(total_bytes=sum(sizes.values()), files=sum(counts.values()), bytes_by_extension=dict(sizes), counts_by_extension=dict(counts))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=SOURCE)
    parser.add_argument('--out', type=Path, default=OUTPUT)
    parser.add_argument('--decoded-audio', type=Path, default=ROOT / '.work/compact-audio-decoded.tsv')
    parser.add_argument('--image-source', type=Path)
    parser.add_argument('--ffmpeg', type=Path, default=tool_path('ffmpeg', 'KNIFE_FFMPEG'))
    parser.add_argument('--jobs', type=int, default=3, choices=range(1, 9))
    args = parser.parse_args()
    started = time.monotonic()
    report = ROOT / 'doc/compact-assets-validation.json'
    result = dict(source=str(args.source.resolve()), output=str(args.out.resolve()), complete=False)
    write_json(report, result)
    assert read_json(args.out / 'asset-build.json')['complete']
    image_source = (args.image_source or args.out.with_name(args.out.name.removesuffix('-x64') + '-image-source')).resolve()
    assert read_json(image_source / 'manifest.json')['profile'] == PROFILE
    assert not list(image_source.rglob('*.webp'))
    result['image_source'] = str(image_source)
    result['archives'] = []
    with ProcessPoolExecutor(max_workers=args.jobs) as pool:
        checks = {pool.submit(check_archive, args.source, args.out / 'assets', image_source, name, strips): name
                  for name, strips in ARCHIVES}
        completed = {}
        for future in as_completed(checks):
            completed[checks[future]] = future.result()
            result['archives'] = [completed[name] for name, _ in ARCHIVES if name in completed]
            write_json(report, result)
    result['audio'] = check_audio(args.source, args.out, args.decoded_audio)
    write_json(report, result)
    result['movies'] = check_movies(args.source, args.out, args.ffmpeg)
    result['retained_files'] = check_retained(args.source, args.out / 'assets')
    result['before'] = size_summary(args.source)
    result['after'] = size_summary(args.out / 'assets')
    result['complete'] = True
    result['elapsed_seconds'] = round(time.monotonic() - started, 2)
    write_json(report, result)
    print(json.dumps({k: result[k] for k in ('complete', 'before', 'after', 'elapsed_seconds')}, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()

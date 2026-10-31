"""Build 960x540 PNG/JPEG, Ogg Opus and H.264 resources plus editable images."""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import io
import json
import mmap
import os
from pathlib import Path
import shutil
import struct
import subprocess
import time
import zlib

from PIL import Image
from compact_media import encode_portable, nopus_to_ogg, nx_wrap, to_portable
from create_540p import digest, write_json
from verify_540p_assets import stored

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-x64/assets'
OUT = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64'
from tool_paths import tool_path
FFMPEG = tool_path('ffmpeg', 'KNIFE_FFMPEG')
FFPROBE = FFMPEG.with_name('ffprobe.exe')
ARCHIVES = [('allpachdml', 4), ('allpacml', 4), ('jallpachd', 1), ('jallpac', 1), ('allui', 1), ('parts', 1)]
PROFILE = 'png-jpeg-540p-v1'


def prepare_bntx(source: Path, archive: str, source_sha: str) -> Path:
    target = ROOT / '.work/portable-bntx' / f'{archive}-{source_sha[:16]}'
    marker = target / '.complete.json'
    if marker.exists():
        assert json.loads(marker.read_text())['source_sha256'] == source_sha
        return target
    target.mkdir(parents=True, exist_ok=True)
    subprocess.run([str(ROOT / 'tools/kdu-compact.exe'), '--romfs', str(source),
                    'export-bntx', archive, str(target)], check=True)
    write_json(marker, dict(source_sha256=source_sha))
    return target


def export_image(root: Path, archive: str, index: int, texture: int, value: bytes, info: dict) -> None:
    """Export the exact runtime payload; split stacked languages into ordinary images."""
    folder = root / archive
    folder.mkdir(parents=True, exist_ok=True)
    stem = f'{index:05d}_{texture:02d}'
    extension = '.png' if info['format'] == 'PNG' else '.jpg'
    payload = value[32:]
    files = []
    if info['strips'] == 1:
        path = folder / (stem + extension)
        path.write_bytes(payload)
        files.append(dict(path=path.relative_to(root).as_posix(), physical=info['physical'],
                          bytes=len(payload), sha256=digest(path)))
    else:
        with Image.open(io.BytesIO(payload)) as image:
            height = image.height // info['strips']
            for i, language in enumerate(('ja', 'en', 'zc', 'zt')):
                path = folder / (stem + '_' + language + '.png')
                image.crop((0, i * height, image.width, (i + 1) * height)).save(path, 'PNG', compress_level=6)
                files.append(dict(path=path.relative_to(root).as_posix(), language=language,
                                  physical=[image.width, height], bytes=path.stat().st_size, sha256=digest(path)))
    info['source_files'] = files


def unpack(data: bytes) -> bytes:
    # Original Nintendo NX headers sometimes round the declared size up; the zlib
    # stream's checksum and end marker, rather than that padding, delimit its bytes.
    if data[:4] in (b'NXCX', b'NXGX'):
        return zlib.decompress(data[16:], 31 if data[:4] == b'NXGX' else 15)
    return data


def bntx_info(data: bytes) -> list[tuple[str, int, int, bool]]:
    count = struct.unpack_from('<I', data, 36)[0]
    table = struct.unpack_from('<Q', data, 40)[0]
    result = []
    for i in range(count):
        brti = struct.unpack_from('<Q', data, table + i * 8)[0]
        fmt = struct.unpack_from('<I', data, brti + 28)[0]
        w, h = struct.unpack_from('<II', data, brti + 36)
        p = struct.unpack_from('<Q', data, brti + 96)[0]
        n = struct.unpack_from('<H', data, p)[0]
        # Match the original engine's String::from_utf8_lossy for legacy Shift-JIS labels.
        name = data[p + 2:p + 2 + n].decode('utf-8', errors='replace')
        result.append((name, w, h, fmt & 255 == 6))
    return result


def convert_entry(cache: Path, image_source: Path, archive: str, index: int, name: str,
                  original: bytes, strips: int) -> tuple[bytes, dict]:
    if not original:
        return original, dict(index=index, name=name, kind='empty')
    data = unpack(original)
    force_png = archive == 'allui' or name.upper().startswith('RULE_')
    if data.startswith(b'KIMG'):
        converted, info = to_portable(data, strips, force_png)
        export_image(image_source, archive, index, 0, converted, info)
        return nx_wrap(converted), dict(index=index, name=name, kind='portable-image', **info)
    if data.startswith(b'BNTX'):
        textures = bntx_info(data)
        encoded, infos = [], []
        for k, (label, w, h, srgb) in enumerate(textures):
            with Image.open(cache / f'{index}_{k}.png') as image:
                assert image.size == (w, h)
                value, info = encode_portable(image, (w, h), srgb=srgb, force_png=force_png)
            export_image(image_source, archive, index, k, value, info)
            encoded.append((label, value))
            infos.append(dict(name=label, **info))
        if archive == 'allui':
            # Multi-texture atlas; retain every original texture name and logical metric.
            output = bytearray(b'KATL' + struct.pack('<II', 1, len(encoded)))
            for label, value in encoded:
                label = label.encode('utf-8')
                output.extend(struct.pack('<II', len(label), len(value)) + label + value)
            return nx_wrap(bytes(output)), dict(index=index, name=name, kind='portable-atlas', textures=infos)
        assert len(encoded) == 1
        info = {key: value for key, value in infos[0].items() if key != 'name'}
        return nx_wrap(encoded[0][1]), dict(index=index, name=name, kind='portable-image', **info)
    if data[:2] == b'\xff\xd8' or data.startswith(b'\x89PNG'):
        with Image.open(io.BytesIO(data)) as image:
            old_size = image.size
            converted, info = encode_portable(image, old_size, force_png=force_png)
        export_image(image_source, archive, index, 0, converted, info)
        return nx_wrap(converted), dict(index=index, name=name, kind='portable-image', **info)
    return original, dict(index=index, name=name, kind='retained-data')


def convert_archive(source: Path, assets: Path, image_source: Path, name: str, strips: int, jobs: int) -> dict:
    report = assets / f'{name}.compact.json'
    source_sha = digest(source / f'{name}.mrg')
    converter_sha = digest(Path(__file__)) + digest(ROOT / 'tools/compact_media.py')
    if report.exists():
        record = json.loads(report.read_text(encoding='utf-8'))
        if record.get('profile') == PROFILE and record.get('converter_sha256') == converter_sha:
            assert record['source_sha256'] == source_sha
            assert record['sha256'] == digest(assets / f'{name}.mrg')
            assert (image_source / name / 'manifest.json').exists()
            return record
    started = time.monotonic()
    cache = prepare_bntx(source, name, source_sha)
    rows = list(struct.iter_unpack('<IHH', (source / f'{name}.hed').read_bytes()))
    rows = rows[:next(i for i, row in enumerate(rows) if row[0] == 0xffffffff)]
    names_path = source / f'{name}.nam'
    names = names_path.read_bytes() if names_path.exists() else None
    partial = assets / f'{name}.mrg.partial'
    records, hed, total = [], bytearray(), 0
    with (source / f'{name}.mrg').open('rb') as file, mmap.mmap(file.fileno(), 0, access=mmap.ACCESS_READ) as mm, \
            partial.open('wb') as output, ThreadPoolExecutor(max_workers=jobs) as pool:
        def convert(i):
            label = names[i * 32:(i + 1) * 32].split(b'\0')[0].split(b'\r')[0].decode('utf-8') if names else f'#{i}'
            data = stored(mm, rows[i], missing=label == 'NOFILE.EXISTS')
            result, info = convert_entry(cache, image_source, name, i, label, data, strips)
            return result, info, (len(unpack(result)) + 2047) // 2048 if result else 0
        # Bounded batches avoid retaining thousands of decoded/compressed images in Futures.
        for start in range(0, len(rows), jobs * 2):
            for data, info, unpacked in pool.map(convert, range(start, min(start + jobs * 2, len(rows)))):
                count = (len(data) + 2047) // 2048
                assert count <= 65535 and unpacked <= 65535
                hed.extend(struct.pack('<IHH', total // 2048, count, unpacked))
                output.write(data)
                output.write(bytes(count * 2048 - len(data)))
                total += count * 2048
                info['stored_bytes'] = len(data)
                records.append(info)
            if start % 200 < jobs * 2:
                print(f'{name}: {len(records)}/{len(rows)}, {total / 1048576:.1f} MiB', flush=True)
    hed.extend(b'\xff' * 8)
    partial.replace(assets / f'{name}.mrg')
    (assets / f'{name}.hed').write_bytes(hed)
    if names is not None:
        shutil.copy2(names_path, assets / names_path.name)
    result = dict(name=name, profile=PROFILE, converter_sha256=converter_sha,
                  source_sha256=source_sha, sha256=digest(assets / f'{name}.mrg'),
                  source_bytes=(source / f'{name}.mrg').stat().st_size, output_bytes=total, entries=len(rows),
                  language_strips=strips, records=records, seconds=round(time.monotonic() - started, 2))
    write_json(report, result)
    write_json(image_source / name / 'manifest.json', result)
    print(f'{name} complete: {result["source_bytes"]:,} -> {total:,}', flush=True)
    return result


def convert_audio(src: Path, assets: Path, source: Path) -> dict:
    rel = src.relative_to(source).with_suffix('.ogg')
    dst = assets / rel
    report = assets.parent / 'media-manifests' / rel.with_suffix('.json')
    if dst.exists() and report.exists():
        record = json.loads(report.read_text(encoding='utf-8'))
        assert record['input_bytes'] == src.stat().st_size and record['output_bytes'] == dst.stat().st_size
        return record
    data, channels, samples = nopus_to_ogg(src.read_bytes())
    dst.parent.mkdir(parents=True, exist_ok=True)
    bitrate = '48k' if channels == 1 else '80k'
    temp = dst.with_suffix('.partial.ogg')
    proc = subprocess.run([str(FFMPEG), '-hide_banner', '-loglevel', 'error', '-nostdin', '-y', '-threads', '1',
                           '-i', 'pipe:0', '-map_metadata', '-1', '-c:a', 'libopus', '-b:a', bitrate,
                           '-vbr', 'on', '-compression_level', '8', '-application', 'audio', '-ar', '48000',
                           '-f', 'ogg', str(temp)], input=data, capture_output=True)
    if proc.returncode:
        raise RuntimeError(f'{src.name}: {proc.stderr.decode(errors="replace")}')
    temp.replace(dst)
    result = dict(path=rel.as_posix(), original=src.relative_to(source).as_posix(), channels=channels,
                  samples=samples, sample_rate=48000, bitrate=bitrate, input_bytes=src.stat().st_size,
                  output_bytes=dst.stat().st_size, sha256=digest(dst))
    write_json(report, result)
    return result


def convert_movie(src: Path, assets: Path, source: Path) -> dict:
    rel = src.relative_to(source)
    dst = assets / rel
    report = assets.parent / 'media-manifests' / rel.with_suffix('.json')
    if dst.exists() and report.exists():
        return json.loads(report.read_text(encoding='utf-8'))
    dst.parent.mkdir(parents=True, exist_ok=True)
    temp = dst.with_suffix('.partial.mp4')
    subprocess.run([str(FFMPEG), '-hide_banner', '-loglevel', 'error', '-nostdin', '-y', '-i', str(src),
                    '-map', '0:v:0', '-map', '0:a?', '-map_metadata', '-1', '-vf', 'scale=960:540:flags=lanczos',
                    '-c:v', 'libx264', '-preset', 'medium', '-crf', '22', '-pix_fmt', 'yuv420p', '-threads', '4',
                    '-c:a', 'copy', '-movflags', '+faststart', str(temp)], check=True)
    temp.replace(dst)
    probe = json.loads(subprocess.check_output([str(FFPROBE), '-v', 'error', '-show_streams', '-show_format',
                                               '-of', 'json', str(dst)]))
    result = dict(path=rel.as_posix(), input_bytes=src.stat().st_size, output_bytes=dst.stat().st_size,
                  sha256=digest(dst), probe=probe)
    write_json(report, result)
    print(f'movie {src.name}: {result["input_bytes"]:,} -> {result["output_bytes"]:,}', flush=True)
    return result


def main() -> None:
    global FFMPEG, FFPROBE
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=SOURCE)
    parser.add_argument('--out', type=Path, default=OUT)
    parser.add_argument('--jobs', type=int, default=6)
    parser.add_argument('--ffmpeg', type=Path, default=FFMPEG)
    parser.add_argument('--only', choices=('images', 'audio', 'movies'))
    parser.add_argument('--image-source', type=Path)
    args = parser.parse_args()
    FFMPEG = args.ffmpeg.resolve()
    FFPROBE = FFMPEG.with_name('ffprobe.exe' if FFMPEG.suffix.lower() == '.exe' else 'ffprobe')
    source, assets = args.source.resolve(), args.out.resolve() / 'assets'
    image_source = (args.image_source or args.out.with_name(args.out.name.removesuffix('-x64') + '-image-source')).resolve()
    assert 1 <= args.jobs <= 16 and assets != source and source not in assets.parents and assets not in source.parents
    assets.mkdir(parents=True, exist_ok=True)
    assert source.is_dir(), f'Missing previously prepared Chengkong 540p input: {source}'
    assert FFMPEG.is_file() and FFPROBE.is_file(), 'Set --ffmpeg to the ffmpeg executable beside ffprobe'
    (ROOT / '.work/compact-extract').mkdir(parents=True, exist_ok=True)
    changed = {f'{name}.{ext}' for name, _ in ARCHIVES for ext in ('hed', 'nam', 'mrg', 'compact.tsv')}
    for src in source.rglob('*'):
        if not src.is_file():
            continue
        rel = src.relative_to(source)
        if rel.as_posix() in changed or src.suffix in ('.opus', '.mp4'):
            continue
        dst = assets / rel
        if not dst.exists():
            dst.parent.mkdir(parents=True, exist_ok=True)
            try:
                os.link(src, dst)
            except OSError:
                shutil.copy2(src, dst)
    summary = dict(profile=PROFILE, source=str(source), target=str(args.out.resolve()),
                   image_source=str(image_source), images=[], audio=[], movies=[], complete=False)
    if args.only in (None, 'images'):
        for name, strips in ARCHIVES:
            record = convert_archive(source, assets, image_source, name, strips, args.jobs)
            summary['images'].append({k: v for k, v in record.items() if k != 'records'})
            write_json(args.out / 'compact-images.json', summary['images'])
        write_json(image_source / 'manifest.json', dict(profile=PROFILE, complete=True,
                   archives=summary['images'], maximum_per_image=[960, 540], languages=['ja', 'en', 'zc', 'zt']))
    if args.only in (None, 'audio'):
        files = sorted(source.rglob('*.opus'))
        with ThreadPoolExecutor(max_workers=args.jobs) as pool:
            for i, record in enumerate(pool.map(lambda src: convert_audio(src, assets, source), files), 1):
                summary['audio'].append(record)
                if i % 200 == 0:
                    print(f'audio: {i}/{len(files)}', flush=True)
        write_json(args.out / 'compact-audio.json', summary['audio'])
    if args.only in (None, 'movies'):
        for src in sorted(source.rglob('*.mp4')):
            summary['movies'].append(convert_movie(src, assets, source))
        write_json(args.out / 'compact-movies.json', summary['movies'])
    if not args.only:
        summary.update(complete=True, output_bytes=sum(p.stat().st_size for p in assets.rglob('*') if p.is_file()))
        write_json(args.out / 'asset-build.json', summary)


if __name__ == '__main__':
    main()

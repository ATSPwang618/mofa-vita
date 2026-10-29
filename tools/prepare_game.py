"""Prepare knife's assets from local NSPs and an installed Switch firmware.

Uses the user's existing prod.keys in place; keys are never copied or logged.
Run from any directory. hactool receives ASCII relative output paths because
its Windows release predates reliable Unicode command-line handling.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import time

ROOT = Path(__file__).resolve().parent.parent
HACTOOL = ROOT / '.tools/hactool/hactool.exe'
FONTS = {
    '0100000000000811': ['nintendo_udsg-r_std_003.bfttf'],
    '0100000000000814': [
        'nintendo_udsg-r_org_zh-cn_003.bfttf',
        'nintendo_udsg-r_ext_zh-cn_003.bfttf',
    ],
    '0100000000000813': ['nintendo_udjxh-db_zh-tw_003.bfttf'],
}


def relative(path):
    return os.path.relpath(path, ROOT)


def save_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


def unpack_nsp(path, kind):
    destination = ROOT / '.work/nca' / kind
    destination.mkdir(parents=True, exist_ok=True)
    manifest_path = destination / 'manifest.json'
    previous = json.loads(manifest_path.read_text(encoding='utf-8')) if manifest_path.exists() else {}
    records = []
    with path.open('rb') as source:
        magic, count, names_size, _ = struct.unpack('<4sIII', source.read(16))
        if magic != b'PFS0' or count > 10000 or names_size > 16 * 1024 * 1024:
            raise ValueError(f'Invalid NSP header: {path.name}')
        entries = source.read(count * 24)
        names = source.read(names_size)
        data_offset = 16 + count * 24 + names_size
        for index in range(count):
            offset, size, name_offset, _ = struct.unpack_from('<QQII', entries, index * 24)
            name = names[name_offset:names.index(b'\0', name_offset)].decode('utf-8')
            if Path(name).name != name or '/' in name or '\\' in name or ':' in name:
                raise ValueError('Unsafe NSP entry name')
            if data_offset + offset + size > path.stat().st_size:
                raise ValueError('NSP entry extends beyond the source file')
            # Tickets, if present in a different dump, are unnecessary for this
            # rights-free input and are not copied into the working directory.
            if not name.endswith('.nca'):
                continue
            target = destination / name
            cached = next((r for r in previous.get('entries', []) if r['name'] == name), None)
            if cached and target.exists() and target.stat().st_size == size:
                records.append(cached)
                print(f'{kind}: reusing {name}', flush=True)
                continue
            source.seek(data_offset + offset)
            digest = hashlib.sha256()
            remaining = size
            partial = target.with_suffix('.partial')
            next_progress = time.monotonic() + 10
            with partial.open('wb') as output:
                while remaining:
                    block = source.read(min(8 * 1024 * 1024, remaining))
                    if not block:
                        raise ValueError('Unexpected end of NSP')
                    output.write(block)
                    digest.update(block)
                    remaining -= len(block)
                    if time.monotonic() >= next_progress:
                        print(f'{kind}: {name}: {(size - remaining) / size:.0%}', flush=True)
                        next_progress = time.monotonic() + 10
            partial.replace(target)
            record = {'name': name, 'bytes': size, 'sha256': digest.hexdigest()}
            records.append(record)
            print(f'{kind}: extracted {name} ({size:,} bytes)', flush=True)
    manifest = {'source': str(path), 'bytes': path.stat().st_size, 'entries': records}
    save_json(manifest_path, manifest)
    # This dump has a single program NCA; confirm content type after extraction.
    program = max(records, key=lambda record: record['bytes'])
    return destination / program['name'], manifest


def hactool(keys, arguments, log_name=None):
    result = subprocess.run(
        [str(HACTOOL), '-k', str(keys), '--disablekeywarns', '--suppresskeys', *arguments],
        cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
    )
    output = result.stdout.decode('utf-8', errors='replace')
    # Suppress any key-like line in addition to hactool's own suppression flag.
    safe = '\n'.join(line for line in output.splitlines()
                     if not re.search(r'(?i)(title.?key|content.?key|key area|decrypted key|master_key|key_source)', line))
    if log_name:
        (ROOT / 'doc/logs' / log_name).write_text(safe + '\n', encoding='utf-8')
    if result.returncode:
        raise RuntimeError(f'hactool failed ({result.returncode}): {safe[-4000:]}')
    return safe


def prepare_fonts(keys, firmware, assets):
    cache_path = ROOT / 'doc/fonts-provenance.json'
    if cache_path.exists():
        cached = json.loads(cache_path.read_text(encoding='utf-8'))
        expected = {name for names in FONTS.values() for name in names}
        if set(cached) == expected and all(
            (assets / name).is_file()
            and hashlib.sha256((assets / name).read_bytes()).hexdigest() == record['sha256']
            for name, record in cached.items()
        ):
            print('Shared fonts already verified', flush=True)
            return cached
    found = {}
    candidates = []
    for item in sorted(firmware.iterdir()):
        path = item / '00' if item.is_dir() else item
        if path.is_file():
            candidates.append(path)
    print(f'Scanning {len(candidates)} firmware archives for shared fonts', flush=True)
    for index, path in enumerate(candidates):
        info = hactool(keys, ['-i', str(path)])
        match = re.search(r'Title ID:\s*([0-9a-fA-F]{16})', info)
        if not match:
            match = re.search(r'Program Id:\s*([0-9a-fA-F]{16})', info, re.IGNORECASE)
        if match and match.group(1).lower() in FONTS and 'RomFS' in info:
            title_id = match.group(1).lower()
            print(f'Found font title {title_id}: {path.parent.name}', flush=True)
            hactool(keys, ['--romfsdir=' + relative(assets), str(path)], f'font-{title_id}.log')
            for name in FONTS[title_id]:
                font = assets / name
                if not font.is_file():
                    raise RuntimeError(f'Font archive did not contain {name}')
                found[name] = {'title_id': title_id, 'bytes': font.stat().st_size,
                               'sha256': hashlib.sha256(font.read_bytes()).hexdigest(),
                               'source_archive': str(path)}
            if len(found) == sum(map(len, FONTS.values())):
                break
        if index % 50 == 0:
            print(f'Firmware archives examined: {index + 1}', flush=True)
    missing = [name for names in FONTS.values() for name in names if name not in found]
    if missing:
        raise RuntimeError(f'Shared fonts not found: {missing}')
    save_json(cache_path, found)
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base', type=Path, required=True)
    parser.add_argument('--update', type=Path, required=True)
    parser.add_argument('--keys', type=Path, required=True)
    parser.add_argument('--firmware', type=Path, required=True)
    parser.add_argument('--out', type=Path, default=ROOT / 'dist/Witch-on-the-Holy-Night-Windows-x64/assets')
    parser.add_argument('--fonts-only', action='store_true')
    args = parser.parse_args()
    assets = args.out.resolve()
    assets.mkdir(parents=True, exist_ok=True)
    (ROOT / 'doc/logs').mkdir(parents=True, exist_ok=True)
    manifest_path = ROOT / 'doc/assets-provenance.json'
    manifest = json.loads(manifest_path.read_text(encoding='utf-8')) if manifest_path.exists() else {}
    if not args.fonts_only:
        base_program, base = unpack_nsp(args.base, 'base')
        update_program, update = unpack_nsp(args.update, 'update-1.02')
        for name, path in [('base', base_program), ('update-1.02', update_program)]:
            info = hactool(args.keys, ['-i', relative(path)], f'nca-{name}.log')
            if not re.search(r'Content Type:\s*Program', info, re.IGNORECASE):
                raise RuntimeError(f'Largest {name} NCA is not a program')
        print('Extracting the merged 1.02 RomFS (base + update BKTR)', flush=True)
        started = time.monotonic()
        hactool(args.keys, ['--basenca=' + relative(base_program),
                           '--romfsdir=' + relative(assets), relative(update_program)], 'romfs-1.02.log')
        print(f'RomFS extracted in {time.monotonic() - started:.1f} seconds', flush=True)
        manifest.update({'base': base, 'update': update, 'merged_assets': str(assets)})
        save_json(manifest_path, manifest)
    manifest['fonts'] = prepare_fonts(args.keys, args.firmware, assets)
    files = [path for path in assets.rglob('*') if path.is_file()]
    manifest['asset_files'] = len(files)
    manifest['asset_bytes'] = sum(path.stat().st_size for path in files)
    save_json(manifest_path, manifest)
    print(f'Assets ready: {len(files):,} files; {manifest["asset_bytes"]:,} bytes', flush=True)


if __name__ == '__main__':
    main()

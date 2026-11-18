"""Build the local Chengkong translation / 540p asset variant from extracted 1.02 data."""
from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_SOURCE = ROOT / 'dist/Witch-on-the-Holy-Night-Windows-x64/assets'
DEFAULT_OUT = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-x64'
DEFAULT_MOD = Path(os.environ['KNIFE_TRANSLATION_ROMFS']).expanduser() if os.environ.get('KNIFE_TRANSLATION_ROMFS') else None
ARCHIVES = [('allpachdml', 0.5, 4), ('allpacml', 0.75, 4),
            ('jallpachd', 0.5, 1), ('jallpac', 0.75, 1)]


def digest(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + '.partial')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    temporary.replace(path)


def private_copy(src: Path, dst: Path) -> None:
    """A MOD copy must never write through a hard link into the original assets."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    if dst.exists():
        if not os.path.samefile(src, dst) and digest(src) == digest(dst):
            return
        raise FileExistsError(f'Refusing to replace existing different file: {dst}')
    shutil.copy2(src, dst)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', type=Path, default=DEFAULT_SOURCE)
    parser.add_argument('--mod', type=Path, default=DEFAULT_MOD, required=DEFAULT_MOD is None)
    parser.add_argument('--out', type=Path, default=DEFAULT_OUT)
    parser.add_argument('--jobs', type=int, default=8)
    parser.add_argument('--pilot', type=Path, help='Reuse a verified allpachdml pilot conversion')
    args = parser.parse_args()
    source, out, mod = args.source.resolve(), args.out.resolve(), args.mod.resolve()
    assets = out / 'assets'
    if source == assets or source in assets.parents or assets in source.parents:
        raise SystemExit('Source and destination assets must be separate directories')
    if not 1 <= args.jobs <= 32:
        raise SystemExit('--jobs must be between 1 and 32')
    mod_files = {p.relative_to(mod).as_posix(): p for p in mod.rglob('*') if p.is_file()}
    required = {'allui.mrg', 'script_text.mrg', 'zc/arshanghaisonggbpro_lt.otf', 'zt/SungASC-Medium.TTF'}
    if set(mod_files) != required:
        raise SystemExit(f'Unexpected MOD file set: {sorted(mod_files)}')
    generated = {f'{name}.{ext}' for name, _, _ in ARCHIVES for ext in ('hed', 'nam', 'mrg')}
    source_files = sorted(p for p in source.rglob('*') if p.is_file())
    assets.mkdir(parents=True, exist_ok=True)
    logs = ROOT / 'doc/logs'
    logs.mkdir(parents=True, exist_ok=True)
    manifest_path = out / 'asset-build.json'
    manifest = {'format': 1, 'source': str(source), 'mod': str(mod),
                'converter_sha256': digest(ROOT / 'tools/kdu.exe'),
                'source_file_count': len(source_files),
                'source_bytes': sum(p.stat().st_size for p in source_files),
                'translation': [{'path': rel, 'bytes': p.stat().st_size, 'sha256': digest(p)}
                                for rel, p in sorted(mod_files.items())],
                'archives': [], 'complete': False}
    if manifest_path.exists():
        previous = json.loads(manifest_path.read_text(encoding='utf-8'))
        for key in ('source', 'mod', 'converter_sha256', 'translation'):
            if previous.get(key) != manifest[key]:
                raise SystemExit(f'Existing build manifest has different {key}')
    linked = copied = 0
    for src in source_files:
        rel = src.relative_to(source)
        if rel.as_posix() in generated or rel.as_posix() in mod_files:
            continue
        dst = assets / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        if dst.exists():
            if not os.path.samefile(src, dst) and digest(src) != digest(dst):
                raise FileExistsError(f'Existing file differs: {dst}')
            continue
        try:
            os.link(src, dst)
            linked += 1
        except OSError:
            shutil.copy2(src, dst)
            copied += 1
    for rel, src in mod_files.items():
        private_copy(src, assets / rel)
    print(f'Assets prepared: {linked} hard links, {copied} copies, {len(mod_files)} private MOD copies', flush=True)
    write_json(manifest_path, manifest)
    for name, scale, strips in ARCHIVES:
        targets = [assets / f'{name}.{ext}' for ext in ('mrg', 'hed', 'nam', 'compact.tsv')]
        if not any(p.exists() for p in targets) and name == 'allpachdml' and args.pilot:
            for dst in targets:
                private_copy(args.pilot.resolve() / dst.name, dst)
        start = time.monotonic()
        if not all(p.exists() for p in targets):
            if any(p.exists() for p in targets):
                raise SystemExit(f'Incomplete output set for {name}; inspect its .partial files')
            command = [str(ROOT / 'tools/kdu.exe'), '--romfs', str(source), 'compact', name,
                       str(assets), '--scale', str(scale), '--strips', str(strips), '--jobs', str(args.jobs)]
            with (logs / f'compact-{name}.log').open('w', encoding='utf-8') as log:
                with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                      text=True, encoding='utf-8', errors='replace') as process:
                    for line in process.stdout:
                        print(line, end='', flush=True)
                        log.write(line)
                        log.flush()
                    if process.wait() != 0:
                        raise SystemExit(f'Conversion failed: {name}; see {log.name}')
        with targets[3].open(encoding='utf-8') as stream:
            rows = list(csv.DictReader(stream, delimiter='\t'))
        if digest(assets / f'{name}.nam') != digest(source / f'{name}.nam'):
            raise SystemExit(f'Archive names/order changed: {name}')
        record = {'name': name, 'scale': scale, 'language_strips': strips, 'entries': len(rows),
                  'resized': sum(row['kind'] != 'original' for row in rows),
                  'original_bytes': (source / f'{name}.mrg').stat().st_size,
                  'output_bytes': targets[0].stat().st_size, 'sha256': digest(targets[0]),
                  'elapsed_this_run_seconds': round(time.monotonic() - start, 2)}
        manifest['archives'].append(record)
        write_json(manifest_path, manifest)
        print(json.dumps(record, ensure_ascii=False), flush=True)
    files = [p for p in assets.rglob('*') if p.is_file()]
    manifest.update(output_file_count=len(files), output_bytes=sum(p.stat().st_size for p in files),
                    complete=True, unchanged_files='NTFS hard links when available; MOD and converted archives are independent files')
    write_json(manifest_path, manifest)
    write_json(ROOT / 'doc/540p-assets.json', manifest)
    print(f'Complete: {manifest["source_bytes"]:,} -> {manifest["output_bytes"]:,} bytes', flush=True)


if __name__ == '__main__':
    main()

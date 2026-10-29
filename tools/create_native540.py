"""Assemble the native 960x540 Vita asset pack (KNIF00540) from the 540p master.

The 540p Compact tree already stores every archive at 960x540 with logical
coordinates preserved, so this step only selects the runtime contract files:
the six image archives (and their indices), the three loose images, scripts,
audio, fonts, movies and the language tables. It never resamples pixels and
never rewrites an archive, so a matching source is byte-identical in the pack.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/assets'
OUTPUT = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets'
SCHEMA = 'knife-540p-assets-v1'
METHOD = 'native-540p-preserve-logical-v1'
ARCHIVES = ['jallpac', 'jallpachd', 'allui', 'allpacml', 'allpachdml', 'parts']
LOOSE_IMAGES = ['corp.tga', 'saveicon_en.tga', 'saveicon_zc.tga']
SHARED = ['allscr.mrg', 'script_text.mrg', 'witch.bfsar', 'exfont_nx01.bffnt',
          'language-profile.txt', 'ui-aliases.tsv',
          'nintendo_udsg-r_org_zh-cn_003.bfttf', 'nintendo_udsg-r_std_003.bfttf',
          'nintendo_udsg-r_ext_zh-cn_003.bfttf']
SHARED_DIRS = ['en', 'zc', 'movie', 'shader', 'stream']
LANGUAGES = ['en', 'zc']


def digest(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def selected():
    names = []
    for archive in ARCHIVES:
        names += [f'{archive}.mrg', f'{archive}.hed']
        if (SOURCE / f'{archive}.nam').is_file():
            names.append(f'{archive}.nam')
    names += [n for n in LOOSE_IMAGES if (SOURCE / n).is_file()]
    names += [n for n in SHARED if (SOURCE / n).is_file()]
    for directory in SHARED_DIRS:
        base = SOURCE / directory
        names += [p.relative_to(SOURCE).as_posix() for p in sorted(base.rglob('*')) if p.is_file()]
    return sorted(set(names))


def output_codecs(directory: Path):
    """Codecs actually present in the finished archives, not in the source audit."""
    sys.path.insert(0, str(ROOT / "tools"))
    from optimize_compact import textures
    from verify_540p_assets import decoded, stored
    import mmap
    seen = set()
    for archive in ARCHIVES:
        hed, mrg = directory / f"{archive}.hed", directory / f"{archive}.mrg"
        if not hed.is_file() or not mrg.is_file():
            continue
        rows = list(struct.iter_unpack("<IHH", hed.read_bytes()))
        terminal = next(i for i, row in enumerate(rows) if row[0] == 0xFFFFFFFF)
        with mrg.open("rb") as handle, mmap.mmap(handle.fileno(), 0, access=mmap.ACCESS_READ) as mm:
            for row in rows[:terminal]:
                raw = decoded(stored(mm, row))
                members = textures(raw) if raw[:4] == b"KATL" else [("", raw)]
                for _, member in members:
                    if member[:4] == b"KIMG":
                        seen.add(struct.unpack_from("<4s7I", member)[6])
    return sorted(seen)


def statistics():
    entries = textures = pixels = 0
    codecs = set()
    archives = []
    for archive in ARCHIVES:
        audit = SOURCE / f'{archive}.compact.json'
        if not audit.is_file():
            audit = OUTPUT / f'{archive}.compact.json'
        count = 0
        area = 0
        data = None
        if audit.is_file():
            data = json.loads(audit.read_text(encoding='utf-8'))
            # Two record shapes: a flat image record (physical at top level) and an
            # atlas record that nests one entry per member texture.
            for record in data.get('records', []):
                nested = record.get('textures')
                if nested:
                    count += len(nested)
                    area += sum(t['physical'][0] * t['physical'][1] for t in nested)
                    codecs.update(t.get('encoding', 0) for t in nested)
                elif 'physical' in record:
                    count += 1
                    area += record['physical'][0] * record['physical'][1]
                    codecs.add(record.get('encoding', 0))
            entries += data.get('entries', len(data.get('records', [])))
        textures += count
        pixels += area
        archives.append({'name': archive,
                         'entries': (data or {}).get('entries', 0),
                         'textures': count, 'pixels': area})
    return archives, entries, textures, pixels, sorted(codecs)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out', type=Path, default=OUTPUT)
    parser.add_argument('--force', action='store_true', help='replace an existing pack')
    parser.add_argument('--manifest-only', action='store_true',
                        help='rewrite the manifest from an existing pack without copying')
    args = parser.parse_args()
    out = args.out
    if args.manifest_only:
        names = [p.relative_to(out).as_posix() for p in sorted(out.rglob('*')) if p.is_file()]
        names = [n for n in names
                 if n != 'vita-540p-assets.json' and not n.endswith('.dxt.json')]
        files = [{'path': n, 'bytes': (out / n).stat().st_size, 'sha256': digest(out / n)}
                 for n in names]
        archives, entries, textures, pixels, _ = statistics()
        codecs = output_codecs(out)
        write_manifest(out, files, archives, entries, textures, pixels, codecs)
        print(f'codecs present: {codecs}')
        print(f'manifest rewritten: {len(files)} files, {textures} textures, {pixels} pixels')
        return
    if out.exists() and not args.force:
        raise SystemExit(f'{out} already exists; pass --force to rebuild')
    names = selected()
    if not names:
        raise SystemExit(f'no source files under {SOURCE}')
    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    files = []
    total = 0
    for name in names:
        src, dst = SOURCE / name, out / name
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(src, dst)
        size = dst.stat().st_size
        total += size
        files.append({'path': name, 'bytes': size, 'sha256': digest(dst)})
    archives, entries, textures, pixels, _ = statistics()
    write_manifest(out, files, archives, entries, textures, pixels, output_codecs(out))
    print(f'{len(files)} files, {total} bytes, {textures} textures, {pixels} pixels -> {out}')


def write_manifest(out: Path, files, archives, entries, textures, pixels, codecs) -> None:
    total = sum(f['bytes'] for f in files)
    manifest = {
        'schema': SCHEMA,
        'method': METHOD,
        'render': [960, 540],
        'display': [960, 540],
        'composite': [1920, 1080],
        'languages': LANGUAGES,
        'codecs': codecs or [1, 2, 7],
        'archives': archives,
        'statistics': {'files': len(files), 'bytes': total, 'entries': entries,
                       'textures': textures, 'pixels': pixels},
        # These describe the pack contract, not game behaviour: every file above was
        # hashed, no pixel was resampled, and no runtime HD fallback exists. Gameplay
        # semantics (grade/soft-clip paths) are covered by the separate keep-list and
        # must not be inferred from this record.
        'verification': {'files_hashed': True, 'source_preserved': False,
                         'resampled': False, 'runtime_hd_fallback': False},
        'files': files,
    }
    (out / 'vita-540p-assets.json').write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')


if __name__ == '__main__':
    main()
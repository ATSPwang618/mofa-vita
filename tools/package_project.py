"""Create a source/build/evidence snapshot with an explicit file allowlist."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import zipfile

ROOT = Path(__file__).resolve().parents[1]
RUNTIME = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-x64'
PREFIX = 'Witch-on-the-Holy-Night-540p-project'


def sha256(path: Path) -> str:
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def source_files(runtime: Path, variant: str) -> list[Path]:
    selected = [ROOT / 'doc/01-项目总览/项目总览与当前交付.md']
    for base, directories, names in os.walk(ROOT / 'knife'):
        directories[:] = [d for d in directories if d not in {'.git', 'target', '__pycache__'}]
        selected.extend(Path(base) / name for name in names)
    selected.extend(p for p in (ROOT / 'tools').iterdir()
                    if p.is_file() and (p.suffix in {'.py', '.sh', '.ps1', '.txt'} or p.name in {'kdu.exe', 'kdu-compact.exe'}))
    selected.extend(p for p in (ROOT / 'doc').iterdir()
                    if p.is_file() and p.suffix in {'.md', '.json'} and not p.name.endswith('project-package.json')
                    and p.name != 'compact-source-package-verification.json')
    logs = ROOT / 'doc/logs'
    for pattern in ['build-compact-production.log', 'build-compact-qa.log', 'build-compact-kdu.log', 'build-tests.log',
                    'pe-compact-production.txt', 'pe-compact-qa.txt', 'pe-compact-kdu.txt',
                    'compact-*.log', 'regression-*.log']:
        selected.extend(logs.glob(pattern))
    if variant == 'compact':
        selected.extend((logs / 'compact').rglob('*.log'))
        selected.extend((ROOT / 'doc/screenshots/compact').rglob('*.png'))
        selected.extend((logs / 'compact-en').rglob('*.log'))
        selected.extend((ROOT / 'doc/screenshots/compact-en').rglob('*.png'))
        selected.extend(logs.glob('vita-toolchain-inventory.log'))
        selected.extend(logs.glob('vita-review-toolchain-inventory.log'))
    for name in ['knife.exe', 'knife.ini', 'THIRD-PARTY-NOTICES.txt']:
        selected.append(runtime / name)
    selected.append(ROOT / 'doc/07-历史归档/Windows运行说明-540p紧凑版.md')
    selected.extend((runtime / 'licenses').glob('*.txt'))
    return sorted(set(selected), key=lambda p: p.relative_to(ROOT).as_posix())


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--variant', choices=['540p', 'compact'], default='compact')
    parser.add_argument('--out', type=Path)
    parser.add_argument('--replace', action='store_true', help='Atomically replace an existing project ZIP after verifying the new archive')
    args = parser.parse_args()
    compact = args.variant == 'compact'
    runtime = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64' if compact else RUNTIME
    prefix = 'Witch-on-the-Holy-Night-540p-Compact-project' if compact else PREFIX
    output_name = 'Witch-on-the-Holy-Night-Chengkong-540p-Compact-project.zip' if compact else 'Witch-on-the-Holy-Night-Chengkong-540p-project.zip'
    args.out = args.out or ROOT / 'dist' / output_name
    assets = json.loads((runtime / 'asset-build.json').read_text(encoding='utf-8'))
    validation_name = 'compact-assets-validation.json' if compact else '540p-assets-validation.json'
    validation = json.loads((ROOT / 'doc' / validation_name).read_text(encoding='utf-8'))
    assert assets['complete'] and validation['complete'], 'Complete and verify assets before packaging'
    if compact:
        assert assets['profile'] == 'png-jpeg-540p-v1'
        assert all(r['no_webp_or_bntx_images_remaining'] for r in validation['archives'])
    files = source_files(runtime, args.variant)
    records = []
    forbidden = {'.nsp', '.nca', '.keys', '.bfttf', '.mrg', '.hed', '.nam', '.otf', '.ttf'}
    for path in files:
        relative = path.relative_to(ROOT)
        assert path.is_file() and path.resolve().is_relative_to(ROOT.resolve()), path
        assert not any(part in {'.git', 'assets', 'saves', '.work'} for part in relative.parts), path
        assert path.suffix.lower() not in forbidden and path.name != 'savedata000', path
        records.append({'path': relative.as_posix(), 'bytes': path.stat().st_size, 'sha256': sha256(path)})
    if (ROOT / 'knife/.git').exists():
        git = lambda *a: subprocess.check_output(['git', '-C', str(ROOT / 'knife'), *a], text=True).strip()
        commit, branch, status = git('rev-parse', 'HEAD'), git('branch', '--show-current'), git('status', '--short')
    else:
        identity = dict(line.split(': ', 1) for line in
                        (ROOT / 'knife/UPSTREAM.txt').read_text(encoding='utf-8').splitlines() if ': ' in line)
        commit, branch, status = identity['Base commit'], identity['Local branch'], 'Source snapshot; no Git metadata'
    manifest = {
        'format': 1, 'upstream_commit': commit,
        'variant': args.variant, 'image_profile': assets.get('profile'),
        'branch': branch, 'local_status': status,
        'game_assets_included': False, 'keys_included': False, 'personal_saves_included': False,
        'files': records,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    assert not args.out.exists() or args.replace, 'Output exists; use --replace to update the project ZIP'
    archive_path = args.out.with_name(args.out.name + '.partial') if args.replace else args.out
    with zipfile.ZipFile(archive_path, 'x', compression=zipfile.ZIP_DEFLATED, compresslevel=6) as archive:
        for path, record in zip(files, records):
            archive.write(path, prefix + '/' + record['path'])
        archive.writestr(prefix + '/SNAPSHOT.json', json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')
    with zipfile.ZipFile(archive_path) as archive:
        assert archive.testzip() is None, 'ZIP CRC validation failed'
        assert len(archive.namelist()) == len(records) + 1
        for record in records:
            contents = archive.read(prefix + '/' + record['path'])
            assert len(contents) == record['bytes']
            assert hashlib.sha256(contents).hexdigest() == record['sha256'], record['path']
    if args.replace:
        os.replace(archive_path, args.out)
    report = {'path': str(args.out.resolve()), 'bytes': args.out.stat().st_size,
              'sha256': sha256(args.out), 'files': len(records) + 1,
              'all_zip_members_crc_and_sha256_verified': True,
              'game_assets_included': False, 'keys_included': False, 'personal_saves_included': False}
    report_name = 'compact-project-package.json' if compact else 'project-package.json'
    (ROOT / 'doc' / report_name).write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()

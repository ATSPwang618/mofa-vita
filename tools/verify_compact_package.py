"""Verify the compact source ZIP, then build its Windows engine from a fresh extraction."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def digest(path: Path) -> str:
    h = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(4 * 1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--package', type=Path, default=ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-project.zip')
    parser.add_argument('--destination', type=Path, default=ROOT / '.work/compact-package-review')
    parser.add_argument('--distro', default=os.environ.get('KNIFE_WSL_DISTRO'))
    args = parser.parse_args()
    target = args.destination.resolve()
    if target.exists():
        raise SystemExit('Destination already exists; select a new --destination for a fresh extraction')
    with zipfile.ZipFile(args.package) as archive:
        assert archive.testzip() is None
        names = archive.namelist()
        assert len(names) == len(set(names))
        roots = {name.split('/')[0] for name in names}
        assert len(roots) == 1
        prefix = next(iter(roots))
        snapshot = json.loads(archive.read(prefix + '/SNAPSHOT.json'))
        assert snapshot['image_profile'] == 'png-jpeg-540p-v1'
        assert not any(snapshot[k] for k in ('game_assets_included', 'keys_included', 'personal_saves_included'))
        expected = {prefix + '/' + r['path'] for r in snapshot['files']}
        assert set(names) == expected | {prefix + '/SNAPSHOT.json'}
        for name in names:
            assert (target / name).resolve().is_relative_to(target), name
            assert '\\' not in name and ':' not in name
        for record in snapshot['files']:
            data = archive.read(prefix + '/' + record['path'])
            assert len(data) == record['bytes']
            assert hashlib.sha256(data).hexdigest() == record['sha256'], record['path']
        target.mkdir(parents=True)
        archive.extractall(target)
    source = target / prefix
    assert not (source / 'knife/.git').exists()
    assert snapshot['upstream_commit'] == '8de7f3b147e1c430fdfdc66af0ced7e79f8ab642'
    version = tomllib.loads((source / 'knife/knife/Cargo.toml').read_text(encoding='utf-8'))['package']['version']
    assert version == '0.9.4'
    print(f'Verified {len(names)} ZIP members; building from {source}', flush=True)
    subprocess.run(['wsl'] + (['-d', args.distro] if args.distro else []) + ['--', 'bash', './tools/build-compact.sh'], cwd=source, check=True)
    executable = source / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Compact-x64/knife.exe'
    raw = executable.read_bytes()
    pe = struct.unpack_from('<I', raw, 0x3c)[0]
    assert raw[:2] == b'MZ' and raw[pe:pe + 4] == b'PE\0\0'
    machine, magic = struct.unpack_from('<H', raw, pe + 4)[0], struct.unpack_from('<H', raw, pe + 24)[0]
    assert (machine, magic) == (0x8664, 0x20b)
    help_result = subprocess.run([str(executable), '--help'], cwd=target, capture_output=True, text=True)
    assert help_result.returncode == 0 and '--assets-dir' in help_result.stdout, help_result.stderr
    persistent_log = ROOT / 'doc/logs/compact/source-package-rebuild.log'
    persistent_log.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source / 'doc/logs/build-compact-production.log', persistent_log)
    report = dict(archive=str(args.package.resolve()), archive_sha256=digest(args.package),
                  upstream_version=version, upstream_commit=snapshot['upstream_commit'],
                  archive_crc_and_member_hashes_verified=True, verified_zip_members=len(names),
                  source_snapshot_extracted=str(source), git_metadata_absent=True,
                  engine_rebuilt_from_snapshot=True, rebuild_used_existing_wsl_toolchain=True,
                  snapshot_rebuilt_engine_sha256=digest(executable), executable_bytes=len(raw),
                  machine=hex(machine), optional_header_magic=hex(magic), windows_help_exit=help_result.returncode,
                  build_log=str(persistent_log),
                  scope='Fresh source extraction without Git metadata rebuilt the Windows x64 engine; --help ran on Windows. Game scenes were checked with the shipped executable separately.')
    path = ROOT / 'doc/compact-source-package-verification.json'
    path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()

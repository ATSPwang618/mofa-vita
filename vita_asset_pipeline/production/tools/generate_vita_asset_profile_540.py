"""Pin bounded startup checks to the native 960x540 asset pack (KNIF00540).

The generated Rust profile travels inside the VPK. No game bytes are embedded.
--check compares without writing. Full SHA-256 verification occurs here, on PC;
Vita reads small metadata files and only the edges of large payloads at startup.
"""
import argparse
import hashlib
import json
import struct
from pathlib import Path, PurePosixPath
import zlib
from vita_image_metadata import check_story_rgba_budget

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'dist/Witch-on-the-Holy-Night-Chengkong-540p-Vita/assets'
OUTPUT = ROOT / 'knife/knife-platform-vita/src/asset_profile_540p.rs'
MARKER = 'vita-540p-assets.json'


def audio_bank_locations(assets):
    """Resolve logical stream names without extracting duplicate loose audio."""
    result = {}
    for name in ('voice.mrg', 'bgm.mrg'):
        path = assets / name
        if not path.is_file():
            continue
        with path.open('rb') as source:
            magic, count, crc = struct.unpack('<8sII', source.read(16))
            assert magic == b'KAUDMRG1' and 0 < count <= 16384, name
            index = source.read(count * 40)
        assert len(index) == count * 40 and zlib.crc32(index) == crc, name
        end, previous = 16 + count * 40, ''
        for offset in range(0, len(index), 40):
            record = index[offset:offset+40]
            key = record[:32].split(b'\0')[0].decode('ascii')
            position, size = struct.unpack('<II', record[32:])
            assert key > previous and '/' not in key and '\\' not in key
            assert position == end and size > 0 and 'stream/' + key not in result
            result['stream/' + key] = (path, position, size)
            previous, end = key, position + size
        assert end == path.stat().st_size, name
    return result


def generate(assets: Path = ASSETS, base_assets=None):
    marker = (assets / MARKER).read_bytes()
    manifest = json.loads(marker)
    assert manifest['schema'] == 'knife-540p-assets-v1'
    assert manifest['method'] == 'native-540p-encoded-unorm-v2'
    assert manifest['color_contract'] == 'encoded-unorm8-v1'
    assert manifest['render'] == [960, 540] and manifest['display'] == [960, 540]
    assert manifest['languages'] == ['en', 'zc']
    assert set(manifest['codecs']) <= {1, 2, 7, 9} and 8 not in manifest['codecs'], manifest['codecs']
    assert manifest['verification']['files_hashed']
    files = {r['path']: r for r in manifest['files']}
    # Native GXM uses embedded GXP shaders; BNSh stays outside the runtime contract.
    names = sorted(n for n in files if not n.startswith(('stream/', 'shader/')))
    start_sidecars = sorted(n for n in files if n.startswith('stream/') and n.endswith('.bgmstart.mrg'))
    streams = sorted(n for n in files if n.startswith('stream/') and n not in start_sidecars)
    assert len(streams) == 9470
    names += [streams[0], streams[-1], *start_sidecars]
    banks = audio_bank_locations(assets)
    if base_assets is not None:
        for key, value in audio_bank_locations(base_assets).items():
            banks.setdefault(key, value)
    rows = []
    total_read = 0
    for name in [MARKER] + names:
        pure = PurePosixPath(name)
        assert not pure.is_absolute() and '..' not in pure.parts and ':' not in name and '\\' not in name
        p = assets / name
        if not p.is_file() and base_assets is not None: p = base_assets / name
        offset = 0
        if p.is_file():
            size = p.stat().st_size
        else:
            p, offset, size = banks[name]
        if name != MARKER:
            expected = files[name]
            assert size == expected['bytes'], name
            with p.open('rb') as f:
                f.seek(offset)
                digest = hashlib.sha256()
                left = size
                while left:
                    block = f.read(min(left, 1024 * 1024))
                    assert block, name
                    digest.update(block)
                    left -= len(block)
                assert digest.hexdigest() == expected['sha256'], name
        if name == MARKER or size <= 1024 * 1024:
            with p.open('rb') as f:
                f.seek(offset)
                data = f.read(size)
            content = f'ContentCheck::Full(0x{zlib.crc32(data):08x})'
            total_read += size
        else:
            with p.open('rb') as f:
                f.seek(offset)
                first = zlib.crc32(f.read(4096))
                f.seek(offset + size - 4096)
                last = zlib.crc32(f.read(4096))
            content = f'ContentCheck::Edges {{ first: 0x{first:08x}, last: 0x{last:08x} }}'
            total_read += 8192
        rows.append(f'    FileSpec {{ path: "{name}", bytes: {size}, content: {content} }},')
    def asset(name):
        path = assets / name
        return path if path.is_file() or base_assets is None else base_assets / name
    # A later quality rebuild must not silently reintroduce oversized RGBA
    # candidates while retaining the offline-budget manifest contract.
    check_story_rgba_budget(asset, manifest)
    if manifest.get('offline_ui_ranges'):
        # Prevent repacking tools from silently restoring the outer NX layer.
        from prepare_vita_ui_ranges import verify
        ui_names = ('allui.hed', 'allui.nam', 'allui.mrg')
        ui_root = base_assets if base_assets and not any((assets / name).is_file()
                                                        for name in ui_names) else assets
        verify(ui_root)
    return ('// Generated by tools/generate_vita_asset_profile_540.py; no game payloads.\n'
            f'// External manifest SHA-256: {hashlib.sha256(marker).hexdigest()}\n'
            f'// {len(rows)} key files, {total_read} bytes read once at startup, one 8192-byte buffer.\n'
            '// Large-file checks cover exact length and first/last 4096 bytes only.\n'
            'const PROFILE: &[FileSpec] = &[\n' + '\n'.join(rows) + '\n];\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--assets', type=Path, default=ASSETS)
    parser.add_argument('--base-assets', type=Path)
    args = parser.parse_args()
    generated = generate(args.assets, args.base_assets)
    if args.check:
        assert OUTPUT.read_text(encoding='utf-8') == generated, 'Embedded 540p asset profile is stale'
    else:
        with OUTPUT.open('w', encoding='utf-8', newline='\n') as output:
            output.write(generated)
    print(generated.splitlines()[2])

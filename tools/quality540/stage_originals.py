#!/usr/bin/env python3
"""Stage selected art from the user's extracted original RomFS, read-only."""
import argparse
import json
import struct
import zlib
from pathlib import Path

from bntx_image import decode
from rebuild_scene_art import ARCHIVES, json_write, read_entry, sha, unpack


def unpack_original(data):
    # Offline originals include seven textures just over 32 MiB. This PC-only
    # bound is independent of the unchanged runtime input/pixel limits.
    if data[:4] not in (b'NXCX', b'NXGX'):
        return data
    size, count = struct.unpack_from('<II', data, 4)
    assert 0 < size <= 128 * 1024 * 1024 and count + 16 == len(data)
    decoder = zlib.decompressobj(31 if data[:4] == b'NXGX' else 15)
    raw = decoder.decompress(data[16:], size + 1)
    assert len(raw) == size and decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail
    return raw


class RomFS:
    def __init__(self, root):
        self.root = root.resolve()

    def read_file(self, name, offset=0, size=None):
        path = (self.root / name).resolve()
        assert path.is_relative_to(self.root)
        with path.open('rb') as stream:
            assert 0 <= offset <= path.stat().st_size
            stream.seek(offset)
            return stream.read() if size is None else stream.read(size)


def names(data):
    assert len(data) % 32 == 0
    return [data[i:i + 32].split(b'\0')[0].split(b'\r')[0].decode()
            for i in range(0, len(data), 32)]


def stage(assets, output, selection, original):
    assert not output.exists(), 'stage must be a new directory'
    config = json.loads(selection.read_bytes())
    original_names = {a: names(original.read_file(a + '.nam')) for a in ARCHIVES}
    original_rows = {a: list(struct.iter_unpack('<IHH', original.read_file(a + '.hed'))) for a in ARCHIVES}
    output.mkdir(parents=True)
    images = []
    for archive in ARCHIVES:
        target_names = names((assets / (archive + '.nam')).read_bytes())
        rows = list(struct.iter_unpack('<IHH', (assets / (archive + '.hed')).read_bytes()))
        thumbnail_start = next((i for i, n in enumerate(target_names) if n.startswith('THUMSP_TOP.')), len(target_names))
        first = {}
        for i, name in enumerate(target_names[:thumbnail_start]):
            first.setdefault(name.split('.')[0], i)
        with (assets / (archive + '.mrg')).open('rb') as target:
            for i, name in enumerate(target_names[:thumbnail_start]):
                stem = name.split('.')[0]
                if i >= len(rows) or rows[i][0] == 0xffffffff or first[stem] != i:
                    continue
                if not any(stem == s or (config['include_variants'] and stem.startswith(s + '_')) for s in config['stems']):
                    continue
                source = 'jallpachd' if name in original_names['jallpachd'] else archive
                source_index = original_names[source].index(name)
                sector, count, _ = original_rows[source][source_index]
                raw = original.read_file(source + '.mrg', sector * 2048, count * 2048)
                if raw[:4] in (b'NXCX', b'NXGX'):
                    raw = raw[:16 + struct.unpack_from('<I', raw, 8)[0]]
                raw = unpack_original(raw)
                image = decode(raw)
                old = unpack(read_entry(target, rows[i]))
                assert old[:4] == b'KIMG'
                filename = f'{source}-{source_index:05d}-{stem}.raw'
                (output / filename).write_bytes(raw)
                images.append({'archive': archive, 'index': i, 'name': name, 'old_raw_sha256': sha(old),
                               'source_archive': source, 'source_index': source_index,
                               'file': filename, 'sha256': sha(raw), 'original_size': list(image.size)})
    result = {'source_manifest_sha256': sha((assets / 'vita-540p-assets.json').read_bytes()),
              'selection_sha256': sha(selection.read_bytes()), 'images': images}
    json_write(output / 'manifest.json', result)
    return result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', required=True, type=Path)
    parser.add_argument('--romfs', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--selection', type=Path, default=Path(__file__).with_name('scenes.json'))
    args = parser.parse_args()
    print('Staged images:', len(stage(args.assets, args.output, args.selection, RomFS(args.romfs))))

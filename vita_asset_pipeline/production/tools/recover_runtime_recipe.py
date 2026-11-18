"""Recover image production contracts from a prepared native DXT pack.

The output contains metadata, never image/audio payloads. It is not a substitute
for missing translated/custom source assets. Original ordinals are preserved.
"""
import argparse
import io
import json
from pathlib import Path
import struct
from PIL import Image, JpegImagePlugin

from resize_story_variants import Reader, unpack, sha
from vita_image_metadata import STORY_KEY


def image_contract(raw):
    if raw[:4] != b'KIMG':
        return {'magic': raw[:8].hex()}
    _, version, lw, lh, w, h, codec, length = struct.unpack_from('<4s7I', raw)
    if version != 1 or length + 32 > len(raw):
        raise ValueError('Invalid KIMG')
    result = dict(logical=[lw, lh], physical=[w, h], codec=codec)
    if codec in (2,7) and raw[32:40]==b'\x89PNG\r\n\x1a\n':
        result['png_color_type']=raw[57]
    if codec==1:
        with Image.open(io.BytesIO(raw[32:32+length])) as jpeg:
            result['jpeg_tables']=[jpeg.quantization[k] for k in sorted(jpeg.quantization)]
            result['jpeg_subsampling']=JpegImagePlugin.get_sampling(jpeg)
    if codec == 9:
        result['format'] = struct.unpack_from('<I', raw, 84)[0]
    return result


def recover(assets):
    marker = (assets / 'vita-540p-assets.json').read_bytes()
    manifest = json.loads(marker)
    result = dict(schema='knife-runtime-recipe-v1', source_manifest_sha256=sha(marker),
                  contains_game_payloads=False, archives={},
                  source_reproducible_release=False,
                  recovered_scope='output contracts only; source selection, resampling history '
                                  'and intermediate lossy encoding are not recovered')
    for base, count in manifest['story_gxt_variants']['original_entries'].items():
        reader = Reader(assets, base, manifest.get('story_archive_volumes', {}).get(base, 1))
        entries, variants = [], []
        try:
            for i in range(count):
                data = reader.read(i)
                raw = unpack(data)
                entries.append(dict(name=reader.name(i), **image_contract(raw),
                                    stored_sha256=sha(data), raw_storage=data[:4] == b'KIMG'))
            i = count
            while i < len(reader.rows) - 2:
                name = reader.name(i)
                if not STORY_KEY.fullmatch(name):
                    raise ValueError(f'Unrecognized variant: {base}:{i}:{name}')
                source = int(name[2:7], 16)
                if source >= count:
                    raise ValueError('Variant refers outside the original directory')
                raw = unpack(reader.read(i))
                key = dict(name=name, index=i, source_index=source,
                           source_name=reader.name(source), grade=int(name[7:9], 16),
                           language=int(name[9], 16), bright_bits=name[10:18],
                           contrast_bits=name[18:26])
                if raw[:4] == b'KQTL':
                    _, version, lw, lh, w, h, cols, rows = struct.unpack_from('<4s7I', raw)
                    if version != 1 or cols != (w + 503)//504 or rows != (h + 503)//504:
                        raise ValueError('Invalid grid dimensions')
                    descriptors = [struct.unpack_from('<5I', raw, 32 + 20*s)
                                   for s in range(cols*rows+1)]
                    if [d[0] for d in descriptors] != list(range(i+1, i+2+cols*rows)):
                        raise ValueError('Non-contiguous grid requires explicit support')
                    key.update(tiled=True, logical=[lw, lh], physical=[w, h],
                               fallback=image_contract(unpack(reader.read(i+1))),
                               fallback_stored_sha256=sha(reader.read(i+1)),
                               tile_count=cols*rows)
                    i += 2 + cols*rows
                else:
                    key.update(tiled=False, **image_contract(raw),
                               raw_storage=reader.read(i)[:4] == b'KIMG')
                    i += 1
                variants.append(key)
            if reader.name(i) != 'KQ_VARIANTS.NXZ':
                raise ValueError('Missing terminal marker')
            result['archives'][base] = dict(originals=entries, variants=variants)
            print(base, len(entries), 'originals;', len(variants), 'variants', flush=True)
        finally:
            reader.volume.__exit__(None, None, None)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = recover(args.assets)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open('x', encoding='utf8') as out:
        json.dump(result, out, ensure_ascii=False, separators=(',', ':'))
        out.write('\n')


if __name__ == '__main__':
    main()

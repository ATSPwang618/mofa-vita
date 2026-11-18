"""Build a separate, bounded-size story texture candidate; never edit the source pack.

Requires Pillow, numpy and ispc_texcomp (optionally supplied by --python-deps).
Only appended KQ variants change. Original ordinals, logical sizes, fallback bytes,
UI, scripts and audio are preserved. KQTL images are assembled before resizing,
then retiled with fresh guards. Run the production metadata validator afterward.
"""
from __future__ import annotations
import argparse
from collections import deque
from concurrent.futures import ProcessPoolExecutor
import hashlib
import importlib.util
import json
import math
from pathlib import Path
import re
import shutil
import struct
import sys
import time
import zlib

from vita_image_metadata import ArchiveVolumes, STORY_KEY, VOLUME_BYTES, volume_name

ROOT = Path(__file__).resolve().parents[1]


def sha(data):
    return hashlib.sha256(data).hexdigest()


def unpack(data):
    if data[:4] in (b'NXCX', b'NXGX'):
        length, packed = struct.unpack_from('<II', data, 4)
        result = zlib.decompress(data[16:16 + packed], 31 if data[:4] == b'NXGX' else 15)
        assert len(result) == length
        return result
    return data


def stored(raw, force_raw=False):
    compressed = zlib.compress(raw, 9) if not force_raw else raw
    result = (struct.pack('<4sIII', b'NXCX', len(raw), len(compressed), 0) + compressed
              if not force_raw and len(compressed) + 16 < len(raw) else raw)
    return result + bytes((-len(result)) % 2048), (len(raw) + 2047) // 2048


class Reader:
    def __init__(self, root, base, volumes):
        self.rows = list(struct.iter_unpack('<IHH', (root / (base + '.hed')).read_bytes()))
        self.names = (root / (base + '.nam')).read_bytes()
        self.volume = ArchiveVolumes(lambda n: root / n, base, volumes).__enter__()

    def name(self, i):
        return self.names[i*32:(i+1)*32].split(b'\0')[0].split(b'\r')[0].decode('ascii')

    def read(self, i):
        sector, count, _ = self.rows[i]
        self.volume.seek(sector * 2048)
        result = self.volume.read(count * 2048)
        assert len(result) == count * 2048
        return result


class Writer:
    def __init__(self, root, base, *, overwrite_incomplete=False):
        self.root, self.base = root, base
        self.write_mode = 'wb' if overwrite_incomplete else 'xb'
        self.position, self.volume_index = 0, 0
        self.file = (root / volume_name(base, 0)).open(self.write_mode)
        self.rows, self.names, self.dedup = [], bytearray(), {}

    def write(self, name, data, decoded):
        assert len(name.encode('ascii')) < 32 and len(data) % 2048 == 0
        digest = sha(data)
        if digest in self.dedup:
            offset, length = self.dedup[digest]
        else:
            offset, length = self.position // 2048, len(data) // 2048
            self.dedup[digest] = offset, length
            start = 0
            while start < len(data):
                if self.position and self.position % VOLUME_BYTES == 0:
                    self.file.close()
                    self.volume_index += 1
                    self.file = (self.root / volume_name(self.base, self.volume_index)).open(self.write_mode)
                count = min(len(data)-start, VOLUME_BYTES-self.position % VOLUME_BYTES)
                self.file.write(data[start:start+count])
                start += count
                self.position += count
        assert length < 65536 and decoded < 65536
        self.rows.append((offset, length, decoded))
        self.names.extend(name.encode('ascii').ljust(32, b'\0'))

    def finish(self):
        self.write('KQ_VARIANTS.NXZ', b'', 0)
        self.rows.append((0xffffffff, 0xffff, 0xffff))
        self.names.extend(bytes(32))
        self.file.close()
        (self.root / (self.base + '.hed')).write_bytes(b''.join(struct.pack('<IHH', *r) for r in self.rows))
        (self.root / (self.base + '.nam')).write_bytes(self.names)


def init_worker(root, base, volumes, deps):
    global reader, gxt, Image, np, encoder
    if deps:
        sys.path.insert(0, deps)
    import numpy as np
    from PIL import Image
    import ispc_texcomp as encoder
    spec = importlib.util.spec_from_file_location('gxt_codec', ROOT / 'tools/migrate-vita-native-color.py')
    gxt = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gxt)
    # Exact replay can pin the historical reciprocal instruction behavior.
    # Normal experimental conversions retain their previous default behavior.
    import os
    if os.environ.get('KNIFE_EXACT_REPLAY') == '1':
        import ctypes
        import ispc_texcomp._ispc_texcomp as module
        global reciprocal_adapter
        if sha(Path(module.__file__).read_bytes()) != '1e69c985ec6a499f541f1d403b1697d8255b6ea07d4aa60a6957e06593b9bf98':
            raise ValueError('Unsupported exact replay ISPC binary')
        reciprocal_adapter = ctypes.CDLL(str(ROOT/'tools/source_replay/reciprocal_compat.dll'))
        reciprocal_adapter.start.argtypes = [ctypes.c_void_p]
        if reciprocal_adapter.start(ctypes.WinDLL(module.__file__)._handle) != 282:
            raise ValueError('Exact replay reciprocal adapter failed')
        reciprocal_adapter.mode(0)
    reader = Reader(Path(root), base, volumes)


def decode(raw):
    magic, version, lw, lh, w, h, codec, length = struct.unpack_from('<4s7I', raw)
    assert magic == b'KIMG' and version == 1 and codec == 9
    return gxt.gxt_decode(raw[32:32+length])[0].crop((0, 0, w, h))


def encode(image, logical, force_raw=False, force_bc3=False):
    w, h = image.size
    bw, bh = max(4, gxt.pow2(w)), max(4, gxt.pow2(h))
    padded = np.pad(np.asarray(image), ((0, bh-h), (0, bw-w), (0, 0)), mode='edge')
    opaque = not force_bc3 and image.getchannel('A').getextrema() == (255, 255)
    bpe, fmt = (8, 0x85000000) if opaque else (16, 0x87000000)
    surface = encoder.RGBASurface(padded.tobytes(), bw, bh)
    # The BC1 binding returns an oversized allocation; trim to the format payload.
    linear = (encoder.compress_blocks_bc1(surface) if opaque else encoder.compress_blocks_bc3(surface))[:bw*bh*bpe//16]
    payload = gxt.swizzle(linear, bw//4, bh//4, bpe)
    assert len(payload) <= 2*1024*1024
    header = struct.pack('<8I', 0x00545847, 0x10000003, 1, 64, len(payload), 0, 0, 0)
    header += struct.pack('<6IHHB3s', 64, len(payload), 0xffffffff, 0, 0xa0000000, fmt, w, h, 1, bytes(3))
    raw = struct.pack('<4s7I', b'KIMG', 1, *logical, w, h, 9, len(header)+len(payload)) + header + payload
    return (*stored(raw, force_raw), (w, h, fmt, len(payload)))


def convert(task):
    index, scale, protected = task
    entry = reader.read(index)
    raw = unpack(entry)
    _, version, lw, lh, w, h, a, b = struct.unpack_from('<4s7I', raw)
    assert version == 1 and w*h <= 64*1024*1024
    tiled = raw[:4] == b'KQTL'
    descriptors = [struct.unpack_from('<5I', raw, 32+20*i) for i in range(a*b+1)] if tiled else []
    # Exact low-memory fallbacks and audited native hot entries remain byte-identical.
    keep = protected or (not tiled and entry[:4] == b'KIMG') or min(w, h) <= 16
    nw, nh = max(1, round(w*scale)), max(1, round(h*scale))
    if tiled and (nw < descriptors[0][1] or nh < descriptors[0][2]):
        keep = True
    if keep:
        nw, nh = w, h
    if tiled:
        fallback = descriptors[0]
        blocks = [(reader.read(fallback[0]), reader.rows[fallback[0]][2], fallback[1:])]
        if keep:
            blocks += [(reader.read(d[0]), reader.rows[d[0]][2], d[1:]) for d in descriptors[1:]]
        else:
            original = Image.new('RGBA', (w, h))
            for t, desc in enumerate(descriptors[1:]):
                tile = decode(unpack(reader.read(desc[0])))
                x, y = (t % a)*504, (t // a)*504
                gx, gy = max(0, x-4), max(0, y-4)
                original.paste(tile.crop((x-gx, y-gy, min(w, x+504)-gx, min(h, y+504)-gy)), (x, y))
            # Pillow RGBA resize uses premultiplied alpha to avoid dark fringes.
            resized = original.resize((nw, nh), Image.Resampling.LANCZOS)
            for y in range(0, nh, 504):
                for x in range(0, nw, 504):
                    tile = resized.crop((max(0, x-4), max(0, y-4), min(nw, x+508), min(nh, y+508)))
                    blocks.append(encode(tile, tile.size))
        return dict(index=index, logical=(lw, lh), before=(w, h), after=(nw, nh),
                    keep=keep, blocks=blocks, tiled=True)
    if keep:
        block = (entry, reader.rows[index][2], None)
    else:
        block = encode(decode(raw).resize((nw, nh), Image.Resampling.LANCZOS), (lw, lh))
    return dict(index=index, logical=(lw, lh), before=(w, h), after=(nw, nh),
                keep=keep, blocks=[block], tiled=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--scale', type=float, default=.8)
    parser.add_argument('--workers', type=int, default=4)
    parser.add_argument('--python-deps', type=Path)
    args = parser.parse_args()
    assert .5 <= args.scale < 1 and 1 <= args.workers <= 8
    assert args.source.resolve() != args.output.resolve() and not args.output.exists()
    args.output.mkdir(parents=True)
    marker = (args.source / 'vita-540p-assets.json').read_bytes()
    manifest = json.loads(marker)
    originals = manifest['story_gxt_variants']['original_entries']
    protected = set()
    for filename in ['vita_live_alpha_data.rs', 'vita_sparse_coverage.rs']:
        text = (ROOT / 'knife/knife-render/src' / filename).read_text('utf-8')
        protected.update(re.findall(r'entry: "(KQ[^\"]+)"', text))
    report = dict(source_manifest_sha256=sha(marker), scale=args.scale, archives={}, variants=[])
    start = time.monotonic()
    for base, original_count in originals.items():
        volumes = manifest.get('story_archive_volumes', {}).get(base, 1)
        source = Reader(args.source, base, volumes)
        writer = Writer(args.output, base)
        for i in range(original_count):
            writer.write(source.name(i), source.read(i), source.rows[i][2])
        # Preserve exact original name bytes, including terminator conventions.
        writer.names[:] = source.names[:original_count*32]
        tasks = []
        i = original_count
        while i < len(source.rows)-2:
            assert STORY_KEY.fullmatch(source.name(i)), (base, i, source.name(i))
            tasks.append((i, args.scale, base == 'jallpac' and source.name(i) in protected))
            data = unpack(source.read(i))
            i += (struct.unpack_from('<I', data, 24)[0]*struct.unpack_from('<I', data, 28)[0]+2) if data[:4] == b'KQTL' else 1
        print(base, 'variants', len(tasks), 'original entries', original_count, flush=True)
        with ProcessPoolExecutor(max_workers=args.workers, initializer=init_worker,
                                 initargs=(str(args.source), base, volumes, str(args.python_deps) if args.python_deps else None)) as pool:
            pending = deque()
            iterator = iter(tasks)
            for task in iterator:
                pending.append(pool.submit(convert, task))
                if len(pending) >= args.workers*2:
                    consume(pending.popleft().result(), source, writer, report, base)
            while pending:
                consume(pending.popleft().result(), source, writer, report, base)
        writer.finish()
        source.volume.__exit__(None, None, None)
        report['archives'][base] = dict(bytes=writer.position, volumes=writer.volume_index+1,
                                        entries=len(writer.rows)-1, variants=len(tasks))
        print(base, report['archives'][base], 'elapsed', round(time.monotonic()-start), flush=True)
        (args.output.parent / 'resize-report.json').write_text(json.dumps(report, indent=2), 'utf-8')
    # All unrelated resources are copied byte-for-byte, not transcoded.
    for source in args.source.rglob('*'):
        if not source.is_file():
            continue
        relative = source.relative_to(args.source)
        if relative.name == 'vita-540p-assets.json' or any(re.fullmatch(re.escape(base)+r'\.(hed|nam|mrg(?:\.\d{3})?)', str(relative)) for base in originals):
            continue
        destination = args.output / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
    for archive in manifest['archives']:
        if archive['name'] in report['archives']:
            archive['bytes'] = report['archives'][archive['name']]['bytes']
    manifest['story_archive_volumes'] = {k:v['volumes'] for k,v in report['archives'].items() if v['volumes'] > 1}
    manifest['story_storage_resize'] = {k:v for k,v in report.items() if k != 'variants'}
    changed = {p.name:p for p in args.output.iterdir() if p.is_file() and any(p.name.startswith(base+'.') for base in originals)}
    manifest['files'] = [r for r in manifest['files'] if not any(r['path'].startswith(base+'.') for base in originals)]
    for name, path in sorted(changed.items()):
        digest = hashlib.sha256()
        with path.open('rb') as f:
            for block in iter(lambda:f.read(1024*1024), b''):
                digest.update(block)
        manifest['files'].append(dict(path=name, bytes=path.stat().st_size, sha256=digest.hexdigest()))
    manifest['files'].sort(key=lambda r:r['path'])
    (args.output / 'vita-540p-assets.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2)+'\n', 'utf-8')
    print('DONE', args.output, 'seconds', round(time.monotonic()-start), flush=True)


def consume(result, source, writer, report, base):
    old = result.pop('index')
    blocks = result.pop('blocks')
    new = len(writer.rows)
    name = source.name(old)
    if result['tiled']:
        w, h = result['after']
        cols, rows = math.ceil(w/504), math.ceil(h/504)
        assert len(blocks) == cols*rows+1
        raw = struct.pack('<4s7I', b'KQTL', 1, *result['logical'], w, h, cols, rows)
        raw += b''.join(struct.pack('<5I', new+1+i, *block[2]) for i, block in enumerate(blocks))
        writer.write(name, *stored(raw, True))
        for slot, (data, decoded, _) in enumerate(blocks):
            writer.write(f'KQT{new:07X}{slot:03X}.NXZ', data, decoded)
    else:
        writer.write(name, blocks[0][0], blocks[0][1])
    report['variants'].append(dict(archive=base, name=name, old_index=old, new_index=new, **result))
    count = len(report['variants'])
    if count % 250 == 0:
        print('converted', count, base, name, 'output MiB', round(writer.position/1048576), flush=True)


if __name__ == '__main__':
    main()

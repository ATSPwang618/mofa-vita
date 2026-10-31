"""Read texture headers without extracting or modifying the game archives."""
import argparse
import os
import collections
import gzip
import hashlib
import json
import mmap
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / 'dist/Witch-on-the-Holy-Night-Windows-x64/assets'
MOD = Path(os.environ['KNIFE_TRANSLATION_ROMFS']).expanduser() if os.environ.get('KNIFE_TRANSLATION_ROMFS') else None


def inspect_archive(base):
    hed = (ASSETS / (base + '.hed')).read_bytes()
    nam = (ASSETS / (base + '.nam')).read_bytes()
    kinds, sizes, formats, mips = (collections.Counter() for _ in range(4))
    samples = []
    with (ASSETS / (base + '.mrg')).open('rb') as f, mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as mm:
        for i in range(len(hed) // 8):
            offset, length, _ = struct.unpack_from('<IHH', hed, i * 8)
            if offset == 0xffffffff:
                break
            name = nam[i*32:(i+1)*32].split(b'\0')[0].split(b'\r')[0].decode('utf-8')
            if name == 'NOFILE.EXISTS':
                kinds['missing'] += 1
                continue
            start = offset * 2048
            def header(limit):
                raw = mm[start:start + min(length * 2048, limit)]
                if raw[:4] in (b'NXGX', b'NXCX'):
                    compressed = struct.unpack_from('<I', raw, 8)[0]
                    dec = zlib.decompressobj(31 if raw[:4] == b'NXGX' else 15)
                    return dec.decompress(mm[start+16:start+16+compressed], limit)
                return raw
            head = header(65536)
            kind = head[:4].hex()
            if head[:4] == b'BNTX':
                kind = 'BNTX'
                count = struct.unpack_from('<I', head, 0x24)[0]
                ptr = struct.unpack_from('<Q', head, 0x28)[0]
                for j in range(count):
                    brti = struct.unpack_from('<Q', head, ptr+j*8)[0]
                    if brti + 0x80 > len(head):
                        head = header(brti + 0x80)
                    fmt = struct.unpack_from('<I', head, brti+0x1c)[0]
                    w, h = struct.unpack_from('<II', head, brti+0x24)
                    mip = struct.unpack_from('<H', head, brti+0x16)[0]
                    sizes[f'{w}x{h}'] += 1
                    formats[hex(fmt)] += 1
                    mips[mip] += 1
                    if len(samples) < 5:
                        samples.append({'name': name, 'width': w, 'height': h, 'format': hex(fmt), 'mips': mip})
            elif head[:3] == b'\xff\xd8\xff':
                kind = 'JPEG'
            elif head[:8] == b'\x89PNG\r\n\x1a\n':
                kind = 'PNG'
            kinds[kind] += 1
    return {'entries': sum(kinds.values()), 'bytes': (ASSETS / (base+'.mrg')).stat().st_size,
            'kinds': dict(kinds), 'dimensions': dict(sizes.most_common()), 'formats': dict(formats),
            'mips': dict(mips), 'samples': samples}


def text_slots(path):
    data = path.read_bytes()
    count = struct.unpack_from('<H', data, 6)[0]
    start = 8 + count*8
    entries = []
    for i in range(count):
        sector, offset, sectors, low = struct.unpack_from('<HHHH', data, 8+i*8)
        pos = start + (sector << 11) + (offset & 0xfff) + ((offset >> 12) << 27)
        size = low if sectors == 0 else ((sectors-1)//32)*65536 + low
        entries.append(data[pos:pos+size])
    slots = []
    for i in range(0, count, 2):
        offsets = struct.unpack('>'+'I'*(len(entries[i])//4), entries[i])
        text = entries[i+1]
        lines = [text[a:b].rstrip(b'\r\n\0') for a,b in zip(offsets, (*offsets[1:], len(text)))]
        slots.append(lines)
    return slots


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', type=Path, default=ASSETS)
    parser.add_argument('--mod', type=Path, default=MOD, required=MOD is None)
    args = parser.parse_args()
    ASSETS, MOD = args.assets, args.mod
    output = {'archives': {base: inspect_archive(base) for base in ['jallpac', 'jallpachd', 'allpacml', 'allpachdml', 'allui']}}
    base_text, mod_text = text_slots(ASSETS/'script_text.mrg'), text_slots(MOD/'script_text.mrg')
    output['translation_slots'] = [{'slot':i, 'base_lines':len(a), 'mod_lines':len(b),
                                     'changed_lines':sum(x!=y for x,y in zip(a,b))}
                                    for i,(a,b) in enumerate(zip(base_text, mod_text))]
    output['mod_files'] = [{'path':p.relative_to(MOD).as_posix(), 'bytes':p.stat().st_size,
                            'sha256':hashlib.file_digest(p.open('rb'),'sha256').hexdigest(),
                            'signature':p.read_bytes()[:4].hex()} for p in sorted(MOD.rglob('*')) if p.is_file()]
    path = ROOT/'doc/texture-inventory.json'
    path.write_text(json.dumps(output, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
    print(json.dumps({k:v for k,v in output.items() if k!='archives'},ensure_ascii=False,indent=2))
    for k,v in output['archives'].items():
        print(k, json.dumps({x:y for x,y in v.items() if x not in ('samples','dimensions')},ensure_ascii=False))
        print('  common dimensions',list(v['dimensions'].items())[:12])

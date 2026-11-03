"""Remove only the outer NX layer of the Vita gallery/library UI atlases.

KIMG names, order, offsets, PNG/DXT payloads, dimensions and alpha are byte exact.
The existing archive raw-entry reader can then seek to one member without a
whole-atlas allocation or repeated inflation. Output is staged separately and
bound to the normal asset manifest; this tool never overwrites its inputs.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import struct
import zlib

from vita_image_metadata import CAP, image_info, images_in, require

SECTOR = 2048
MANIFEST = 'vita-540p-assets.json'
FILES = ('allui.hed', 'allui.nam', 'allui.mrg')
BASES = ('CG_PARTS', 'ARCHIVE_PARTS', 'ARCHTHUMB_PARTS')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def selected(name):
    stem = name.rsplit('.', 1)[0].upper()
    return any(stem == base or stem.startswith(base + '_') for base in BASES)


def decode_entry(data):
    if data[:4] not in (b'NXCX', b'NXGX'):
        return data
    require(len(data) >= 16, 'truncated NX header')
    length, compressed, reserved = struct.unpack_from('<III', data, 4)
    require(0 < length <= CAP and compressed + 16 <= len(data), 'NX bounds')
    require(reserved == 0, 'NX reserved word')
    decoder = zlib.decompressobj(31 if data[:4] == b'NXGX' else 15)
    try:
        output = decoder.decompress(data[16:16+compressed], length+1)
    except zlib.error as error:
        raise ValueError('invalid NX stream') from error
    require(len(output) == length and decoder.eof and not decoder.unused_data
        and not decoder.unconsumed_tail, 'NX length or checksum')
    require(not any(data[16+compressed:]), 'nonzero NX sector padding')
    return output


def atlas_members(data):
    """Exact KIMG ranges, with only the archive's final zero sector pad allowed."""
    require(len(data) >= 12 and data[:4] == b'KATL', 'expected compact KATL atlas')
    version, count = struct.unpack_from('<II', data, 4)
    require(version == 1 and 0 < count <= 4096, 'KATL header')
    pos, members = 12, []
    for _ in range(count):
        require(pos+8 <= len(data), 'truncated KATL member')
        nl, il = struct.unpack_from('<II', data, pos)
        pos += 8
        require(0 < nl <= 1024 and 0 < il <= CAP and pos+nl+il <= len(data), 'KATL bounds')
        name = data[pos:pos+nl].decode('utf-8')
        pos += nl
        image = data[pos:pos+il]
        info = image_info(image)
        require(info['envelope_bytes'] == il, 'KIMG trailing bytes')
        members.append(dict(name=name, offset=pos, bytes=il, sha256=sha(image),
            logical=info['logical'], physical=info['physical'], codec=info['codec']))
        pos += il
    require(pos == len(data) or (len(data) == ((pos+SECTOR-1)//SECTOR)*SECTOR
        and not any(data[pos:])), 'KATL trailing bytes')
    images_in(data[:pos])
    return members, pos


def inputs(assets):
    raw_manifest = (assets/MANIFEST).read_bytes()
    manifest = json.loads(raw_manifest)
    records = {r['path']: r for r in manifest['files']}
    for name in FILES:
        data = (assets/name).read_bytes()
        row = records[name]
        require(len(data) == row['bytes'] and sha(data) == row['sha256'], f'manifest mismatch: {name}')
    hed = (assets/'allui.hed').read_bytes()
    require(len(hed) % 8 == 0, 'HED alignment')
    rows = list(struct.iter_unpack('<IHH', hed))
    terminal = next((i for i, row in enumerate(rows) if row[0] == 0xffffffff), None)
    require(terminal is not None, 'missing HED terminator')
    nam = (assets/'allui.nam').read_bytes()
    require(len(nam) >= terminal*32, 'truncated NAM')
    names = [nam[i*32:(i+1)*32].split(b'\0')[0].split(b'\r')[0].decode() for i in range(terminal)]
    return raw_manifest, manifest, hed, rows[:terminal], names


def verify(assets):
    _, _, _, rows, names = inputs(assets)
    checked, total, largest = [], 0, 0
    with (assets/'allui.mrg').open('rb') as source:
        for index, (row, name) in enumerate(zip(rows, names)):
            if not row[1] or not selected(name):
                continue
            source.seek(row[0]*SECTOR)
            raw = source.read(row[1]*SECTOR)
            require(len(raw) == row[1]*SECTOR and len(raw) <= CAP, 'raw UI extent')
            require(raw[:4] == b'KATL', f'{name} still compressed; run prepare_vita_ui_ranges.py')
            members, exact = atlas_members(raw)
            checked.append(dict(index=index, entry=name, bytes=len(raw), envelope_bytes=exact,
                members=len(members), member_digest=sha(json.dumps(members, sort_keys=True).encode())))
            total += len(members)
            largest = max(largest, max(r['bytes'] for r in members))
    require(checked, 'no gallery/library atlas found')
    return dict(entries=checked, members_verified=total, largest_member_bytes=largest,
        direct_reads=True, image_bytes_unchanged=True)


def prepare(assets, output):
    assets, output = assets.resolve(), output.resolve()
    require(not output.is_relative_to(assets) and not assets.is_relative_to(output), 'output must be separate')
    raw_manifest, manifest, hed, rows, names = inputs(assets)
    require(not output.exists(), 'output already exists')
    output.mkdir(parents=True)
    changed, relocated, new_hed, verified = [], {}, bytearray(), 0
    with (assets/'allui.mrg').open('rb') as source, (output/'allui.mrg').open('wb') as dest:
        for index, (row, name) in enumerate(zip(rows, names)):
            if not row[1]:
                new_hed += struct.pack('<IHH', *row)
                continue
            source.seek(row[0]*SECTOR)
            raw = source.read(row[1]*SECTOR)
            require(len(raw) == row[1]*SECTOR, f'truncated entry {index}')
            convert = selected(name)
            if convert:
                decoded = decode_entry(raw)
                members, exact = atlas_members(decoded)
                data = decoded[:exact]
                padded = data + bytes(-len(data) % SECTOR)
                require(len(padded) <= CAP, 'UI output exceeds archive limit')
                # Compare every member, not a sample or decoded pixel heuristic.
                after, _ = atlas_members(padded)
                require(after == members, f'KIMG bytes changed: {name}')
                verified += len(members)
                unpacked = len(padded)//SECTOR
                if raw != padded:
                    changed.append(dict(index=index, entry=name, before_bytes=len(raw),
                        after_bytes=len(padded), members=len(members), image_bytes_unchanged=True))
            else:
                padded, unpacked = raw, row[2]
            key = (sha(padded), unpacked)
            if key not in relocated:
                require(len(padded)//SECTOR <= 0xffff, 'HED size overflow')
                relocated[key] = (dest.tell()//SECTOR, len(padded)//SECTOR, unpacked)
                dest.write(padded)
            new_hed += struct.pack('<IHH', *relocated[key])
        new_hed += hed[len(rows)*8:]
    (output/'allui.hed').write_bytes(new_hed)
    (output/'allui.nam').write_bytes((assets/'allui.nam').read_bytes())
    for row in manifest['files']:
        if row['path'] in FILES:
            data = (output/row['path']).read_bytes()
            row.update(bytes=len(data), sha256=sha(data))
    for row in manifest['archives']:
        if row['name'] == 'allui':
            row['bytes'] = (output/'allui.mrg').stat().st_size
    manifest['statistics']['bytes'] = sum(row['bytes'] for row in manifest['files'])
    if changed:
        manifest['offline_ui_ranges'] = dict(schema='knife-ui-direct-ranges-v1',
            bases=list(BASES), source_manifest_sha256=sha(raw_manifest),
            image_bytes_unchanged=True, storage='raw-sector-katl', whole_atlas_preload=False)
    # Preserve exact manifest bytes on an already prepared pack.
    if not changed and manifest == json.loads(raw_manifest):
        (output/MANIFEST).write_bytes(raw_manifest)
    else:
        (output/MANIFEST).write_text(json.dumps(manifest, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
    result = verify(output)
    result.update(schema='knife-ui-direct-ranges-v1', source_manifest_sha256=sha(raw_manifest),
        manifest_sha256=sha((output/MANIFEST).read_bytes()), changed_entries=changed,
        members_verified=verified, image_bytes_unchanged=True)
    (output/'ui-range-report.json').write_text(json.dumps(result, ensure_ascii=False, indent=2)+'\n', encoding='utf-8')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--assets', required=True, type=Path)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    if args.check:
        result = verify(args.assets)
    else:
        parser.error('--output is required for staging') if args.output is None else None
        result = prepare(args.assets, args.output)
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()

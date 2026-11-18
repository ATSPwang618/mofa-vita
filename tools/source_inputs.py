"""Resolve verified upstream files before any texture conversion.

RomFS must already be BASE plus the supported update. MOD and custom logo are
explicit inputs, never searched for in a finished pack. Unresolved image/UI
recovery inputs remain explicit and are not relabelled as retail content.
"""
import hashlib
import json
from pathlib import Path

from slim_languages import unpack_mzp, pack_mzp


def checked(data, record, label):
    if len(data) != record['bytes'] or hashlib.sha256(data).hexdigest() != record['sha256']:
        raise ValueError('Source transformation differs from locked output: '+label)
    return data


def resolve(romfs, mod, logo, kit):
    result = {}
    for rel, record in kit['files'].items():
        if rel.startswith('stream/'):
            continue
        if rel == 'script_text.mrg':
            data = (mod/rel).read_bytes()
            blocks = unpack_mzp(data)
            if len(blocks) != 10:
                raise ValueError('Unsupported translation script layout')
            result[rel] = (checked(pack_mzp(blocks[2:6]), record, rel), 'MOD/en-zc-script')
        elif rel == 'movie/logo.mp4':
            result[rel] = (checked(logo.read_bytes(), record, rel), 'explicit-custom-logo')
        elif rel == 'zc/arshanghaisonggbpro_lt.otf':
            result[rel] = (checked((mod/rel).read_bytes(), record, rel), 'MOD/font')
        elif rel == 'allscr.mrg' or rel.startswith('shader/') or record['source'] == 'romfs':
            result[rel] = (checked((romfs/rel).read_bytes(), record, rel), 'patched-romfs')
    return result


def apply_overrides(romfs, mod, logo, kit_path, output):
    kit = json.loads((kit_path/'kit.json').read_bytes())
    resolved = resolve(romfs, mod, logo, kit)
    for rel, (data, origin) in resolved.items():
        target = output/rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
    return {rel: dict(origin=origin, bytes=len(data), sha256=hashlib.sha256(data).hexdigest())
            for rel, (data, origin) in resolved.items()}

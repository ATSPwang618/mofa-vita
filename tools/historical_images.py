"""Replay recorded 2026-11 quality refresh and subsequent memory-budget resize.

This stage must use Pillow BC decode, as the historical converter did. Source
PNG export uses a different decode contract; do not silently share those paths.
Every reconstructed entry is checked against its existing locked hash.
"""
import gzip
import hashlib
import json
from PIL import Image


def load(path):
    records = json.loads(gzip.decompress(path.read_bytes()))['images']
    return {(r['archive'],r['index']):r for r in records}


def rebuild(base, index, contract, history, source_at, encode_image, texture):
    record = history.get((base,index))
    if record is None:
        return None
    image = source_at(record['source_archive'],record['source_index'])
    image = image.resize(tuple(record['physical']),Image.Resampling.LANCZOS)
    stage, _, _ = texture.encode(image,record['logical'])
    raw = texture.unpack(stage)
    if hashlib.sha256(raw).hexdigest() != record['raw_sha256']:
        raise ValueError(f'Historical first DXT encoding mismatch: {base}:{index}')
    if list(record['physical']) != contract['physical']:
        image = texture.decode(raw).resize(tuple(contract['physical']),Image.Resampling.LANCZOS)
        stage, decoded, _ = encode_image(image,contract,contract['raw_storage'])
    else:
        stage, decoded = texture.stored(raw,contract['raw_storage'])
    if hashlib.sha256(stage).hexdigest() != contract['stored_sha256']:
        raise ValueError(f'Historical final entry mismatch: {base}:{index}')
    return contract['name'],stage,decoded

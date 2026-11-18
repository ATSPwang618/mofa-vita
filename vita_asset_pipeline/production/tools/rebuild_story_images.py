"""Reconstruct story pixels from original RomFS plus recovered metadata/LUTs.

No fallback to reference-pack pixels. Callers must report source identities and
validate their choice of resize/grade order against the reference build.
"""
import importlib.util
from pathlib import Path
import struct

import numpy as np
from PIL import Image

from resize_story_variants import Reader, unpack

_spec = importlib.util.spec_from_file_location('original_image_export', Path(__file__).with_name('export-original-switch-png.py'))
original = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(original)


def read_luts(path):
    data = path.read_bytes()
    result, pos = {}, 0
    while pos < len(data):
        magic, grade, bright, contrast, grey, count = struct.unpack_from('<4s5I', data, pos)
        if magic != b'KGL1' or count != (65281 if grey else 256):
            raise ValueError('Invalid production LUT stream')
        pos += 24
        result[(grade, bright, contrast)] = bool(grey), np.frombuffer(data[pos:pos+count*3], dtype=np.uint8).reshape(count,3)
        pos += count*3
    if pos != len(data):
        raise ValueError('Truncated LUT')
    return result


def apply_lut(image, key, luts):
    grey, lut = luts[(key['grade'], int(key['bright_bits'],16), int(key['contrast_bits'],16))]
    rgba = np.asarray(image).copy()
    if grey:
        rgb = rgba[:,:,:3].astype(np.uint32)
        index = rgb[:,:,0]*54 + rgb[:,:,1]*183 + rgb[:,:,2]*19
        rgba[:,:,:3] = lut[index]
    else:
        for channel in range(3):
            rgba[:,:,channel] = lut[rgba[:,:,channel],channel]
    return Image.fromarray(rgba)


class Originals:
    def __init__(self, romfs):
        self.root = romfs
        self.readers = {}
        self.names = {}
        for base in ['jallpac','jallpachd','allpacml','allpachdml']:
            r = Reader(romfs, base, 1)
            self.readers[base] = r
            mapping = {}
            for i, row in enumerate(r.rows):
                if row[0] == 0xffffffff:
                    break
                name = r.name(i).rsplit('.',1)[0]
                mapping.setdefault(name,i)
            self.names[base] = mapping

    def close(self):
        for r in self.readers.values():
            r.volume.__exit__(None,None,None)

    def image(self, base, key, prefer_hd=True):
        stem = key['source_name'].rsplit('.',1)[0]
        # Repeated names after THUMSP_TOP refer to genuinely different pictures.
        # Never promote those ordinals to the first large/HD image with that name.
        thumbnail = key['source_index'] >= original.thumbnail_start(self.root,base)
        hd = {'jallpac':'jallpachd','allpacml':'allpachdml'}.get(base,base)
        source = hd if prefer_hd and not thumbnail and stem in self.names[hd] else base
        if stem not in self.names[source]:
            raise ValueError(f'No original source for {base}:{stem}')
        index = key['source_index'] if thumbnail else self.names[source][stem]
        raw = self.readers[source].read(index)
        textures = [(image,fmt) for _,leaf in original.leaves(raw)
                    for _,_,image,fmt in original.textures(leaf)]
        if len(textures) != 1:
            raise ValueError(f'Expected single story image: {source}:{index}; got {len(textures)}')
        image,fmt = textures[0]
        if key['language']:
            # Original Switch strips are ja,en,zc,zt. Recovered keys use 1=en,2=zc.
            slot = key['language']
            if slot not in (1,2):
                raise ValueError('Unexpected language key')
            y = image.height*slot//4
            image = image.crop((0,y,image.width,y+image.height//4))
        return image, dict(archive=source,index=index,format=fmt,physical=list(image.size))


def rebuild(originals, base, key, luts, *, associated_alpha=True, grade_first=False, prefer_hd=True):
    image, source = originals.image(base,key,prefer_hd)
    if grade_first:
        image = apply_lut(image,key,luts)
    size = tuple(key['physical'])
    image = original.resize(image,size) if associated_alpha else image.resize(size,Image.Resampling.LANCZOS)
    if not grade_first:
        image = apply_lut(image,key,luts)
    return image,source

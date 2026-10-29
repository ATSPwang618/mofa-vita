"""Offline RGB check for Vita3K captures. Nonblack pixels do not prove correctness."""
import hashlib
import io
import json
import sys
from pathlib import Path

from PIL import Image, ImageChops


def inspect_frame(frame):
    path = Path(frame['path'])
    data = path.read_bytes()
    sha256 = hashlib.sha256(data).hexdigest()
    if frame.get('expected_sha256') not in (None, sha256):
        raise ValueError(f'Screenshot identity changed: {path}')
    with Image.open(io.BytesIO(data)) as source:
        rgb = source.convert('RGB')
    red, green, blue = rgb.split()
    maximum = ImageChops.lighter(ImageChops.lighter(red, green), blue)
    total = rgb.width * rgb.height
    nonblack = total - maximum.histogram()[0]
    return dict(label=frame['label'], path=str(path), bytes=len(data), sha256=sha256,
                width=rgb.width, height=rgb.height,
                expected_resolution=rgb.size == (960, 544),
                rgb_extrema=rgb.getextrema(), non_black_rgb_pixels=nonblack,
                total_pixels=total, all_rgb_black=nonblack == 0)


def inspect_captures(frames):
    rows = [inspect_frame(frame) for frame in frames]
    required = ('story', 'menu', 'return')
    valid = True
    for label in required:
        selected = [row for row in rows if row['label'] == label]
        if len(selected) != 1:
            valid = False
        elif selected[0]['all_rgb_black'] or not selected[0]['expected_resolution']:
            valid = False
    return dict(schema='knife-vita-capture-rgb-v1', frames=rows,
                required_labels=required, required_views_nonblack=valid,
                visual_correctness_verified=False,
                limitation='Rejects all-black or missing/wrong-sized required captures; '
                           'nonblack pixels alone do not establish a correct game image. '
                           'Startup may legitimately be black. This is an offline host-image check.')


if __name__ == '__main__':
    # Node sends UTF-8 bytes; Windows redirected stdin may otherwise use cp936,
    # which can consume a neighboring JSON backslash as a multibyte trail byte.
    print(json.dumps(inspect_captures(json.loads(sys.stdin.buffer.read().decode('utf-8'))),
                     ensure_ascii=True))

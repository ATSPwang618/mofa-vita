"""Prevent a submitted but black framebuffer from passing a visual smoke check."""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from PIL import Image

import vita_capture_check as check


class CaptureCheck(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def frame(self, name, color, size=(960, 544)):
        filename = self.root / f'{name}.png'
        Image.new('RGBA', size, color).save(filename)
        return {'label': name, 'path': str(filename)}

    def test_opaque_black_is_not_visible_evidence(self):
        result = check.inspect_frame(self.frame('story', (0, 0, 0, 255)))
        self.assertTrue(result['all_rgb_black'])
        self.assertEqual(result['non_black_rgb_pixels'], 0)

    def test_alpha_differences_do_not_make_black_visible(self):
        frame = self.frame('story', (0, 0, 0, 0))
        with Image.open(frame['path']) as source:
            image = source.copy()
        image.putpixel((0, 0), (0, 0, 0, 255))
        image.save(frame['path'])
        self.assertTrue(check.inspect_frame(frame)['all_rgb_black'])

    def test_low_blue_channel_counts_without_luma_rounding(self):
        frame = self.frame('story', (0, 0, 0, 255))
        with Image.open(frame['path']) as source:
            image = source.copy()
        image.putpixel((20, 30), (0, 0, 1, 255))
        image.save(frame['path'])
        self.assertEqual(check.inspect_frame(frame)['non_black_rgb_pixels'], 1)

    def test_black_startup_is_allowed_but_required_views_must_be_nonblack(self):
        frames = [self.frame('startup', (0, 0, 0, 255))]
        frames += [self.frame(label, (90, 80, 70, 255))
                   for label in ('story', 'menu', 'return')]
        self.assertTrue(check.inspect_captures(frames)['required_views_nonblack'])
        frames[-1] = self.frame('return', (0, 0, 0, 255))
        self.assertFalse(check.inspect_captures(frames)['required_views_nonblack'])

    def test_missing_duplicate_or_wrong_sized_views_fail_closed(self):
        frames = [self.frame(label, (20, 30, 40, 255))
                  for label in ('story', 'menu', 'return')]
        self.assertFalse(check.inspect_captures(frames[:-1])['required_views_nonblack'])
        self.assertFalse(check.inspect_captures(frames + [frames[0]])['required_views_nonblack'])
        frames[0] = self.frame('story', (20, 30, 40, 255), (1, 1))
        self.assertFalse(check.inspect_captures(frames)['required_views_nonblack'])

    def test_changed_screenshot_is_rejected(self):
        frame = self.frame('story', (20, 30, 40, 255))
        frame['expected_sha256'] = '0' * 64
        with self.assertRaises(ValueError):
            check.inspect_frame(frame)

    def test_utf8_pipe_preserves_chinese_paths_under_windows_locale(self):
        chinese = self.root / '魔法使之夜'
        chinese.mkdir()
        filename = chinese / 'story.png'
        Image.new('RGB', (960, 544), (90, 80, 70)).save(filename)
        frames = [dict(label=label, path=str(filename))
                  for label in ('story', 'menu', 'return')]
        result = subprocess.run([sys.executable, str(Path(check.__file__))],
            input=json.dumps(frames, ensure_ascii=False).encode('utf-8'),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env={**os.environ, 'PYTHONIOENCODING': 'cp936'}, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr.decode('cp936', errors='replace'))
        self.assertTrue(json.loads(result.stdout)['required_views_nonblack'])


if __name__ == '__main__':
    unittest.main()

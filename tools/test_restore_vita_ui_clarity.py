"""Verify selective UI recovery uses retained pixels and bounded native density."""
import io
import struct
import unittest

from PIL import Image

from test_create_270p import kimg, pixels
from optimize_compact import textures
import restore_vita_ui_clarity as ui


class SelectiveUI(unittest.TestCase):
    def test_native_density_is_bounded_and_never_degrades_existing_pixels(self):
        self.assertEqual(ui.target_size([1648, 384], [960, 224], [480, 112], 1), (824, 192))
        self.assertEqual(ui.target_size([20, 20], [100, 60], [50, 30], 1), (50, 30))
        self.assertEqual(ui.target_size([1920, 1080], [960, 540], [480, 270], 1), (960, 540))
        with self.assertRaises(ValueError):
            ui.target_size([3840, 2160], [1920, 1080], [960, 540], 1)

    def test_recovery_uses_baseline_pixels_not_an_upscale_of_270p(self):
        source = Image.new('RGBA', (64, 32), (240, 30, 60, 255))
        source.paste((20, 240, 30, 128), (32, 0, 64, 32))
        before = kimg(source, 2, (128, 64))
        current = kimg(Image.new('RGBA', (32, 16), 'black'), 2, (128, 64))
        result, rows = ui.restore_portable(before, current, [{'strips': 1}])
        self.assertEqual(result, before)
        self.assertEqual(pixels(result).getpixel((40, 4)), (20, 240, 30, 128))
        self.assertTrue(rows[0]['restored'])
        self.assertEqual(rows[0]['previous_physical'], [32, 16])

    def test_small_ui_is_byte_identical_when_existing_density_is_enough(self):
        before = kimg(Image.new('RGB', (100, 60), '#153090'), 1, (20, 20))
        current = kimg(Image.new('RGB', (50, 30), '#2380a0'), 1, (20, 20))
        result, rows = ui.restore_portable(before, current, [{'strips': 1}])
        self.assertEqual(result, current)
        self.assertFalse(rows[0]['restored'])

    def test_separate_language_strips_and_alpha_survive(self):
        source = Image.new('RGBA', (12, 14), (255, 0, 0, 255))
        source.paste((0, 0, 255, 128), (0, 7, 12, 14))
        before = kimg(source, 2, (24, 20))
        current = kimg(source.resize((6, 8)), 2, (24, 20))
        result, rows = ui.restore_portable(before, current, [{'strips': 2}])
        out = pixels(result)
        self.assertEqual(out.size, (12, 10))
        self.assertEqual(out.getpixel((6, 4)), (255, 0, 0, 255))
        self.assertEqual(out.getpixel((6, 5)), (0, 0, 255, 128))
        self.assertNotEqual(Image.open(io.BytesIO(result[32:])).mode, 'P')

    def test_atlas_mapping_and_logical_dimensions_are_preserved(self):
        def atlas(values):
            out = bytearray(struct.pack('<4sII', b'KATL', 1, len(values)))
            for name, data in values:
                label = name.encode('utf-8')
                out += struct.pack('<II', len(label), len(data)) + label + data
            return bytes(out)
        source = kimg(Image.new('RGBA', (32, 16)), 2, (64, 32))
        small = kimg(Image.new('RGBA', (16, 8)), 2, (64, 32))
        out, rows = ui.restore_portable(atlas([('中', source)]), atlas([('中', small)]), [{'strips': 1}])
        self.assertEqual(textures(out), [('中', source)])
        self.assertEqual(rows[0]['logical'], [64, 32])
        with self.assertRaises(ValueError):
            ui.restore_portable(atlas([('a', source)]), atlas([('b', small)]), [{'strips': 1}])

    def test_mismatched_codec_or_logical_metadata_fails(self):
        before = kimg(Image.new('RGB', (32, 16)), 1, (64, 32))
        for current in [kimg(Image.new('RGB', (16, 8)), 2, (64, 32)),
                        kimg(Image.new('RGB', (16, 8)), 1, (66, 32))]:
            with self.assertRaises(ValueError):
                ui.restore_portable(before, current, [{'strips': 1}])


if __name__ == '__main__':
    unittest.main()

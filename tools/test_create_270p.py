"""The 270p profile changes physical pixels while keeping script coordinates."""
import io
import struct
import unittest

from PIL import Image

import create_270p as profile
from optimize_compact import textures


def kimg(image, codec=2, logical=(1920, 1080)):
    stream = io.BytesIO()
    image.save(stream, 'JPEG' if codec == 1 else 'PNG')
    payload = stream.getvalue()
    return struct.pack('<4s7I', b'KIMG', 1, *logical, *image.size, codec, len(payload)) + payload


def pixels(value):
    return Image.open(io.BytesIO(value[32:])).convert('RGBA')


class QuarterPixels(unittest.TestCase):
    def test_fullscreen_pixels_shrink_and_logical_coordinates_survive(self):
        original = kimg(Image.new('RGB', (960, 540), '#8090a0'), 1)
        converted, rows = profile.convert_portable(original, {'strips': 1})
        self.assertEqual(struct.unpack_from('<4s7I', converted)[:7],
                         (b'KIMG', 1, 1920, 1080, 480, 270, 1))
        self.assertEqual(pixels(converted).size, (480, 270))
        self.assertEqual(rows[0]['codec'], 1)

    def test_small_odd_split_image_keeps_alpha_and_never_becomes_jpeg(self):
        image = Image.new('RGBA', (65, 33), (250, 20, 60, 0))
        image.paste((0, 255, 0, 200), (20, 4, 60, 30))
        converted, _ = profile.convert_portable(kimg(image, 2, (130, 66)), {'strips': 1})
        result = pixels(converted)
        self.assertEqual(result.size, (33, 17))
        self.assertEqual(struct.unpack_from('<I', converted, 24)[0], 2)
        self.assertEqual(result.getpixel((0, 0))[3], 0)
        self.assertEqual(result.getpixel((20, 10)), (0, 255, 0, 200))

    def test_two_language_strips_are_resized_separately(self):
        image = Image.new('RGBA', (12, 14), (255, 0, 0, 255))
        image.paste((0, 0, 255, 128), (0, 7, 12, 14))
        converted, rows = profile.convert_portable(kimg(image, 2, (24, 28)), {'strips': 2})
        result = pixels(converted)
        self.assertEqual(result.size, (6, 8))
        self.assertEqual(result.getpixel((3, 3)), (255, 0, 0, 255))
        self.assertEqual(result.getpixel((3, 4)), (0, 0, 255, 128))
        self.assertEqual(rows[0]['strips'], 2)

    def test_linear_mask_stays_linear_and_grayscale_is_not_palettized(self):
        image = Image.new('L', (960, 540), 137)
        converted, _ = profile.convert_portable(kimg(image, 7), {'strips': 1})
        self.assertEqual(struct.unpack_from('<I', converted, 24)[0], 7)
        self.assertEqual(pixels(converted).getpixel((40, 40)), (137, 137, 137, 255))
        self.assertNotEqual(Image.open(io.BytesIO(converted[32:])).mode, 'P')

    def test_atlas_labels_order_and_member_coordinates_survive(self):
        members = [('one', kimg(Image.new('RGBA', (8, 6)), 2, (16, 12))),
                   ('two', kimg(Image.new('RGB', (6, 4)), 1, (12, 8)))]
        atlas = bytearray(struct.pack('<4sII', b'KATL', 1, 2))
        for label, data in members:
            label = label.encode()
            atlas.extend(struct.pack('<II', len(label), len(data)) + label + data)
        output, rows = profile.convert_portable(bytes(atlas), {'textures': [{'strips': 1}, {'strips': 1}]})
        self.assertEqual([name for name, _ in textures(output)], ['one', 'two'])
        self.assertEqual([pixels(data).size for _, data in textures(output)], [(4, 3), (3, 2)])
        self.assertEqual([r['logical'] for r in rows], [[16, 12], [12, 8]])

    def test_unknown_codec_and_bad_strip_count_fail_explicitly(self):
        image = kimg(Image.new('RGBA', (10, 5)))
        with self.assertRaises(ValueError):
            profile.convert_portable(image, {'strips': 2})
        image = image[:24] + struct.pack('<I', 8) + image[28:]
        with self.assertRaises(ValueError):
            profile.convert_portable(image, {'strips': 1})


if __name__ == '__main__':
    unittest.main()

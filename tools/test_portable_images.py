"""Check the resource contract needed by the later Vita renderer."""
import io
import struct
import unittest
from PIL import Image
from compact_media import encode_portable


class PortableImages(unittest.TestCase):
    def decode(self, encoded):
        header = struct.unpack_from('<4s7I', encoded)
        image = Image.open(io.BytesIO(encoded[32:]))
        image.load()
        self.assertEqual(header[-1], len(encoded) - 32)
        self.assertEqual(tuple(header[4:6]), image.size)
        return header, image

    def test_opaque_photo_is_baseline_jpeg_at_actual_540p(self):
        data, info = encode_portable(Image.new('RGBA', (1920, 1080), (80, 100, 120, 255)), (1920, 1080))
        header, image = self.decode(data)
        self.assertEqual((header[6], image.format, image.size), (1, 'JPEG', (960, 540)))
        self.assertFalse(image.info.get('progressive', False))
        self.assertFalse(info['has_alpha'])

    def test_semitransparent_pixels_are_lossless_png(self):
        original = Image.new('RGBA', (4, 3), (20, 80, 140, 127))
        original.putpixel((0, 0), (20, 40, 60, 0))
        data, info = encode_portable(original, (4, 3))
        header, image = self.decode(data)
        self.assertEqual((header[6], image.format), (2, 'PNG'))
        self.assertEqual(image.convert('RGBA').tobytes(), original.tobytes())
        self.assertTrue(info['has_alpha'])

    def test_opaque_ui_and_linear_masks_stay_png(self):
        original = Image.new('RGBA', (16, 16), (61, 23, 182, 255))
        for srgb, code in [(True, 2), (False, 7)]:
            data, _ = encode_portable(original, (16, 16), srgb=srgb, force_png=srgb)
            header, image = self.decode(data)
            self.assertEqual((header[6], image.format), (code, 'PNG'))
            self.assertEqual(image.convert('RGBA').tobytes(), original.tobytes())

    def test_language_strips_resize_independently(self):
        original = Image.new('RGBA', (1920, 4320), (0, 0, 0, 255))
        colors = [(255,0,0,255),(0,255,0,255),(0,0,255,255),(255,255,255,255)]
        for i, color in enumerate(colors):
            original.paste(color, (0, i*1080, 1920, (i+1)*1080))
        data, info = encode_portable(original, (1920, 4320), strips=4, force_png=True)
        _, image = self.decode(data)
        self.assertEqual(image.size, (960, 2160))
        self.assertEqual(info['strips'], 4)
        for i, color in enumerate(colors):
            self.assertEqual(image.convert('RGBA').getpixel((480, i*540)), color)


if __name__ == '__main__':
    unittest.main()

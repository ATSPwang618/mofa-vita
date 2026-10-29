"""Regression cases for retaining the original English/Chengkong language IDs."""
import io
import struct
import unittest

from PIL import Image

from slim_languages import slim_kimg, slim_sysmes, pack_mzp, unpack_mzp


class BilingualAssetsTests(unittest.TestCase):
    def test_stacked_png_keeps_exact_english_and_chengkong_pixels(self):
        image = Image.new('RGBA', (8, 12))
        for language in range(4):
            image.paste((language * 60, 12, 90, 37 + language), (0, language * 3, 8, language * 3 + 3))
        out = io.BytesIO()
        image.save(out, format='PNG')
        source = struct.pack('<4s7I', b'KIMG', 1, 16, 48, 8, 12, 2, len(out.getvalue())) + out.getvalue()
        result = slim_kimg(source)
        self.assertEqual(struct.unpack('<4s7I', result[:32]), (b'KIMG', 1, 16, 24, 8, 6, 2, len(result) - 32))
        with Image.open(io.BytesIO(result[32:])) as reduced:
            self.assertEqual(reduced.convert('RGBA').tobytes(), image.crop((0, 3, 8, 9)).tobytes())

    def test_sysmes_contains_only_two_languages_and_preserves_strings(self):
        strings = [['ja', '日本語'], ['en', 'English'], ['zc', '澄空中文'], ['zt', '繁體中文']]
        header = bytearray(struct.pack('<IIQ', 4, 2, 0) + bytes(4 * 8 + 4 * 2 * 8))
        for language, rows in enumerate(strings):
            table = 16 + 4 * 8 + language * 2 * 8
            struct.pack_into('<Q', header, 16 + language * 8, table)
            for i, text in enumerate(rows):
                struct.pack_into('<Q', header, table + i * 8, len(header))
                header.extend(text.encode() + bytes([0]))
        result = slim_sysmes(bytes(header))
        self.assertEqual(struct.unpack_from('<II', result), (2, 2))
        self.assertNotIn('繁體中文'.encode(), result)
        for index, expected in enumerate(strings[1:3]):
            table = struct.unpack_from('<Q', result, 16 + index * 8)[0]
            actual = []
            for i in range(2):
                offset = struct.unpack_from('<Q', result, table + i * 8)[0]
                actual.append(result[offset:result.index(0, offset)].decode())
            self.assertEqual(actual, expected)

    def test_mzp_roundtrip_preserves_selected_text_pairs(self):
        blocks = [b'offset-en', b'English story' * 8000, b'offset-zc', '澄空中文'.encode()]
        self.assertEqual(unpack_mzp(pack_mzp(blocks)), blocks)


if __name__ == '__main__':
    unittest.main()

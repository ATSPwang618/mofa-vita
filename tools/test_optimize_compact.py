"""Regression tests for pixel-safe PNG optimization and archive aliasing."""
import importlib.util
import io
from pathlib import Path
import struct
import tempfile
import unittest

from PIL import Image


class LosslessTests(unittest.TestCase):
    def subject(self):
        spec = importlib.util.find_spec('optimize_compact')
        self.assertIsNotNone(spec, 'Lossless optimizer is not implemented yet')
        import optimize_compact
        return optimize_compact

    def png(self, pixels):
        im = Image.new('RGBA', (len(pixels), 1))
        im.putdata(pixels)
        out = io.BytesIO()
        im.save(out, 'PNG', compress_level=0)
        return out.getvalue()

    def test_transparent_rgb_and_fractional_alpha_are_preserved(self):
        m = self.subject()
        pixels = [(17, 29, 43, 0), (19, 20, 21, 127), (255, 2, 3, 255)] * 100
        before = self.png(pixels)
        after = m.optimize_png(before)
        self.assertLessEqual(len(after), len(before))
        with Image.open(io.BytesIO(after)) as im:
            self.assertEqual(list(im.convert('RGBA').getdata()), pixels)

    def test_gray_alpha_reduction_keeps_exact_rgba_values(self):
        m = self.subject()
        pixels = [(n, n, n, n) for n in range(256)] * 5
        after = m.optimize_png(self.png(pixels))
        with Image.open(io.BytesIO(after)) as im:
            self.assertEqual(list(im.convert('RGBA').getdata()), pixels)

    def test_jpeg_payload_and_logical_metrics_are_byte_identical(self):
        m = self.subject()
        out = io.BytesIO()
        Image.new('RGB', (16, 9), (40, 80, 120)).save(out, 'JPEG')
        payload = out.getvalue()
        value = struct.pack('<4s7I', b'KIMG', 1, 1920, 1080, 16, 9, 1, len(payload)) + payload
        result, stats = m.optimize_portable(value)
        self.assertEqual(result, value)
        self.assertEqual(stats['jpeg_unchanged'], 1)

    def test_atlas_labels_order_encoding_and_dimensions_survive(self):
        m = self.subject()
        payload = self.png([(40, 40, 40, 100)] * 100)
        value = struct.pack('<4s7I', b'KIMG', 1, 800, 600, 100, 1, 7, len(payload)) + payload
        label = '按钮'.encode('utf-8')
        atlas = b'KATL' + struct.pack('<II', 1, 1) + struct.pack('<II', len(label), len(value)) + label + value
        result, stats = m.optimize_portable(atlas)
        self.assertEqual(result[:16], atlas[:16])
        self.assertEqual(result[20:20 + len(label)], label)
        child = result[20 + len(label):]
        self.assertEqual(child[:28], value[:28])
        with Image.open(io.BytesIO(child[32:])) as im:
            self.assertEqual(list(im.convert('RGBA').getdata()), [(40, 40, 40, 100)] * 100)
        self.assertEqual(stats['png_verified'], 1)

    def test_repeated_payload_aliases_keep_every_index_and_sentinel(self):
        m = self.subject()
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'fixture.mrg'
            with path.open('w+b') as out:
                writer = m.ArchiveWriter(out)
                rows = [writer.add(b'first', 1), writer.add(b'second', 1), writer.add(b'first', 1), writer.add(b'', 0)]
                hed = b''.join(rows) + b'\xff' * 8
                self.assertEqual(len(hed), 40)
                self.assertEqual(struct.unpack('<IHH', rows[0]), (0, 1, 1))
                self.assertEqual(struct.unpack('<IHH', rows[1]), (1, 1, 1))
                self.assertEqual(struct.unpack('<IHH', rows[2]), (0, 1, 1))
                self.assertEqual(writer.duplicates, 1)
                self.assertEqual(writer.total, 4096)
            data = path.read_bytes()
            self.assertEqual([data[struct.unpack('<IHH', row)[0] * 2048:][:len(want)] for row, want in zip(rows, [b'first', b'second', b'first', b''])], [b'first', b'second', b'first', b''])
            self.assertEqual(hed[-8:], b'\xff' * 8)


if __name__ == '__main__':
    unittest.main()

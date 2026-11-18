"""Small synthetic regressions for offline archive resizing and bank lookup."""
import importlib.util
import os
from pathlib import Path
import struct
import tempfile
import unittest
import zlib

import resize_story_variants as resize
from generate_vita_asset_profile_540 import audio_bank_locations


class AudioBankTests(unittest.TestCase):
    def test_logical_stream_offsets_and_invalid_index(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            index = b'test.ogg'.ljust(32, b'\0') + struct.pack('<II', 56, 4)
            path = root/'bgm.mrg'
            path.write_bytes(struct.pack('<8sII', b'KAUDMRG1', 1, zlib.crc32(index))+index+b'OggS')
            self.assertEqual(audio_bank_locations(root)['stream/test.ogg'], (path, 56, 4))
            data = bytearray(path.read_bytes())
            data[16] ^= 1
            path.write_bytes(data)
            with self.assertRaises(AssertionError):
                audio_bank_locations(root)


class ResizeTests(unittest.TestCase):
    def test_roundtrip_stream_and_dedup(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            writer = resize.Writer(root, 'sample')
            data, decoded = resize.stored(b'KIMG'+b'a'*7000)
            writer.write('A.NXZ', data, decoded)
            writer.write('B.NXZ', data, decoded)
            writer.finish()
            reader = resize.Reader(root, 'sample', 1)
            self.assertEqual(reader.rows[0], reader.rows[1])
            self.assertEqual(resize.unpack(reader.read(1)), b'KIMG'+b'a'*7000)
            reader.volume.__exit__(None, None, None)

    def test_tile_grid_changes_preserve_logical_size_and_fallback(self):
        deps = os.environ.get('KNIFE_ISPC_DEPS')
        if not deps and importlib.util.find_spec('ispc_texcomp') is None:
            self.skipTest('ispc_texcomp not installed; set KNIFE_ISPC_DEPS')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # Initialize the worker codec using a minimal empty reader archive.
            init = resize.Writer(root, 'init')
            init.finish()
            resize.init_worker(str(root), 'init', 1, deps)
            resize.reader.volume.__exit__(None, None, None)
            Image = resize.Image
            original = Image.new('RGBA', (600, 32), (35, 130, 220, 255))
            fallback = resize.encode(original.resize((60, 4)), (1200, 64))
            tiles = [resize.encode(original.crop((0, 0, 508, 32)), (508, 32)),
                     resize.encode(original.crop((500, 0, 600, 32)), (100, 32))]
            blocks = [fallback, *tiles]
            header = struct.pack('<4s7I', b'KQTL', 1, 1200, 64, 600, 32, 2, 1)
            header += b''.join(struct.pack('<5I', i+1, *block[2]) for i, block in enumerate(blocks))
            writer = resize.Writer(root, 'sample')
            writer.write('PARENT.NXZ', *resize.stored(header, True))
            for i, block in enumerate(blocks):
                writer.write(f'TILE{i}.NXZ', block[0], block[1])
            writer.finish()
            resize.reader = resize.Reader(root, 'sample', 1)
            result = resize.convert((0, .8, False))
            self.assertEqual(result['logical'], (1200, 64))
            self.assertEqual(result['after'], (480, 26))
            self.assertEqual(len(result['blocks']), 2)
            self.assertEqual(result['blocks'][0][:2], fallback[:2])
            self.assertEqual(result['blocks'][1][2][:2], (480, 26))
            self.assertEqual(result['blocks'][1][2][2], 0x85000000)
            decoded = resize.decode(resize.unpack(result['blocks'][1][0]))
            self.assertEqual(decoded.getchannel('A').getextrema(), (255, 255))
            kept = resize.convert((0, .8, True))
            self.assertEqual(kept['after'], (600, 32))
            self.assertEqual(kept['blocks'], blocks)
            resize.reader.volume.__exit__(None, None, None)

    def test_transparency_uses_bc3(self):
        if not hasattr(resize, 'Image'):
            self.skipTest('codec initialized by tile test')
        image = resize.Image.new('RGBA', (18, 19), (180, 80, 30, 128))
        block = resize.encode(image, (36, 38))
        self.assertEqual(block[2][2], 0x87000000)
        self.assertEqual(struct.unpack_from('<II', resize.unpack(block[0]), 8), (36, 38))
        decoded = resize.decode(resize.unpack(block[0]))
        self.assertTrue(all(120 <= n <= 136 for n in decoded.getchannel('A').getextrema()))


if __name__ == '__main__':
    unittest.main()

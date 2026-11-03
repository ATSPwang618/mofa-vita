"""Offline asset regression: allocation bounds, strip identity and archive ABI."""
import hashlib
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
import zlib

from PIL import Image
import limit_runtime_art as budget


class OfflineBudgetTests(unittest.TestCase):
    def test_profile_guard_rejects_reintroduced_large_art_before_packaging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            image = Image.new('RGBA', (508,2037), (45,77,123,128))
            stream = io.BytesIO(); image.save(stream,format='PNG')
            raw = b'KIMG'+struct.pack('<7I',1,1041,4166,508,2037,2,len(stream.getvalue()))+stream.getvalue()
            (root/'jallpac.nam').write_bytes(b'PORTRAIT.NXZ'.ljust(32,b'\0'))
            manifest = {'offline_story_memory_budget':{'schema':budget.SCHEMA,'rgba_allocation_cap':budget.RGBA_CAP},
                        'archives':[{'name':'jallpac'}]}
            def store(data):
                padded=data+bytes(-len(data)%2048)
                (root/'jallpac.mrg').write_bytes(padded)
                (root/'jallpac.hed').write_bytes(struct.pack('<IHH',0,len(padded)//2048,0)+b'\xff'*8)
            store(raw)
            with self.assertRaisesRegex(ValueError,'RGBA.*budget'):
                budget.metadata.check_story_rgba_budget(lambda name:root/name,manifest)
            raw,_,_=budget.encode_image(image,(1041,4166),2,1)
            store(raw)
            self.assertEqual(budget.metadata.check_story_rgba_budget(lambda name:root/name,manifest)['checked'],1)

    def test_actual_crash_portrait_and_landscape_fit_before_runtime_decode(self):
        for size in [(508, 2037), (503, 2048), (1314, 788), (1400, 642), (960, 540)]:
            out = budget.fit_size(*size)
            self.assertLessEqual(budget.rgba_charge(*out), budget.RGBA_CAP)
            self.assertLessEqual(out[0], 960)
            self.assertLessEqual(out[1], 540)
            self.assertLessEqual(abs(out[0] * size[1] - out[1] * size[0]), max(size))
        self.assertEqual(budget.fit_size(508, 2037), (134, 540))

    def test_pitch_boundary_and_repeated_processing_do_not_grow_or_shrink_again(self):
        for w in (1, 7, 8, 9, 127, 508, 511, 512, 513, 960, 2048):
            for h in (1, 7, 128, 511, 512, 540, 1024, 2037, 2048):
                out = budget.fit_size(w, h)
                self.assertLessEqual(budget.rgba_charge(*out), budget.RGBA_CAP)
                self.assertEqual(out, budget.fit_size(*out))
                self.assertTrue(0 < out[0] <= w and 0 < out[1] <= h)

    def test_language_strips_keep_four_separate_rows_and_no_colour_bleed(self):
        src = Image.new('RGBA', (960, 1012))
        colours = [(255, 0, 0, 255), (0, 255, 0, 128), (0, 0, 255, 255), (255, 255, 255, 64)]
        for k, colour in enumerate(colours):
            src.paste(colour, (0, k * 253, 960, (k + 1) * 253))
        size = budget.fit_size(*src.size, strips=4)
        out = budget.resize_strips(src, size, 4)
        self.assertEqual(out.height % 4, 0)
        self.assertLessEqual(budget.rgba_charge(*out.size), budget.RGBA_CAP)
        for k, colour in enumerate(colours):
            strip = out.crop((0, k * out.height // 4, out.width, (k + 1) * out.height // 4))
            self.assertEqual(strip.getextrema(), tuple((c, c) for c in colour))

    def test_offline_conversion_preserves_logical_size_alpha_and_codec_contract(self):
        src = Image.new('RGBA', (508, 2037), (45, 77, 123, 128))
        for codec in (2, 7, 9):
            raw, expected, info = budget.encode_image(src, (1041, 4166), codec, 1)
            self.assertEqual(struct.unpack_from('<II', raw, 8), (1041, 4166))
            self.assertEqual(struct.unpack_from('<II', raw, 16), (134, 540))
            self.assertEqual(struct.unpack_from('<I', raw, 24)[0], codec)
            self.assertLessEqual(info['rgba_allocation_bytes'], budget.RGBA_CAP)
            self.assertEqual(expected.size, (134, 540))
            self.assertTrue(all(120 <= a <= 136 for a in expected.getchannel('A').getextrema()))

    def test_scan_and_repack_all_entries_without_executing_scenes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            assets, project = root / 'assets', root / 'project'
            assets.mkdir(); (project / 'jallpac').mkdir(parents=True)
            picture = Image.new('RGBA', (508, 2037), (45, 77, 123, 128))
            png = io.BytesIO(); picture.save(png, format='PNG')
            data = b'KIMG' + struct.pack('<7I', 1, 1041, 4166, 508, 2037, 2, len(png.getvalue())) + png.getvalue()
            compressed = zlib.compress(data)
            stored = b'NXCX' + struct.pack('<III', len(data), len(compressed), 0) + compressed
            padded = stored + bytes(-len(stored) % 2048)
            marker = b'opaque auxiliary entry' + bytes(2048 - len(b'opaque auxiliary entry'))
            (assets / 'jallpac.mrg').write_bytes(padded + marker)
            sectors = len(padded) // 2048
            (assets / 'jallpac.hed').write_bytes(struct.pack('<IHH', 0, sectors, (len(data)+2047)//2048) * 2
                + struct.pack('<IHH', sectors, 1, 0) + b'\xff' * 8)
            (assets / 'jallpac.nam').write_bytes(b'PORTRAIT.NXZ'.ljust(32, b'\0') * 2 + b'AUX'.ljust(32,b'\0'))
            (project / 'jallpac/image.png').write_bytes(png.getvalue())
            manifest = {'archive': 'jallpac', 'entries': [
                {'index': i, 'name': 'PORTRAIT.NXZ', 'files': [{'path': 'image.png', 'sha256': hashlib.sha256(png.getvalue()).hexdigest()}]}
                for i in range(2)]}
            (project / 'jallpac/manifest.json').write_text(json.dumps(manifest))
            files = [{'path': p.name, 'bytes': p.stat().st_size, 'sha256': budget.file_sha(p)} for p in assets.iterdir()]
            pack = {'method': 'native-540p-encoded-unorm-v2', 'files': files,
                'archives': [{'name': 'jallpac', 'pixels': 2*508*2037, 'bytes': len(padded)+2048}],
                'statistics': {'bytes': sum(f['bytes'] for f in files), 'pixels': 2*508*2037}}
            (assets / 'vita-540p-assets.json').write_text(json.dumps(pack))
            plan = budget.scan(assets, project, set())
            self.assertEqual(plan['counts']['resize'], 2)
            self.assertEqual(plan['counts']['non-image'], 1)
            output = root / 'output'
            report = budget.rebuild(assets, project, output, plan, jobs=1)
            self.assertEqual(len(report['images']), 2)
            rows = list(struct.iter_unpack('<IHH', (output/'jallpac.hed').read_bytes()))
            self.assertEqual(rows[0], rows[1], 'shared locators must stay shared')
            with (output/'jallpac.mrg').open('rb') as f:
                self.assertEqual(budget.read_entry(f, rows[2]), marker)
                raw = budget.unpack(budget.read_entry(f, rows[0]))
            self.assertEqual(struct.unpack_from('<4I', raw, 8), (1041, 4166, 134, 540))
            self.assertEqual((assets/'jallpac.mrg').read_bytes(), padded+marker, 'source runtime is immutable during staging')
            self.assertEqual((project/'jallpac/image.png').read_bytes(), png.getvalue())
            self.assertEqual((assets/'jallpac.nam').read_bytes(), (output/'jallpac.nam').read_bytes())
            # A changed file must fail identity checks before any replacement is published.
            (project/'jallpac/image.png').write_bytes(b'changed')
            with self.assertRaises(ValueError):
                budget.rebuild(assets, project, root/'rejected', plan, jobs=1)


if __name__ == '__main__':
    unittest.main()

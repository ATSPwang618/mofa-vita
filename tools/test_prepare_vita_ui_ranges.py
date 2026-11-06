"""Offline UI pack regressions; synthetic archives contain no game payloads."""
import hashlib
import json
from pathlib import Path
import struct
import tempfile
import unittest
import zlib

import prepare_vita_ui_ranges as pack


def image():
    # Structural KIMG/PNG metadata, sufficient for the metadata-only packer.
    png = b'\x89PNG\r\n\x1a\n' + struct.pack('>I', 13) + b'IHDR'
    png += struct.pack('>II5B', 8, 4, 8, 6, 0, 0, 0) + b'\0' * 4
    return b'KIMG' + struct.pack('<7I', 1, 16, 8, 8, 4, 7, len(png)) + png


def atlas():
    result = b'KATL' + struct.pack('<II', 1, 2)
    for name in (b'th_first', b'th_last'):
        result += struct.pack('<II', len(name), len(image())) + name + image()
    return result


class UiRangePackTests(unittest.TestCase):
    def fixture(self, directory):
        directory.mkdir()
        data = atlas()
        compressed = zlib.compress(data)
        nx = b'NXCX' + struct.pack('<III', len(data), len(compressed), 0) + compressed
        nx += bytes(-len(nx) % 2048)
        other = b'UNCHANGED' + bytes(2048 - 9)
        (directory / 'allui.mrg').write_bytes(nx + other)
        # A duplicate extent must stay shared. A NOFILE row must stay absent.
        rows = [(0, 1, 1), (0, 1, 1), (1, 1, 1), (0, 0, 0)]
        (directory / 'allui.hed').write_bytes(b''.join(struct.pack('<IHH', *r) for r in rows) + b'\xff' * 8)
        names = ['CG_PARTS.NXZ', 'ARCHIVE_PARTS_ZC.NXZ', 'UNRELATED.NXZ', 'NOFILE.EXISTS']
        (directory / 'allui.nam').write_bytes(b''.join(n.encode().ljust(32, b'\0') for n in names))
        manifest = {'files': [], 'archives': [{'name': 'allui', 'bytes': 4096}], 'statistics': {'bytes': 4096}}
        for ext in ('hed', 'nam', 'mrg'):
            path = directory / f'allui.{ext}'
            manifest['files'].append({'path': path.name, 'bytes': path.stat().st_size,
                'sha256': hashlib.sha256(path.read_bytes()).hexdigest()})
        (directory / 'vita-540p-assets.json').write_text(json.dumps(manifest))
        return data, nx, other

    def test_library_and_gallery_are_seekable_without_touching_kimg_or_other_entries(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            expected, _, other = self.fixture(root / 'in')
            report = pack.prepare(root / 'in', root / 'out')
            rows = list(struct.iter_unpack('<IHH', (root / 'out/allui.hed').read_bytes()))
            raw = (root / 'out/allui.mrg').read_bytes()
            self.assertEqual(rows[0], rows[1], 'duplicate extents stay deduplicated')
            self.assertEqual(raw[rows[0][0]*2048:rows[0][0]*2048+len(expected)], expected)
            self.assertEqual(raw[rows[2][0]*2048:(rows[2][0]+rows[2][1])*2048], other)
            self.assertEqual(rows[3:], [(0, 0, 0), (0xffffffff, 0xffff, 0xffff)])
            self.assertEqual(report['members_verified'], 4)
            self.assertEqual(len(report['changed_entries']), 2)
            self.assertTrue(report['image_bytes_unchanged'])
            self.assertEqual(pack.verify(root / 'out')['members_verified'], 4)

    def test_story_and_save_ui_languages_are_selected(self):
        for base in ('DISP_PARTS', 'SAVE_PARTS', 'MENU_PARTS', 'CONF_PARTS', 'TITLE_PARTS'):
            for suffix in ('', '_ZC', '_EN'):
                self.assertTrue(pack.selected(base + suffix + '.NXZ'))
        self.assertFalse(pack.selected('DISP_PARTSOTHER.NXZ'))

    def test_raw_repack_is_idempotent_and_keeps_manifest_identity(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root / 'in')
            pack.prepare(root / 'in', root / 'first')
            pack.prepare(root / 'first', root / 'second')
            for name in ('allui.hed', 'allui.nam', 'allui.mrg', 'vita-540p-assets.json'):
                self.assertEqual((root/'first'/name).read_bytes(), (root/'second'/name).read_bytes(), name)

    def test_source_hash_mismatch_is_rejected_before_publishing(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root / 'in')
            (root/'in/allui.mrg').write_bytes(b'broken')
            with self.assertRaisesRegex(ValueError, 'manifest mismatch'):
                pack.prepare(root/'in', root/'out')
            self.assertFalse((root/'out/vita-540p-assets.json').exists())

    def test_compressed_pack_cannot_claim_direct_ui_reads(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.fixture(root/'in')
            with self.assertRaisesRegex(ValueError, 'still compressed'):
                pack.verify(root/'in')

    def test_nonzero_tail_truncated_nx_and_extra_sector_are_rejected(self):
        expected = atlas()
        self.assertEqual(pack.atlas_members(expected + bytes(-len(expected) % 2048))[1], len(expected))
        for invalid in (expected+b'\x01', expected+bytes(4096-len(expected)),
                        expected[:-1]):
            with self.assertRaises(ValueError):
                pack.atlas_members(invalid)
        with self.assertRaises(ValueError):
            pack.decode_entry(b'NXCX'+struct.pack('<III', len(expected), 100, 0)+b'x')


if __name__ == '__main__':
    unittest.main()

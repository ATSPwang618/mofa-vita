"""Exercise credits maintenance after temporary resource backups are cleaned."""
import contextlib
import hashlib
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest
from unittest.mock import patch
import zlib

import apply_credit_intro as credit


def png(rgba):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    return (credit.PNG_MAGIC + chunk(b'IHDR', struct.pack('>IIBBBBB', 8, 4, 8, 6, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress((b'\0' + bytes(rgba) * 8) * 4)) + chunk(b'IEND', b''))


class CreditMaintenanceTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='knife-credits-')
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.assets = self.root / 'assets'
        self.assets.mkdir()
        self.artwork = self.root / 'artwork'
        self.artwork.mkdir()
        for name, color in zip(credit.IMAGES, [(20, 40, 60, 255), (80, 100, 120, 255)]):
            (self.artwork / name).write_bytes(png(color))
        self.here = patch.object(credit, 'HERE', self.artwork)
        self.here.start()
        self.addCleanup(self.here.stop)
        chunk, unpacked = credit.build_entry(self.artwork / credit.IMAGES[0])
        sectors = len(chunk) // credit.SECTOR
        self.base_mrg = chunk * 2
        self.base_hed = struct.pack('<IHHIHH', 0, sectors, unpacked, sectors, sectors, unpacked) + b'\xff' * 8
        mrg, hed = credit.build_archive(self.base_mrg, self.base_hed)
        (self.assets / 'parts.mrg').write_bytes(mrg)
        (self.assets / 'parts.hed').write_bytes(hed)
        files = [{'path': name, 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
                 for name, data in [('parts.hed', hed), ('parts.mrg', mrg)]]
        pixels = 4 * 8 * 4
        manifest = {'schema': 'knife-540p-assets-v1', 'color_contract': 'encoded-unorm8-v1',
                    'codecs': [1, 2, 7, 9], 'files': files,
                    'archives': [{'name': 'parts', 'entries': 4, 'textures': 4, 'pixels': pixels, 'bytes': len(mrg)}],
                    'statistics': {'files': 2, 'bytes': len(hed) + len(mrg), 'entries': 4,
                                   'textures': 4, 'pixels': pixels}}
        (self.assets / credit.MARKER).write_text(json.dumps(manifest, indent=2), encoding='utf-8')

    def run_credit(self, *options):
        with contextlib.redirect_stdout(io.StringIO()):
            return credit.main(['--assets', str(self.assets), '--backup', str(self.root / 'backup'), *options])

    def test_check_after_cleanup_needs_no_backup_and_writes_nothing(self):
        before = {p.name: p.read_bytes() for p in self.assets.iterdir()}
        self.assertEqual(self.run_credit('--check'), 0)
        self.assertFalse((self.root / 'backup').exists())
        self.assertEqual(before, {p.name: p.read_bytes() for p in self.assets.iterdir()})

    def test_replace_existing_credit_preserves_original_entries_and_updates_manifest(self):
        backup = self.root / 'backup'
        backup.mkdir()
        (backup / 'parts.hed').write_bytes(self.base_hed)
        (backup / 'parts.mrg').write_bytes(self.base_mrg)
        (self.artwork / credit.IMAGES[1]).write_bytes(png((240, 10, 30, 255)))
        self.assertEqual(self.run_credit(), 0)
        updated = (self.assets / 'parts.mrg').read_bytes()
        self.assertEqual(updated[:len(self.base_mrg)], self.base_mrg)
        self.assertEqual(self.run_credit('--check'), 0)


if __name__ == '__main__':
    unittest.main()

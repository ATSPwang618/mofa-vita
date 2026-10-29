"""Contract tests for the two exact backlog atlas replacements, without a GPU."""
import importlib.util
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("backlog_stage_tested", Path(__file__).with_name("stage-vita-gxt-backlog.py"))
stage = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = stage
spec.loader.exec_module(stage)
c = stage.checked


class BacklogStageContract(unittest.TestCase):
    def setUp(self):
        self.original = c.image_bytes()
        self.other = c.image_bytes(payload=b"preserve palette and transparency bytes")
        self.atlas = c.atlas_bytes([("other", self.other), ("bg_backlog", self.original)])
        self.replacement = c.image_bytes(codec=8, payload=c.gxt_fixture(fmt=0x85000000))

    def swap(self, atlas=None, replacement=None):
        return stage.replace_backlog(self.atlas if atlas is None else atlas,
                                     self.replacement if replacement is None else replacement,
                                     c.sha(self.atlas), c.sha(self.original))

    def test_only_exact_member_changes_and_logical_coordinates_survive(self):
        output, original = self.swap()
        before, after = c.parse_atlas(self.atlas), c.parse_atlas(output)
        self.assertEqual(original, self.original)
        self.assertEqual([x["name_bytes"] for x in before], [x["name_bytes"] for x in after])
        self.assertEqual(before[0]["record"], after[0]["record"])
        self.assertEqual(after[1]["image"], self.replacement)
        self.assertEqual(after[1]["metadata"]["logical"], [16, 8])
        self.assertEqual(after[1]["metadata"]["physical"], [8, 4])

    def test_identity_mismatch_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "atlas SHA"):
            self.swap(c.atlas_bytes([("bg_backlog", self.original)]))
        with self.assertRaisesRegex(ValueError, "KIMG SHA"):
            stage.replace_backlog(self.atlas, self.replacement, c.sha(self.atlas), "0" * 64)

    def test_missing_and_duplicate_member_are_rejected(self):
        for items in [[("other", self.other)], [("bg_backlog", self.original)] * 2]:
            data = c.atlas_bytes(items)
            with self.assertRaises(ValueError):
                stage.replace_backlog(data, self.replacement, c.sha(data), c.sha(self.original))

    def test_unselected_name_collisions_are_preserved_as_ordered_records(self):
        data = c.atlas_bytes([("bg_backlog", self.original), ("unselected", self.other),
                              ("unselected", c.image_bytes(payload=b"different source record"))])
        output, _ = stage.replace_backlog(data, self.replacement, c.sha(data), c.sha(self.original))
        self.assertEqual([x["record"] for x in c.parse_atlas(data)[1:]],
                         [x["record"] for x in c.parse_atlas(output)[1:]])

    def test_dxt5_or_coordinate_change_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "DXT1"):
            self.swap(replacement=c.image_bytes(codec=8, payload=c.gxt_fixture()))
        with self.assertRaisesRegex(ValueError, "dimensions"):
            self.swap(replacement=c.image_bytes(logical=(18, 8), codec=8,
                                               payload=c.gxt_fixture(fmt=0x85000000)))

    def test_rebuilt_archive_preserves_shared_extents_raw_padding_and_other_members(self):
        with tempfile.TemporaryDirectory(prefix="knife-backlog-test-") as td:
            root = Path(td)
            source, _ = c.source_fixture(root)
            # Rebuild two synthetic selected atlases while retaining a raw padded
            # entry and an additional exact shared source extent.
            archive = c.Archive(source)
            raw = [archive.stored(e) for e in archive.entries]
            raw[6] = c.wrap_nx(self.atlas)
            raw[7] = c.wrap_nx(self.atlas)
            hed, nam, mrg, offset = bytearray(), bytearray(), bytearray(), 0
            for index, data in enumerate(raw):
                size = (len(data) + c.SECTOR - 1) // c.SECTOR
                hed.extend(struct.pack("<IHH", offset, size, size))
                name = c.TARGET_ENTRY if index == 11 else f"ENTRY_{index:02}.BIN"
                nam.extend(name.encode().ljust(30, b"\0") + b"\r\n")
                mrg.extend(data + b"\xa5" * (size * c.SECTOR - len(data)))
                offset += size
            hed.extend(hed[0:8])
            nam.extend(b"ALIAS.BIN".ljust(30, b"\0") + b"\r\n")
            hed.extend(b"\xff" * 16)
            for suffix, data in (("hed", hed), ("nam", nam), ("mrg", mrg)):
                (source / f"allui.{suffix}").write_bytes(data)
            output = root / "output"
            output.mkdir()
            replacement, _ = self.swap()
            report = stage.rebuild_archive(source, output, {6: replacement, 7: replacement})
            self.assertEqual(report["changed_archive_entries"], 2)
            self.assertEqual(report["unchanged_entries_stored_and_decoded_exact"], len(raw) - 1)
            before, after = c.Archive(source), c.Archive(output)
            self.assertEqual(before.nam, after.nam)
            self.assertEqual(before.tail, after.tail)
            self.assertEqual(after.entries[0].offset, after.entries[-1].offset)
            self.assertEqual(before.stored(before.entries[12]), after.stored(after.entries[12]))


if __name__ == "__main__":
    unittest.main()

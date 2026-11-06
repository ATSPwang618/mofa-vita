#!/usr/bin/env python3
"""Exercise build receipts for stock and custom hardware test profiles (host only)."""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class HardwareProfileReceiptTests(unittest.TestCase):
    def receipt(self, profile=None, expected_error=None):
        environment = os.environ.copy()
        environment.pop("KNIFE_VITA_OPTIMIZATIONS", None)
        environment["KNIFE_VITA_BUILD_KIND"] = "dev"
        environment["KNIFE_VITA_TITLE_ID"] = "KNIF00540"
        environment["PATH"] = "/opt/vitasdk/bin" + os.pathsep + environment["PATH"]
        if profile is not None:
            environment["KNIFE_VITA_OPTIMIZATIONS"] = profile
        (ROOT / ".work").mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="hardware-profile-", dir=ROOT / ".work") as destination:
            result = subprocess.run([
                "python3", str(ROOT / "tools/vita-build-receipt.py"), str(ROOT), destination,
                "1.99.0", "armv7-sony-vita-newlibeabihf", "game",
            ], env=environment, text=True, capture_output=True)
            receipt = Path(destination) / "vita-game-540p-build.json"
            if expected_error:
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected_error, result.stderr)
                self.assertFalse(receipt.exists())
                return None
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(receipt.read_text())["optimization_profile"]

    def test_stock_hardware_receipt_records_active_candidates_and_migration(self):
        profile = self.receipt()
        self.assertEqual(profile["default"], "hardware-test")
        self.assertEqual(profile["default_mask"], 0x1CFDFFFFFFF)
        self.assertEqual(profile["default_excludes"], ["sparse-coverage", "rt-scenes-4", "ps-full-material"])
        self.assertTrue({"group-bounds", "half-color", "tiled-targets", "blur-weights", "rt-scenes-2",
                         "ps-all-operations", "ps-plain-op7", "ps-chain-op7"}.issubset(profile["default_features"]))
        self.assertTrue(profile["legacy_stock_migration"]["enabled"])
        self.assertEqual(profile["legacy_stock_migration"]["exact_profiles"],
                         ["all", "all ps-all-operations", "all ps-full-material"])
        self.assertTrue(profile["legacy_stock_migration"]["custom_profiles_preserved"])

    def test_custom_hardware_receipt_reflects_disables_and_capacity_four(self):
        profile = self.receipt("hardware-test -half-color -rt-scenes-2 rt-scenes-4")
        self.assertEqual(profile["default_mask"], 0x1D6DFFFFFFF)
        self.assertEqual(profile["default_excludes"], ["sparse-coverage", "half-color", "rt-scenes-2", "ps-full-material"])
        self.assertFalse(profile["legacy_stock_migration"]["enabled"])

    def test_stable_baseline_receipt_does_not_claim_candidate_defaults(self):
        profile = self.receipt("all")
        self.assertEqual(profile["default_mask"], 0x5FFFFFFF)
        self.assertNotIn("ps-all-operations", profile["default_features"])
        self.assertIn("ps-all-operations", profile["default_excludes"])
        self.assertFalse(profile["legacy_stock_migration"]["enabled"])

    def test_invalid_default_cannot_emit_a_misleading_receipt(self):
        for text, error in [
            ("hardware-test typo", "unknown optimization typo"),
            ("hardware-test rt-scenes-4", "mutually exclusive"),
            ("hardware-test -batch-arena", "require batch-arena"),
            ("baseline batch-arena dependency-graph", "require batch-arena and async-submit"),
        ]:
            with self.subTest(profile=text):
                self.receipt(text, expected_error=error)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Host controls for the separate synthetic Vita UI color oracle; no GPU is run."""
from __future__ import annotations

import importlib.util
import copy
import hashlib
import json
import math
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from PIL import Image


def load_tool(name: str):
    path = Path(__file__).with_name(name)
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


oracle = load_tool("check-vita-ui-color-pixels.py")


class MathTests(unittest.TestCase):
    def test_alter_known_gray_and_alpha(self):
        self.assertEqual(oracle.alter_pixel((128, 64, 200, 37)), (187, 139, 228, 37))
        self.assertEqual(oracle.alter_pixel((0, 255, 0, 0)), (0, 255, 0, 0))

    def test_srgb_decode_precedes_bilinear_filter(self):
        image = Image.new("RGBA", (2, 1))
        image.putdata([(0, 0, 0, 0), (255, 255, 255, 255)])
        sample = oracle.sample_linear_srgb(image, .5, .5)
        for actual in sample:
            self.assertAlmostEqual(actual, .5, places=12)
        # Decoding the already-filtered byte value would incorrectly give .214.
        self.assertGreater(sample[0] - oracle.srgb_decode(.5), .28)

    def test_clamp_uses_logical_extent(self):
        image = Image.new("RGBA", (2, 1))
        image.putdata([(255, 0, 0, 51), (0, 255, 0, 204)])
        self.assertEqual(oracle.sample_linear_srgb(image, -100., .5), (1., 0., 0., .2))
        self.assertEqual(oracle.sample_linear_srgb(image, 100., .5), (0., 1., 0., .8))

    def test_sprite_uses_premultiplied_rgb_but_preserves_destination_alpha(self):
        result = oracle.sprite_blend((.8, .4, .2, .5), (.5, .75, 1., .6), (.2, .3, .4, .9))
        for actual, expected in zip(result, (.26, .3, .34, .9)):
            self.assertAlmostEqual(actual, expected, places=12)

    def test_present_includes_intermediate_srgb_byte_quantization(self):
        value = oracle.present_working_rgb((.5, .25, 0.))
        self.assertEqual(value, (129, 62, 0))
        self.assertTrue(all(type(channel) is int for channel in value))

    def test_invalid_sampler_coordinates_fail(self):
        with self.assertRaises(ValueError):
            oracle.sample_linear_srgb(Image.new("RGBA", (1, 1)), math.nan, 0.)


class EvidenceControls(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(__file__).resolve().parents[1]
        cls.fixtures = cls.root / ".work/vita/ui-color-fixtures"
        cls.directory = Path(tempfile.mkdtemp(prefix="cpu-controls-", dir=cls.root / ".work/vita/ui-color-validation"))
        manifest = cls.fixtures / "manifest.json"
        cls.runtime = {"expected_title_id": oracle.TITLE_ID,
                       "fixture_manifest_sha256": hashlib.sha256(manifest.read_bytes()).hexdigest(),
                       "evidence_kind": "cpu-control", "synthetic": True, "runtime_verified": True,
                       "ui_color_captures": {}}
        cls.references, cls.source_identity = oracle.load_references(cls.fixtures, cls.runtime)
        cls.expected = {}
        for stage, image in cls.references.items():
            frame = oracle.render_expected(image)
            cls.expected[hashlib.sha256(image.tobytes()).hexdigest()] = frame
            artifact = oracle.evidence.save_png(cls.directory / f"{stage}-CPU-CONTROL.png", frame)
            cls.runtime["ui_color_captures"][stage] = {"captured": True, "stage": stage,
                "stage_valid_during_capture": True, "artifact": artifact}
        cls.fault_report = []

    def compare_cached(self, runtime, label):
        # Real color reference generation was executed in setUpClass; this cache
        # avoids redoing it when testing only receipt and image-authentication gates.
        def known(image):
            return self.expected[hashlib.sha256(image.tobytes()).hexdigest()].copy()
        path = self.directory / f"{label}-runtime.json"
        path.write_text(json.dumps(runtime), encoding="utf8")
        with patch.object(oracle, "render_expected", side_effect=known):
            result = oracle.compare(path, self.fixtures, self.directory / f"{label}-comparison.json")
        (self.directory / f"{label}-comparison.json").write_text(json.dumps(result, indent=2), encoding="utf8")
        return result

    def test_cpu_positive_never_claims_gpu_even_with_runtime_true(self):
        result = self.compare_cached(copy.deepcopy(self.runtime), "positive-cpu")
        self.assertTrue(result["pixels_verified"])
        self.assertFalse(result["gpu_verified"])
        self.assertFalse(result["hardware_verified"])
        self.assertTrue(all(x["content"]["max_absolute_error"] == 0 for x in result["stages"].values()))

    def test_actual_process_failure_prevents_gpu_claim(self):
        runtime = copy.deepcopy(self.runtime)
        runtime.update(evidence_kind="gpu-capture", synthetic=False, runtime_verified=False)
        result = self.compare_cached(runtime, "failed-runtime-gate")
        self.assertTrue(result["pixels_verified"])
        self.assertFalse(result["gpu_verified"])

    def test_negative_gamma_pot_uv_alpha_point_and_tint_controls(self):
        image = self.references["UI_OFFLINE_DXT5"]
        expected = self.expected[hashlib.sha256(image.tobytes()).hexdigest()]
        for fault in ("gamma", "pot-uv", "drop-alpha", "double-alpha", "point", "no-tint"):
            with self.subTest(fault=fault):
                actual = oracle.render_expected(image, fault=fault)
                metric, mask = oracle.evidence.metrics(actual.crop(oracle.CONTENT), expected.crop(oracle.CONTENT),
                                                      oracle.DEFAULT_TOLERANCE, oracle.DEFAULT_BAD_RATIO)
                self.fault_report.append({"fault": fault, **metric})
                oracle.evidence.save_png(self.directory / f"negative-{fault}.png", actual)
                oracle.evidence.save_png(self.directory / f"negative-{fault}-mask.png", mask)
                self.assertFalse(metric["passed"], f"fault {fault} was not detected")

    def test_capture_identity_timing_hash_and_unique_path_gates(self):
        for fault in ("stage", "timing", "hash", "reused-path"):
            with self.subTest(fault=fault):
                runtime = copy.deepcopy(self.runtime)
                captures = runtime["ui_color_captures"]
                target = captures["UI_OFFLINE_DXT5"]
                if fault == "stage":
                    target["stage"] = "UI_OFFLINE_RGBA"
                elif fault == "timing":
                    target["stage_valid_during_capture"] = False
                elif fault == "hash":
                    target["artifact"]["sha256"] = "0" * 64
                else:
                    target["artifact"] = captures["UI_OFFLINE_RGBA"]["artifact"]
                result = self.compare_cached(runtime, f"bad-{fault}")
                self.assertFalse(result["pixels_verified"])
                self.assertTrue(result["stages"]["UI_OFFLINE_DXT5"]["errors"])

    def test_fixture_manifest_and_title_must_match(self):
        for key, value in (("fixture_manifest_sha256", "0" * 64), ("expected_title_id", "KNIF00012")):
            with self.subTest(key=key):
                runtime = copy.deepcopy(self.runtime)
                runtime[key] = value
                result = self.compare_cached(runtime, f"bad-{key}")
                self.assertFalse(result["pixels_verified"])
                self.assertTrue(result["errors"])

    @classmethod
    def tearDownClass(cls):
        report = {"schema": "knife-vita-ui-color-cpu-controls-v1", "evidence_kind": "cpu-control",
                  "gpu_verified": False, "hardware_verified": False,
                  "directory": str(cls.directory), "fixtures": cls.source_identity,
                  "negative_color_controls": cls.fault_report}
        (cls.directory / "controls.json").write_text(json.dumps(report, indent=2), encoding="utf8")
        print("CPU controls evidence: " + str(cls.directory / "controls.json"))


if __name__ == "__main__":
    unittest.main(verbosity=2)

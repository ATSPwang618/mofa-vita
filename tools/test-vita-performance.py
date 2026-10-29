"""Contract tests for the hardware measurement log reader (stdlib only)."""
import importlib.util
import json
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("performance", Path(__file__).with_name("read-vita-performance.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def fixture(**overrides):
    sample = dict(kind="sample", elapsed_ms=5000, window_ms=5000, frames=100,
        work_us=2000000, wall_us=5000000, update_us=1000000, render_us=1000000,
        max_work_us=60000, p95_work_upper_us=33333, over_16ms=50, over_33ms=10,
        pending=1, ready=1, active=1, jpeg_hw_ok=4, jpeg_work_cdram=0,
        jpeg_peak_work_cdram=1048576, jpeg_last_hw_error=0x80000001)
    sample.update(overrides)
    return json.dumps(dict(kind="header", schema=1)).encode() + b"\n" + json.dumps(sample).encode() + b"\n"


class ReaderTests(unittest.TestCase):
    def test_totals_and_memory_scope(self):
        result = module.analyze(fixture(), "emulator")
        self.assertEqual(result["average_work_ms"], 20)
        self.assertEqual(result["observed_frame_rate"], 20)
        self.assertEqual(result["jpeg_last_hw_error_hex"], "0x80000001")
        self.assertEqual(result["last_cumulative_counters"]["jpeg_hw_ok"], 4)
        self.assertEqual(result["environment"], "emulator")
        self.assertFalse(result["queue_and_jpeg_bound_violations"])

    def test_bounds_are_observed_failures(self):
        result = module.analyze(fixture(pending=9, jpeg_peak_work_cdram=3145729), "hardware")
        self.assertEqual(len(result["queue_and_jpeg_bound_violations"]), 2)

    def test_contract_diagnostics_are_last_values_not_cumulative_counters(self):
        first = fixture(jpeg_contract_mask=8, jpeg_info_y_pitch=(464 << 16) | 272)
        later = fixture(elapsed_ms=10000, jpeg_contract_mask=1,
                        jpeg_info_y_pitch=(480 << 16) | 272).split(b"\n")[1] + b"\n"
        result = module.analyze(first + later, "emulator")
        self.assertEqual(result["jpeg_last_rejected_contract"]["jpeg_contract_mask"], 1)
        self.assertEqual(result["jpeg_last_rejected_contract"]["jpeg_info_y_pitch"], (480 << 16) | 272)
        self.assertFalse(result["counter_discontinuities"])

    def test_truncated_last_line_is_explicit_and_other_bad_json_rejected(self):
        self.assertTrue(module.analyze(fixture() + b'{"kind":', "emulator")["warnings"])
        with self.assertRaises(ValueError): module.analyze(fixture() + b'broken\n', "emulator")

    def test_invalid_or_unbounded_measurements_rejected(self):
        for data in (b"", fixture(frames=0), fixture(over_33ms=101), b"x" * (module.MAX_LOG + 1)):
            with self.subTest(length=len(data)), self.assertRaises(ValueError):
                module.analyze(data, "unspecified")

    def test_counter_wrap_is_not_turned_into_a_rate(self):
        first = fixture()
        later = fixture(elapsed_ms=10000, jpeg_hw_ok=1).split(b"\n")[1] + b"\n"
        result = module.analyze(first + later, "emulator")
        self.assertEqual(result["frames"], 200)
        self.assertEqual(result["last_cumulative_counters"]["jpeg_hw_ok"], 1)
        self.assertEqual(result["counter_discontinuities"], [{"elapsed_ms": 10000, "counter": "jpeg_hw_ok"}])

    def test_audio_counters_distinguish_idle_from_starvation_and_full_is_backpressure(self):
        result = module.analyze(fixture(audio_submitted=244, audio_output=250,
            audio_underruns=2, audio_idle=4, audio_queue_full=400, audio_peak_queued=4,
            audio_mix_blocks=244, audio_mix_max_us=21000, audio_mix_late=1,
            audio_clipped_samples=32), "hardware")
        self.assertEqual(result["audio"]["underrun_blocks"], 2)
        self.assertEqual(result["audio"]["intentional_idle_blocks"], 4)
        self.assertEqual(result["audio"]["late_mix_blocks"], 1)
        self.assertEqual(result["audio"]["maximum_mix_ms"], 21)
        self.assertFalse(result["queue_and_jpeg_bound_violations"])
        self.assertTrue(any("starvation" in warning for warning in result["warnings"]))

    def test_audio_invalid_counter_and_queue_bound(self):
        with self.assertRaises(ValueError):
            module.analyze(fixture(audio_underruns=-1), "hardware")
        result = module.analyze(fixture(audio_peak_queued=5), "hardware")
        self.assertEqual(result["queue_and_jpeg_bound_violations"][0]["field"], "audio_peak_queued")

    def test_audio_reset_is_explicit_and_old_log_has_unknown_audio(self):
        first = fixture(audio_underruns=8)
        later = fixture(elapsed_ms=10000, audio_underruns=1).split(b"\n")[1] + b"\n"
        result = module.analyze(first + later, "hardware")
        self.assertIn({"elapsed_ms": 10000, "counter": "audio_underruns"}, result["counter_discontinuities"])
        self.assertIsNone(module.analyze(fixture(), "hardware")["audio"]["underrun_blocks"])


if __name__ == "__main__":
    unittest.main()

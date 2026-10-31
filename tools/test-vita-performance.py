"""Contract tests for the hardware measurement log reader (stdlib only)."""
import importlib.util
import json
from pathlib import Path
import unittest
from vita_audit_log import FRAME_CLASSES, FRAME_CLASS_FIELDS, analyze_frame_classes

spec = importlib.util.spec_from_file_location("performance", Path(__file__).with_name("read-vita-performance.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
compare_spec = importlib.util.spec_from_file_location("hardware_compare", Path(__file__).with_name("render-audit") / "compare-hardware.py")
comparison = importlib.util.module_from_spec(compare_spec)
compare_spec.loader.exec_module(comparison)


def fixture(**overrides):
    sample = dict(kind="sample", elapsed_ms=5000, window_ms=5000, frames=100,
        work_us=2000000, wall_us=5000000, update_us=1000000, render_us=1000000,
        max_work_us=60000, p95_work_upper_us=33333, over_16ms=50, over_33ms=10,
        pending=1, ready=1, active=1, jpeg_hw_ok=4, jpeg_work_cdram=0,
        jpeg_peak_work_cdram=1048576, jpeg_last_hw_error=0x80000001)
    sample.update(overrides)
    return json.dumps(dict(kind="header", schema=1)).encode() + b"\n" + json.dumps(sample).encode() + b"\n"


class ReaderTests(unittest.TestCase):
    def test_async_resource_events_survive_console_without_a_finished_window(self):
        rows = [dict(kind="header", schema=3)] + [dict(kind="resource_event", schema=1,
            scene=3, pc=104, status=status, detail="IMG0225") for status in ("pending", "failed")]
        data = b"\n".join(json.dumps(row).encode() for row in rows) + b"\n"
        result = module.analyze(data, "hardware", console=True)
        self.assertEqual([r["status"] for r in result["resource_events"]], ["pending", "failed"])
        self.assertEqual(result["frames"], 0)

    def test_watchdog_preserves_stall_without_a_completed_frame_window(self):
        state = dict(kind="runtime_state", schema=1, elapsed_ms=3000, frame=857,
            phase="render", no_progress_ms=2500, stalled=True, scene=1, pc=72,
            requested_scene=1, command_scene=1, command_pc=72, command="STCH",
            native_phase="gxm_notification_wait", native_detail=123,
            input_status=1, buttons=0, last_pressed=16384, press_edges=5,
            last_press_frame=856, wait="none", paused=False, menu_lock=False,
            ended=False, page_waiting=False, page_visible=False, page_chars=0,
            page_revealed=0, observation="atomic-best-effort")
        def capture(row):
            return b'prefix {"kind":"header","schema":3}\n' + b'prefix ' + json.dumps(row).encode() + b'\n'
        result = module.analyze(capture(state), "hardware", console=True)
        self.assertEqual(result["runtime_stalls"], [state])
        self.assertEqual(result["frames"], 0)
        for key, value in (("input_status", "1"), ("stalled", False), ("command_pc", -1)):
            with self.subTest(key=key), self.assertRaises(ValueError):
                module.analyze(capture(dict(state, **{key: value})), "hardware", console=True)

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


class ConsoleReaderTests(unittest.TestCase):
    @staticmethod
    def capture(rows):
        return b"".join(b"[12:00:00] |I| [export_sceClibPrintf]: " +
            json.dumps(row, separators=(",", ":")).encode() + b"\n" for row in rows)

    @staticmethod
    def sample():
        return json.loads(fixture().splitlines()[1])

    @staticmethod
    def bank(frame=100, value=10_556_590_462):
        return dict(kind="audit_bank", frame=frame, domain="rust", part=0,
            counters={"nx_range": dict(calls=401, us=987654, bytes=value, aux=26_318_432)})

    def test_console_prefixes_fragments_and_u64_audit_roundtrip(self):
        sample = self.sample()
        sample["transport_probe"] = "x" * 2400
        text = json.dumps(sample, separators=(",", ":")) + "\n"
        pieces = [text[i:i+384] for i in range(0, len(text), 384)]
        chunks = [dict(kind="audit_chunk", seq=5, part=i, total=len(pieces), payload=value)
                  for i, value in enumerate(pieces)]
        data = self.capture([dict(kind="header", schema=3), *chunks, self.bank(),
            dict(kind="audit", frame=100, phase="window", banks=1, hotspots=0, native_available=False)])
        result = module.analyze(data, "hardware", console=True)
        self.assertEqual(result["frames"], 100)
        window = result["copy_audit"]["windows"][0]
        self.assertTrue(window["complete"])
        self.assertEqual(window["delta"]["rust.nx_range"]["bytes"], 10_556_590_462)
        self.assertGreater(window["observations"]["range_output_to_selected_ratio"], 400)
        self.assertIsNone(window["observations"]["gxt_native_uploads"])

    def test_lost_bank_is_unknown_and_not_a_zero_copy_window(self):
        data = self.capture([dict(kind="header", schema=3), self.sample(), self.bank(),
            dict(kind="audit", frame=100, phase="window", banks=2, hotspots=0, native_available=True)])
        result = module.analyze(data, "hardware", console=True)
        window = result["copy_audit"]["windows"][0]
        self.assertFalse(window["complete"])
        self.assertEqual(window["delta"], {})
        self.assertIsNone(window["observations"])
        self.assertTrue(any("lost bank" in w for w in result["warnings"]))

    def test_partial_chunk_and_counter_reset_are_explicit(self):
        data = self.capture([dict(kind="header", schema=3), self.sample(), self.bank(),
            dict(kind="audit", frame=100, phase="window", banks=1, hotspots=0, native_available=True),
            self.bank(frame=200, value=1),
            dict(kind="audit", frame=200, phase="window", banks=1, hotspots=0, native_available=True),
            dict(kind="audit_chunk", seq=99, part=0, total=2, payload="partial")])
        result = module.analyze(data, "hardware", console=True)
        self.assertEqual(result["copy_audit"]["windows"][1]["resets"], ["rust.nx_range"])
        self.assertTrue(any("Incomplete console chunk" in w for w in result["warnings"]))

    def test_missing_audit_keeps_explicit_counter_interval(self):
        data = self.capture([dict(kind="header", schema=3), self.sample(),
            self.bank(frame=100, value=100),
            dict(kind="audit", frame=100, phase="window", banks=1, hotspots=0, native_available=True),
            self.bank(frame=200, value=200),
            dict(kind="audit", frame=200, phase="window", banks=2, hotspots=0, native_available=True),
            self.bank(frame=300, value=300),
            dict(kind="audit", frame=300, phase="window", banks=1, hotspots=0, native_available=True)])
        result = module.analyze(data, "hardware", console=True)
        last = result["copy_audit"]["windows"][-1]
        self.assertEqual(last["delta_start_frame"], 100)
        self.assertEqual(last["delta"]["rust.nx_range"]["bytes"], 200)

    def test_unchanged_cumulative_banks_can_be_omitted_without_recounting(self):
        end = lambda frame, banks: dict(kind="audit", frame=frame, phase="window",
            banks=banks, hotspots=0, native_available=True)
        data = self.capture([dict(kind="header", schema=3, audit_bank_policy="changed-cumulative-v1"),
            self.sample(), self.bank(frame=100, value=100), end(100, 1), end(200, 0),
            self.bank(frame=300, value=125), end(300, 1)])
        windows = module.analyze(data, "hardware", console=True)["copy_audit"]["windows"]
        self.assertTrue(windows[1]["complete"])
        self.assertEqual(windows[1]["observations"]["range_full_output_bytes"], 0)
        self.assertEqual(windows[2]["delta_start_frame"], 200)
        self.assertEqual(windows[2]["delta"]["rust.nx_range"]["bytes"], 25)

    def test_compact_timing_recovers_lost_sample_and_preserves_shader_capability(self):
        compact = dict(self.sample(), kind="frame_window", frame=100)
        data = self.capture([dict(kind="header", schema=3),
            dict(kind="shader_capabilities", group15=1, ps_fetch17=0), compact])
        result = module.analyze(data, "hardware", console=True)
        self.assertEqual(result["frames"], 100)
        self.assertEqual(result["average_work_ms"], 20)
        self.assertEqual(result["shader_capabilities"][0]["ps_fetch17"], 0)
        self.assertTrue(any("compact timing" in w for w in result["warnings"]))
        full = dict(self.sample(), frame=100)
        result = module.analyze(data + self.capture([full]), "hardware", console=True)
        self.assertEqual(result["frames"], 100)

    def test_malformed_counters_and_conflicting_transport_are_rejected(self):
        bank = self.bank()
        bank["counters"]["nx_range"]["bytes"] = -1
        with self.assertRaises(ValueError):
            module.analyze(self.capture([dict(kind="header", schema=3), bank]), "hardware", console=True)
        chunk = dict(kind="audit_chunk", seq=1, part=0, total=2, payload="a")
        with self.assertRaises(ValueError):
            module.analyze(self.capture([chunk, chunk]), "hardware", console=True)


def class_window(first=1, last=3, emission=4):
    classes = {name: dict.fromkeys(FRAME_CLASS_FIELDS, 0) for name in FRAME_CLASSES}
    for row in classes.values():
        row["native_available"] = False
    row = classes["redraw"]
    row.update(frames=last-first+1, native_frames=last-first+1, native_available=True,
        stage_redraws=last-first+1, work_us=90000, render_us=60000, update_us=30000,
        wall_us=100000, max_work_us=40000, end_scene_us=24000, end_scene_calls=6,
        scenes=6, atlas_wait_us=12000, atlas_wait_calls=1, ps_copies=4,
        ps_copy_us=4000, ps_copy_bytes=4096, group_copies=2, group_copy_us=2000,
        group_copy_bytes=2048, finish_wait_us=3000, transfer_wait_us=1000,
        batch_wait_us=2000, display_wait_us=4000, wait_us=10000)
    return dict(kind="frame_classes", schema=1, frame=emission, first_frame=first,
        last_frame=last, frames=last-first+1, window_ms=5000, classes=classes)


class FrameClassTests(unittest.TestCase):
    def capture(self, rows):
        return ConsoleReaderTests.capture([dict(kind="header", schema=3, frame_classes_schema=1), *rows])

    def test_chunked_classes_and_separate_wait_scopes_roundtrip(self):
        window = class_window()
        text = json.dumps(window, separators=(",", ":")) + "\n"
        pieces = [text[i:i+384] for i in range(0, len(text), 384)]
        chunks = [dict(kind="audit_chunk", seq=8, part=i, total=len(pieces), payload=value)
                  for i, value in enumerate(pieces)]
        result = module.analyze(self.capture(chunks), "hardware", console=True)["frame_classes"]
        self.assertEqual(result["windows"], [window])
        redraw = result["totals"]["redraw"]
        self.assertEqual(redraw["average_work_ms"], 30)
        self.assertEqual(redraw["average_end_scene_ms"], 8)
        self.assertAlmostEqual(redraw["average_wait_ms"], 3.333333)
        self.assertEqual(redraw["ps_copies"], 4)
        self.assertEqual(redraw["group_copy_bytes"], 2048)
        self.assertEqual(result["totals"]["reuse"]["average_work_ms"], None)

    def test_bad_intervals_counts_waits_and_classes_are_rejected(self):
        for field, value in (("frames", 2), ("end_scene_us", -1), ("wait_us", 34000),
                             ("stage_redraws", 0), ("work_us", True)):
            window = class_window()
            window["classes"]["redraw"][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                module.analyze(self.capture([window]), "hardware", console=True)
        for first, last, emission in ((1, 3, 3), (0, 2, 4)):
            with self.subTest(first=first), self.assertRaises(ValueError):
                analyze_frame_classes([class_window(first, last, emission)])

    def test_coverage_gaps_are_unknown_and_overlap_rejected(self):
        first = class_window()
        self.assertTrue(analyze_frame_classes([first, class_window(6, 8, 9)])["warnings"])
        with self.assertRaises(ValueError):
            analyze_frame_classes([first, class_window(3, 5, 6)])

    def test_partial_native_data_cannot_establish_native_average(self):
        window = class_window()
        window["classes"]["redraw"].update(native_frames=2, native_available=False)
        redraw = analyze_frame_classes([window])["totals"]["redraw"]
        self.assertIsNone(redraw["average_end_scene_ms"])
        self.assertIsNone(redraw["average_wait_ms"])
        self.assertEqual(redraw["average_work_ms"], 30)

    def test_old_captures_cannot_establish_per_class_savings(self):
        old = module.analyze(fixture(), "hardware")["frame_classes"]
        new = analyze_frame_classes([class_window()])
        self.assertFalse(old["available"])
        result = comparison.compare_frame_classes(dict(frame_classes=old), dict(frame_classes=new))
        self.assertFalse(result["available"])
        self.assertEqual(result["classes"], {})

    def test_comparison_uses_observed_per_class_counts(self):
        before = analyze_frame_classes([class_window()])
        window = class_window()
        window["classes"]["redraw"]["work_us"] = 60000
        after = analyze_frame_classes([window])
        result = comparison.compare_frame_classes(dict(frame_classes=before), dict(frame_classes=after))
        self.assertEqual(result["classes"]["redraw"]["work_saved_ms"], 10)
        self.assertFalse(result["classes"]["reuse"]["available"])

    def test_worst_details_and_ps19_capability_are_retained(self):
        worst = dict(class_window()["classes"]["redraw"], frame=3, scene=1, pc=246, frame_class="redraw")
        rows = [dict(kind="shader_capabilities", group15=1, ps_fetch17=0, ps_fetch19=1, ps_group19=1, ps_fetch11=1, ps_fetch13=0),
                dict(kind="audit", frame=4, phase="window", banks=0, hotspots=0, native_available=True, worst=worst)]
        result = module.analyze(self.capture(rows), "hardware", console=True)
        self.assertEqual(result["copy_audit"]["windows"][0]["worst"], worst)
        self.assertEqual(result["shader_capabilities"][0]["ps_fetch19"], 1)
        self.assertEqual(result["shader_capabilities"][0]["ps_group19"], 1)
        self.assertEqual(result["shader_capabilities"][0]["ps_fetch11"], 1)
        self.assertEqual(result["shader_capabilities"][0]["ps_fetch13"], 0)
        rows[0]["ps_fetch19"] = 2
        with self.assertRaises(ValueError):
            module.analyze(self.capture(rows), "hardware", console=True)


class NativeColorCapabilityTests(unittest.TestCase):
    def test_native_plain_capabilities_are_admitted_and_validated(self):
        data = ConsoleReaderTests.capture([dict(kind="header", schema=3),
            dict(kind="shader_capabilities", group15=1, ps_fetch17=1,
                 plain_sprite22=1, plain_ps19_23=1, color_contract="encoded-unorm8-uchar4")])
        result = module.analyze(data, "hardware", console=True)
        self.assertEqual(result["shader_capabilities"][0]["plain_ps19_23"], 1)
        with self.assertRaises(ValueError):
            module.analyze(data.replace(b'"plain_sprite22":1', b'"plain_sprite22":2'), "hardware", console=True)

if __name__ == "__main__":
    unittest.main()

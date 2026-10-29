"""Read an opt-in Vita performance.log offline. Host emulator times are not Vita speed.

The logger records window totals and cumulative image counters. Missing/truncated
records are reported explicitly. Memory figures exclude the process/decoder heap.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

MAX_LOG = 256 * 1024
COUNTERS = (
    "demand_decodes", "demand_us", "worker_reused", "demand_wait_us", "uploads", "upload_us",
    "prefetch_skipped", "worker_threads", "worker_completed", "worker_stale", "queue_full",
    "worker_failed", "worker_read_decode_us", "jpeg_hw_candidates", "jpeg_hw_ok",
    "jpeg_fallback_profile", "jpeg_fallback_busy", "jpeg_fallback_budget", "jpeg_fallback_init",
    "jpeg_fallback_alloc", "jpeg_fallback_driver", "jpeg_fallback_contract",
    "software_png_ok", "software_jpeg_ok",
    "audio_submitted", "audio_output", "audio_underruns", "audio_idle", "audio_queue_full",
    "audio_peak_queued", "audio_mix_blocks", "audio_mix_max_us", "audio_mix_late", "audio_clipped_samples",
    # schema 2: prove the pre-baked GXT route is actually used at runtime.
    "gxt_images", "decoded_images",
    # schema 2: the three stages stay separate -- card read, NX inflation, and the
    # CPU side of GPU synchronisation. None of them is a GPU execution time.
    "card_read_us", "card_read_calls", "card_read_bytes", "max_card_read_us",
    "nx_inflate_us", "nx_inflate_calls",
    "gpu_finish_us", "gpu_finish_calls", "gpu_end_scene_us", "gpu_end_scene_calls",
    "gpu_display_finish_us", "gpu_display_finish_calls",
    "gpu_target_create_us", "gpu_target_create_calls",
    "deferred_items", "deferred_item_us", "deferred_upload_us", "deferred_bake_us",
)
REQUIRED = ("elapsed_ms", "window_ms", "frames", "work_us", "wall_us", "update_us", "render_us",
            "max_work_us", "p95_work_upper_us", "over_16ms", "over_33ms")


def analyze(data: bytes, environment: str) -> dict:
    if len(data) > MAX_LOG:
        raise ValueError("performance.log exceeds the documented 256 KiB limit")
    if environment not in {"hardware", "emulator", "unspecified"}:
        raise ValueError("Unknown measurement environment")
    warnings, samples, header = [], [], None
    limit_reached = False
    lines = data.splitlines()
    for index, line in enumerate(lines):
        try:
            row = json.loads(line)
        except (ValueError, UnicodeError):
            if index == len(lines) - 1 and not data.endswith(b"\n"):
                warnings.append("Incomplete final record was excluded")
                break
            raise ValueError(f"Invalid JSON record at line {index + 1}") from None
        if not isinstance(row, dict):
            raise ValueError(f"Expected object at line {index + 1}")
        kind = row.get("kind")
        if kind == "header":
            if index or header is not None or row.get("schema") not in (1, 2):
                raise ValueError("Expected exactly one schema 1/2 header at the beginning")
            header = row
        elif kind == "sample":
            if header is None or limit_reached:
                raise ValueError("Sample outside header/limit boundaries")
            for key in REQUIRED:
                if type(row.get(key)) is not int or row[key] < 0:
                    raise ValueError(f"Missing or invalid {key} at line {index + 1}")
            if not row["frames"] or not row["window_ms"]:
                raise ValueError("Empty timing window")
            if samples and row["elapsed_ms"] <= samples[-1]["elapsed_ms"]:
                raise ValueError("Window times must increase")
            if row["over_33ms"] > row["over_16ms"] or row["over_16ms"] > row["frames"]:
                raise ValueError("Invalid frame threshold counts")
            for key, value in row.items():
                if key in COUNTERS and (type(value) is not int or value < 0):
                    raise ValueError(f"Invalid cumulative counter {key}")
            samples.append(row)
        elif kind == "limit":
            if not header or limit_reached or index != len(lines) - 1:
                raise ValueError("Misplaced log limit record")
            limit_reached = True
        else:
            raise ValueError(f"Unknown record kind at line {index + 1}")
    if header is None:
        raise ValueError("Missing performance header")
    if not samples:
        warnings.append("No complete five-second measurement window")
    total = lambda name: sum(row[name] for row in samples)
    peak = lambda name: max((row.get(name, 0) for row in samples), default=0)
    frames, work, wall = total("frames"), total("work_us"), total("wall_us")
    final = samples[-1] if samples else {}
    counters = {key: final[key] for key in COUNTERS if key in final}
    if samples:
        missing = sorted(set(COUNTERS) - set(counters))
        if missing:
            warnings.append("Counters absent from this log: " + ", ".join(missing))
    # Never silently turn a reset/wrap into an enormous per-window rate.
    discontinuities = []
    for before, after in zip(samples, samples[1:]):
        for key in COUNTERS:
            if key in before and key in after and after[key] < before[key]:
                discontinuities.append({"elapsed_ms": after["elapsed_ms"], "counter": key})
    if discontinuities:
        warnings.append("Cumulative counters reset or wrapped; totals are last observations only")
    violations = []
    for row in samples:
        for key, maximum in (("pending", 8), ("ready", 2), ("active", 1),
                             ("peak_pending", 8), ("peak_ready", 2), ("audio_peak_queued", 4),
                             ("jpeg_work_cdram", 3 * 1024**2), ("jpeg_peak_work_cdram", 3 * 1024**2)):
            if key in row and row[key] > maximum:
                violations.append({"elapsed_ms": row["elapsed_ms"], "field": key,
                                   "observed": row[key], "maximum": maximum})
    last_error = final.get("jpeg_last_hw_error")
    if final.get("audio_underruns", 0):
        warnings.append("Audio starvation observed; inspect its window/scene and mix latency on hardware")
    if final.get("audio_clipped_samples", 0):
        warnings.append("Mixer clipping observed; compare voice/music/effect levels with the audible event")
    return {
        "schema": "knife-vita-performance-summary-v1", "environment": environment,
        "environment_authority": "caller-declared; not inferred from frame times",
        "scope": "Observed log windows only; no full-game, visual correctness or hardware-speed certification",
        "log_bytes": len(data), "log_sha256": hashlib.sha256(data).hexdigest(),
        "header": header, "windows": len(samples), "frames": frames,
        # Derived only from the fields above; a zero means the site never ran.
        "stages": {
            "card_read_ms": round(final.get("card_read_us", 0) / 1000, 3),
            "card_read_calls": final.get("card_read_calls", 0),
            "card_read_bytes": final.get("card_read_bytes", 0),
            "max_card_read_ms": round(final.get("max_card_read_us", 0) / 1000, 3),
            "nx_inflate_ms": round(final.get("nx_inflate_us", 0) / 1000, 3),
            "gpu_finish_ms": round(final.get("gpu_finish_us", 0) / 1000, 3),
            "gpu_end_scene_ms": round(final.get("gpu_end_scene_us", 0) / 1000, 3),
            "gpu_display_finish_ms": round(final.get("gpu_display_finish_us", 0) / 1000, 3),
            "deferred_items": final.get("deferred_items", 0),
            "deferred_item_ms": round(final.get("deferred_item_us", 0) / 1000, 3),
            "deferred_upload_ms": round(final.get("deferred_upload_us", 0) / 1000, 3),
            "deferred_bake_ms": round(final.get("deferred_bake_us", 0) / 1000, 3),
            "note": "CPU-side observations: finish/display are blocked time, not GPU execution time",
        },
        "window_ms": total("window_ms"),
        "average_work_ms": round(work / frames / 1000, 3) if frames else None,
        "average_update_ms": round(total("update_us") / frames / 1000, 3) if frames else None,
        "average_render_submission_ms": round(total("render_us") / frames / 1000, 3) if frames else None,
        "observed_frame_rate": round(frames * 1_000_000 / wall, 3) if wall else None,
        "maximum_work_ms": peak("max_work_us") / 1000 if samples else None,
        "largest_window_p95_upper_ms": peak("p95_work_upper_us") / 1000 if samples else None,
        "percentile_scope": "Maximum of per-window histogram p95 upper bounds, not a merged global p95",
        "over_16_667_ms_frames": total("over_16ms"), "over_33_333_ms_frames": total("over_33ms"),
        "last_cumulative_counters": counters,
        "audio": {
            "underrun_blocks": final.get("audio_underruns"),
            "intentional_idle_blocks": final.get("audio_idle"),
            "late_mix_blocks": final.get("audio_mix_late"),
            "maximum_mix_ms": final["audio_mix_max_us"] / 1000 if "audio_mix_max_us" in final else None,
            "clipped_samples": final.get("audio_clipped_samples"),
            "scope": "Last cumulative observations; 20 ms PCM blocks, queue-full is producer backpressure, mix time includes decode but excludes control/file-open work; absent counters are unknown",
        },
        "jpeg_last_hw_error_hex": f"0x{last_error:08X}" if last_error is not None else None,
        "jpeg_last_rejected_contract": {key: final[key] for key in (
            "jpeg_contract_mask", "jpeg_info_size", "jpeg_info_y_pitch",
            "jpeg_info_chroma_pitch", "jpeg_info_color_space") if key in final},
        "sampled_memory_peaks_bytes": {key: peak(key) for key in (
            "gxm_main_mapped_bytes", "gxm_cdram_mapped_bytes", "gxm_parameter_reserved_bytes",
            "jpeg_work_cdram", "jpeg_peak_work_cdram") if any(key in row for row in samples)},
        "memory_scope": "Tracked GXM mappings/parameter reservation and separate JPEG work only; excludes Rust heap, stacks and codec heap; separate peaks need not coincide",
        "queue_and_jpeg_bound_violations": violations,
        "counter_discontinuities": discontinuities, "limit_reached": limit_reached,
        "warnings": warnings,
        "windows_detail": [{key: row[key] for key in ("elapsed_ms", "window_ms", "scene", "pc", "frames",
            "max_work_us", "p95_work_upper_us", "pending", "ready", "active", "jpeg_hw_ok",
            "jpeg_fallback_driver", "worker_failed", "audio_underruns", "audio_queue_full",
            "audio_mix_max_us", "audio_mix_late", "audio_clipped_samples") if key in row} for row in samples],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--environment", choices=("hardware", "emulator", "unspecified"), default="unspecified")
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    # Bound reads even for a mistakenly selected large file.
    with args.log.open("rb") as stream:
        result = analyze(stream.read(MAX_LOG + 1), args.environment)
    text = json.dumps(result, ensure_ascii=False, indent=2) + "\n"
    if args.out:
        args.out.write_text(text, encoding="utf-8")
    print(text, end="")
    return 1 if result["queue_and_jpeg_bound_violations"] else 0


if __name__ == "__main__":
    raise SystemExit(main())

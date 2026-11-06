"""Offline analysis for the fixed Vita GPU replay diagnostic.

This tool consumes only retained console/performance text. It never executes a
Vita binary and never promotes an optimization. Hardware acceptance requires a
header with build/clock identity, all four ABBA blocks, exact RGBA8 equality,
and the measured timing gates. Emulator and unspecified captures remain
inconclusive even when their local gates pass.
"""
from __future__ import annotations

import argparse
import json
import statistics
import sys
from pathlib import Path

MAX_INPUT = 16 * 1024 * 1024
MAX_OUTPUT = 128 * 1024
MASK_PLAIN = 1 << 30
MASK_FULL = 1 << 37
MASK_ALL = 1 << 38
MASK_PLAIN7 = 1 << 39
MASK_CHAIN7 = 1 << 40
MASK_MATERIAL = 1 << 41
ALL_CENSUS_MASKS = (MASK_ALL, MASK_PLAIN7, MASK_CHAIN7, MASK_MATERIAL)
CHECKED_MASKS = (MASK_FULL, *ALL_CENSUS_MASKS)
TOGGLES = {"ps-plain-material": MASK_PLAIN, "ps-full-material": MASK_FULL,
           "ps-all-operations": MASK_ALL, "ps-plain-op7": MASK_PLAIN7, "ps-chain-op7": MASK_CHAIN7, "ps-material-variants": MASK_MATERIAL}
FULL_PROGRAMS = (54, 55, 56)
ABBA = (False, True, True, False)
BLOCKS = 4
MEASURED = 120


def _percentile(values: list[int], percent: int) -> int:
    if not values:
        raise ValueError("empty timing sample")
    ordered = sorted(values)
    # Nearest-rank percentile, with rank clamped to the observed sample set.
    rank = max(1, (len(ordered) * percent + 99) // 100)
    return ordered[rank - 1]


def _json_objects(data: bytes) -> tuple[list[dict], list[str]]:
    if len(data) > MAX_INPUT:
        raise ValueError("capture exceeds 16 MiB bound")
    errors: list[str] = []
    plain: list[str] = []
    chunks: dict[int, dict[int, str]] = {}
    chunk_totals: dict[int, int] = {}
    for line_no, raw in enumerate(data.decode("utf-8", errors="replace").splitlines(), 1):
        text = raw.strip()
        if not text:
            continue
        # sceClibPrintf captures may carry a prefix before the JSON object.
        start = text.find("{")
        if start >= 0:
            text = text[start:]
        try:
            row = json.loads(text)
        except json.JSONDecodeError:
            errors.append(f"invalid JSON at line {line_no}")
            continue
        if not isinstance(row, dict):
            errors.append(f"non-object JSON at line {line_no}")
            continue
        if row.get("kind") == "audit_chunk":
            try:
                seq, part, total, payload = (row["seq"], row["part"], row["total"], row["payload"])
                if not all(type(v) is int and v >= 0 for v in (seq, part, total)) or not isinstance(payload, str):
                    raise ValueError
                if total == 0 or part >= total or len(chunks.setdefault(seq, {})) >= total:
                    raise ValueError
                chunks[seq][part] = payload
                chunk_totals[seq] = total
            except (KeyError, ValueError):
                errors.append(f"invalid audit chunk at line {line_no}")
            continue
        plain.append(text)
    for seq in sorted(chunks):
        parts, total = chunks[seq], chunk_totals[seq]
        if set(parts) != set(range(total)):
            errors.append(f"incomplete audit chunk sequence {seq}")
            continue
        payload = "".join(parts[i] for i in range(total))
        plain.extend(payload.splitlines())
    objects: list[dict] = []
    for text in plain:
        try:
            row = json.loads(text)
        except json.JSONDecodeError:
            errors.append("reassembled audit payload is not JSON")
            continue
        if isinstance(row, dict):
            objects.append(row)
        else:
            errors.append("reassembled audit payload is not an object")
    return objects, errors


def _work_delta(before: dict, after: dict) -> dict:
    if not isinstance(before, dict) or not isinstance(after, dict):
        raise ValueError("work snapshots must be objects")
    if before.get("schema") != after.get("schema"):
        raise ValueError("work snapshot schema changed inside replay block")
    bt, at = before.get("totals"), after.get("totals")
    if not (isinstance(bt, list) and isinstance(at, list) and len(bt) >= 2 and len(at) >= 2):
        raise ValueError("work snapshots have no totals")
    if any(type(v) is not int or v < 0 for v in bt[:2] + at[:2]):
        raise ValueError("work totals must be non-negative integers")
    if at[0] < bt[0] or at[1] < bt[1]:
        raise ValueError("work totals moved backwards")
    return {"calls": at[0] - bt[0], "sources": at[1] - bt[1],
            "before": bt[:3], "after": at[:3]}


def _full_program_delta(before: dict, after: dict, program_ids=FULL_PROGRAMS) -> dict[str, int]:
    counts = []
    for snapshot in (before, after):
        if snapshot.get("schema") != 3 or snapshot.get("scope") != "successful-native-draws-cumulative":
            raise ValueError("full program snapshot has unknown schema or scope")
        if type(snapshot.get("program_omitted")) is not int or snapshot["program_omitted"] != 0:
            raise ValueError("full program counter coverage is missing or omitted")
        rows = snapshot.get("program_counts")
        if not isinstance(rows, list):
            raise ValueError("full program counters are missing")
        programs = {}
        for row in rows:
            if (not isinstance(row, list) or len(row) != 2 or
                    any(type(v) is not int or v < 0 for v in row) or row[0] >= 128 or row[0] in programs):
                raise ValueError("full program counters are invalid or duplicated")
            programs[row[0]] = row[1]
        counts.append(programs)
    delta = {str(program): counts[1].get(program, 0) - counts[0].get(program, 0) for program in program_ids}
    if any(count < 0 for count in delta.values()):
        raise ValueError("full program counters moved backwards")
    calls = after["totals"][0] - before["totals"][0]
    if sum(delta.values()) > calls:
        raise ValueError("full program calls exceed total native draws")
    return delta


def _block(row: dict, expected_index: int, expected_candidate: bool, baseline: int, origin: dict,
           toggle_mask: int = MASK_PLAIN) -> tuple[dict, list[str]]:
    reasons: list[str] = []
    if row.get("type") != "gpu_replay_block" or row.get("index") != expected_index:
        reasons.append("missing_or_misordered_block")
    if row.get("candidate") is not expected_candidate:
        reasons.append("abba_candidate_order_mismatch")
    if type(row.get("mask")) is not int or row.get("mask") != baseline | (toggle_mask if expected_candidate else 0):
        reasons.append("mask_mismatch")
    if row.get("bytes") != 960 * 540 * 4 or row.get("differing_bytes") != 0 or row.get("differing_pixels") != 0:
        reasons.append("rgba8_mismatch")
    if row.get("maximum_channel_delta") != 0 or row.get("absolute_channel_delta") != 0:
        reasons.append("rgba8_channel_mismatch")
    samples = row.get("submit_complete_us")
    if not isinstance(samples, list) or len(samples) != MEASURED or any(type(v) is not int or v < 0 for v in samples):
        reasons.append("missing_or_invalid_completion_samples")
        samples = []
    else:
        row["submit_complete_us"] = samples
    try:
        delta = _work_delta(row.get("executed_work_before"), row.get("executed_work_after"))
        if delta["calls"] <= 0:
            reasons.append("no_executed_work")
        if toggle_mask in CHECKED_MASKS:
            ids = range(88,105) if toggle_mask == MASK_MATERIAL else (48,) if toggle_mask == MASK_PLAIN7 else (57,) if toggle_mask == MASK_CHAIN7 else range(64,88) if toggle_mask == MASK_ALL else FULL_PROGRAMS
            delta["full_programs"] = _full_program_delta(row["executed_work_before"], row["executed_work_after"], ids)
            exercised = sum(delta["full_programs"].values())
            if expected_candidate and exercised == 0:
                reasons.append("no_full_material_program_executed")
            elif not expected_candidate and exercised != 0:
                reasons.append("full_material_program_executed_in_baseline")
    except ValueError as error:
        reasons.append(f"invalid_work_snapshot:{error}")
        delta = {"calls": 0, "sources": 0}
    if origin and (row.get("scene") is not None or row.get("pc") is not None):
        if row.get("scene") != origin.get("scene") or row.get("pc") != origin.get("pc"):
            reasons.append("origin_changed_inside_run")
    return {"index": expected_index, "candidate": expected_candidate, "samples": samples,
            "timing": {"n": len(samples), "median_us": statistics.median(samples) if samples else None,
                        "p95_us": _percentile(samples, 95) if samples else None,
                        "p99_us": _percentile(samples, 99) if samples else None},
            "rgba8": {k: row.get(k) for k in ("bytes", "differing_bytes", "differing_pixels",
                                                "maximum_channel_delta", "absolute_channel_delta", "first_byte", "fnv1a64")},
            "work": delta}, reasons


def _run(header: dict, replay_header: dict, rows: list[dict]) -> dict:
    reasons: list[str] = []
    origin = {key: replay_header.get(key) for key in ("scene", "pc", "update_frame")}
    if replay_header.get("order") != "ABBA" or replay_header.get("warm_per_block") != 30 or replay_header.get("measure_per_block") != MEASURED:
        reasons.append("replay_schedule_mismatch")
    toggle = replay_header.get("toggle")
    toggle_mask = TOGGLES.get(toggle, MASK_PLAIN)
    if toggle not in TOGGLES or replay_header.get("exact_equal") is not True:
        reasons.append("replay_contract_mismatch")
    if "toggle_mask" in replay_header and replay_header["toggle_mask"] != toggle_mask:
        reasons.append("replay_toggle_mask_mismatch")
    candidate_field = "all_input_candidates" if toggle_mask in ALL_CENSUS_MASKS else "full_input_candidates" if toggle_mask == MASK_FULL else "plain_input_candidates"
    candidates = replay_header.get(candidate_field)
    if (not isinstance(candidates, list) or len(candidates) != (24 if toggle_mask in ALL_CENSUS_MASKS else 3) or
            any(type(v) is not int or v < 0 for v in candidates) or not any(candidates)):
        reasons.append("no_full_material_input_candidate" if toggle_mask in CHECKED_MASKS else "no_plain_material_input_candidate")
    elif toggle_mask in (MASK_PLAIN7, MASK_CHAIN7) and candidates[6] == 0:
        reasons.append("no_full_material_input_candidate")
    if toggle_mask in CHECKED_MASKS and replay_header.get("full_input_candidate_scope") != "Stage,Pixel,Mask,Composite":
        reasons.append("full_material_candidate_scope_unknown")
    baseline = replay_header.get("baseline_mask")
    if type(baseline) is not int or baseline < 0 or baseline >= 1 << 64 or baseline & toggle_mask:
        reasons.append("invalid_baseline_mask")
        baseline = 0
    blocks: list[dict] = []
    for index, expected in enumerate(ABBA):
        row = rows[index] if index < len(rows) else {}
        parsed, block_reasons = _block(row, index, expected, baseline, origin, toggle_mask)
        blocks.append(parsed); reasons.extend(block_reasons)
    if len(rows) != BLOCKS:
        reasons.append("wrong_block_count")
    base_samples = blocks[0]["samples"] + blocks[3]["samples"]
    candidate_samples = blocks[1]["samples"] + blocks[2]["samples"]
    performance = {"status": "inconclusive", "baseline": {}, "candidate": {}}
    if len(base_samples) == len(candidate_samples) == 2 * MEASURED:
        base = {"n": len(base_samples), "median_us": statistics.median(base_samples),
                "p95_us": _percentile(base_samples, 95), "p99_us": _percentile(base_samples, 99)}
        cand = {"n": len(candidate_samples), "median_us": statistics.median(candidate_samples),
                "p95_us": _percentile(candidate_samples, 95), "p99_us": _percentile(candidate_samples, 99)}
        gain = (base["median_us"] - cand["median_us"]) * 100 / base["median_us"] if base["median_us"] else 0
        performance = {"status": "pass" if gain >= 5 and cand["p95_us"] <= base["p95_us"] * 1.05 and cand["p99_us"] <= base["p99_us"] * 1.10 else "fail",
                       "baseline": base, "candidate": cand, "median_gain_percent": gain,
                       "gates": {"median_gain_at_least_percent": 5, "p95_regression_at_most_percent": 5,
                                 "p99_regression_at_most_percent": 10}}
        if gain < 5: reasons.append("median_gain_below_5_percent")
        if cand["p95_us"] > base["p95_us"] * 1.05: reasons.append("p95_regression_above_5_percent")
        if cand["p99_us"] > base["p99_us"] * 1.10: reasons.append("p99_regression_above_10_percent")
    elif not any(reason == "missing_or_invalid_completion_samples" for reason in reasons):
        reasons.append("incomplete_timing_samples")
    # A timing difference cannot accept a full-source candidate that was never
    # selected, or whose executed-program evidence is incomplete. Keep observed
    # sample statistics but withhold the performance verdict in that case.
    if toggle_mask in CHECKED_MASKS and any(
            reason in {"no_full_material_input_candidate", "no_full_material_program_executed",
                       "full_material_program_executed_in_baseline", "full_material_candidate_scope_unknown",
                       "replay_toggle_mask_mismatch", "invalid_baseline_mask", "mask_mismatch"}
            or reason.startswith("invalid_work_snapshot") for reason in reasons):
        performance["status"] = "inconclusive"
    return {"origin": origin, "toggle": toggle, "toggle_mask": toggle_mask,
            candidate_field: candidates, "performance": performance, "blocks": blocks,
            "work": [block["work"] for block in blocks],
            "failure_reasons": sorted(set(reasons))}


def analyze(data: bytes, environment: str) -> dict:
    objects, transport_errors = _json_objects(data)
    header = next((row for row in objects if row.get("kind") == "header"), None)
    if header is None:
        return {"status": "inconclusive", "transport_errors": transport_errors + ["missing_header"], "runs": [], "replay_errors": []}
    if header.get("schema") != 3:
        transport_errors.append("invalid_header_schema")
    runs: list[dict] = []
    replay_errors: list[str] = []
    current: tuple[dict, list[dict]] | None = None
    for row in objects:
        if row.get("type") == "gpu_replay":
            if current is not None:
                runs.append(_run(header, current[0], current[1]))
            current = (row, [])
        elif row.get("type") == "gpu_replay_block":
            if current is not None: current[1].append(row)
        elif row.get("kind") == "gpu_replay_error":
            replay_errors.append(str(row.get("error", "unknown replay error")))
    if current is not None:
        runs.append(_run(header, current[0], current[1]))
    if not runs:
        transport_errors.append("missing_gpu_replay_run")
    elif any(run["failure_reasons"] for run in runs):
        pass
    hardware_identity = environment == "hardware" and all(type(header.get(k)) is int and header[k] > 0 for k in ("arm_mhz", "bus_mhz", "gpu_mhz"))
    statuses = [run["performance"]["status"] for run in runs]
    exact = all(not any(reason.startswith("rgba8_") for reason in run["failure_reasons"]) for run in runs)
    hard_reasons = {"missing_or_misordered_block", "abba_candidate_order_mismatch", "mask_mismatch",
                    "missing_or_invalid_completion_samples", "incomplete_timing_samples",
                    "wrong_block_count", "no_plain_material_input_candidate", "invalid_baseline_mask",
                    "no_executed_work", "no_full_material_input_candidate", "no_full_material_program_executed",
                    "full_material_program_executed_in_baseline", "full_material_candidate_scope_unknown",
                    "replay_toggle_mask_mismatch"}
    malformed = any(reason in hard_reasons or reason.startswith("invalid_work_snapshot") for run in runs for reason in run["failure_reasons"])
    visual_or_gate_failure = any(any(reason.startswith("rgba8_") or reason.endswith("_regression_above_5_percent")
                                     or reason.endswith("_regression_above_10_percent") or reason == "median_gain_below_5_percent"
                                     for reason in run["failure_reasons"]) for run in runs)
    status = "pass" if runs and not transport_errors and not replay_errors and hardware_identity and exact and all(s == "pass" for s in statuses) and all(not run["failure_reasons"] for run in runs) else "fail" if runs and not malformed and visual_or_gate_failure else "inconclusive"
    if environment != "hardware" and status == "pass": status = "inconclusive"
    return {"schema": "knife-vita-gpu-replay-analysis-v1", "status": status, "environment": environment,
            "default_promotion_applied": False, "global_visual_correctness_verified": False,
            "header": {k: header.get(k) for k in ("build_tag", "arm_mhz", "bus_mhz", "gpu_mhz", "asset_manifest_sha256")},
            "runs": runs, "replay_errors": replay_errors, "transport_errors": transport_errors,
            "limitations": ["Frozen current production Stage only; no scene38 auto-start or whole-story claim.",
                            "Timing gates cover recorded/submit-complete samples only; they do not establish global frame pacing.",
                            "RGBA8 equality is limited to the captured Stage output and does not prove UI, movie, audio, or save behavior."]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("capture", type=Path)
    parser.add_argument("--environment", choices=("hardware", "emulator", "unspecified"), default="unspecified")
    args = parser.parse_args()
    try:
        result = analyze(args.capture.read_bytes(), args.environment)
    except (OSError, ValueError) as error:
        print(json.dumps({"schema": "knife-vita-gpu-replay-analysis-v1", "status": "inconclusive", "error": str(error), "default_promotion_applied": False}, separators=(",", ":")))
        return 2
    output = json.dumps(result, ensure_ascii=True, separators=(",", ":"))
    if len(output.encode()) > MAX_OUTPUT:
        raise SystemExit("analysis output exceeds 128 KiB bound")
    print(output)
    return 0 if result["status"] == "pass" else 1 if result["status"] == "fail" else 2


if __name__ == "__main__":
    raise SystemExit(main())

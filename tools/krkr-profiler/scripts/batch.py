"""Run scripted game cases and rank their measured graphics/VM workload."""
import argparse
import bisect
import collections
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import re
import shutil
import subprocess
import threading


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2), encoding="utf-8")


def scan(root, output):
    pages = []
    scripts = []
    for path in sorted(root.rglob("*.ks")):
        source = path.read_text(encoding="utf-8-sig")
        scripts.append({"file": path.relative_to(root).as_posix(),
                        "commands": dict(collections.Counter(re.findall(r"(?m)^@(\w+)", source)))})
        for section in re.split(r"(?m)^(?=\*page\d+)", source):
            label = re.match(r"\*(page\d+)", section)
            if not label:
                continue
            commands = collections.Counter(re.findall(r"(?m)^@(\w+)", section))
            effects = collections.Counter(re.findall(
                r"\b(blur|xblur|yblur|haze|particle|rotate|zoomx|zoomy|effect|contrast|brightness)=", section))
            pages.append({"file": path.relative_to(root).as_posix(), "script": path.name, "label": "*" + label[1],
                          "commands": dict(commands), "effects": dict(effects),
                          "effect_parameters": sum(effects.values()),
                          "animations": commands["fgact"] + commands["bgact"],
                          "images": commands["fg"] + commands["bg"] + commands["chgfg"]})
    pages.sort(key=lambda p: (p["effect_parameters"], p["animations"], p["images"]), reverse=True)
    interfaces = []
    for path in sorted(root.rglob("*.tjs")):
        entries = re.findall(r"(?m)^\s*function\s+((?:open|close|show|hide|ask|CFopenPage|changePage)[\w]*)\s*\(",
                             path.read_text(encoding="utf-8-sig"))
        if entries:
            interfaces.append({"file": path.relative_to(root).as_posix(), "entries": entries})
    save(output, {"kind": "static candidates, not measured performance", "pages": pages,
                  "scripts": scripts, "interfaces": interfaces})
    print(f"Scanned {len(pages)} pages; candidates: {output}", flush=True)


def analyze(directory, case, compact=False):
    events_path = directory / "events.jsonl"
    cached = directory / "analysis.json"
    if not events_path.exists() and cached.exists():
        analysis = json.loads(cached.read_text(encoding="utf-8"))
        if analysis["case"] != case:
            raise ValueError(f"compact recording has different case settings: {directory}")
        return analysis["row"]
    run_path = directory / "run.json"
    process_path = directory / "process.json"
    if not events_path.exists() or not (run_path.exists() or process_path.exists()):
        return {"case": case["name"], "error": "no recording", "segments": []}
    events = []
    with events_path.open(encoding="utf-8") as source:
        for line in source:
            try:
                events.append(json.loads(line))
            except json.JSONDecodeError:
                if run_path.exists() or line.endswith('\n'):
                    raise
                # A terminated writer can leave its final record unfinished.
    partial = not run_path.exists()
    run = json.loads(run_path.read_text(encoding="utf-8")) if not partial else {
        "elapsed_ms": max((e.get("at_ns", e.get("start_ns", 0)) + e.get("duration_ns", 0)
                           for e in events), default=0)/1e6,
        "error": "recording interrupted; values cover only captured events",
        "dropped_events": None,
    }
    process = json.loads(process_path.read_text(encoding="utf-8")) if process_path.exists() else {}
    markers = [e for e in events if e["type"] == "marker"]
    messages = "\n".join(e.get("detail", "") for e in markers)
    missing = [s for s in case.get("expect", []) if s not in messages]
    failure = [e["detail"] for e in markers if "PERF-FAIL:" in e.get("detail", "")]
    bounds = [(0, "startup")]
    for e in sorted(markers, key=lambda e: e["at_ns"]):
        if match := re.search(r"PERF: ([\w.-]+)", e.get("detail", "")):
            bounds.append((e["at_ns"], match[1]))
        elif case.get("pages") and (match := re.search(
            r"\b([\w.-]+\.ks)\s*:\s*(\*page\d+)\|", e.get("detail", ""))):
            bounds.append((e["at_ns"], match[1] + match[2]))
    bounds.append((int(run["elapsed_ms"] * 1e6), "end"))
    spans = [e for e in events if e["type"] == "span"]
    captures = [e for e in spans if e["name"] == "capture.screenshot"]
    spans = [e for e in spans if not any(
        e["thread"] == c["thread"] and c["start_ns"] <= e["start_ns"] < c["start_ns"] + c["duration_ns"]
        for c in captures)]
    spans.sort(key=lambda e: e["start_ns"])
    span_times = [e["start_ns"] for e in spans]
    # Attribute GL transfers to the command/render span that submitted them.
    # Prefix sums keep this linearithmic even for effect-heavy recordings.
    transfers = collections.defaultdict(list)
    transfer_names = {"gl.copy_tex_sub_image": "copy_calls", "gl.finish": "finishes",
                      "gl.read_pixels": "readbacks", "gl.tex_image": "allocations"}
    for e in spans:
        if e["name"] in transfer_names:
            transfers[e["thread"]].append(e)
    indexed = {}
    for thread, values in transfers.items():
        values.sort(key=lambda e: e["start_ns"])
        totals = {key: [0] for key in [*transfer_names.values(), "copy_bytes"]}
        for e in values:
            for samples in totals.values():
                samples.append(samples[-1])
            totals[transfer_names[e["name"]]][-1] += 1
            if e["name"] == "gl.copy_tex_sub_image" and (size := re.search(
                    r"size=(\d+)x(\d+)", e.get("detail", ""))):
                totals["copy_bytes"][-1] += int(size[1]) * int(size[2]) * 4
        indexed[thread] = ([e["start_ns"] for e in values], totals)
    counters = collections.defaultdict(list)
    for e in events:
        if e["type"] == "counter":
            counters[e["name"]].append((e["at_ns"], e["value"]))
    for values in counters.values():
        values.sort()
    counter_times = {key: [at for at, _ in values] for key, values in counters.items()}
    segments = []
    for (lo, name), (hi, _) in zip(bounds, bounds[1:]):
        if hi <= lo:
            continue
        selected = spans[bisect.bisect_left(span_times, lo):bisect.bisect_left(span_times, hi)]
        counts = collections.Counter(e["name"] for e in selected)
        metrics = {}
        for key, values in counters.items():
            first = bisect.bisect_right(counter_times[key], lo)
            last = bisect.bisect_right(counter_times[key], hi)
            prior = [values[first-1][1]] if first else []
            inside = [v for _, v in values[first:last]]
            samples = ([] if key.startswith("frame.") else ([prior[-1]] if prior else [])) + inside
            if samples:
                metrics[key] = {"peak": max(samples), "delta": samples[-1] - (prior[-1] if prior else 0)}
                if key.startswith("frame."):
                    ordered = sorted(samples)
                    metrics[key].update(mean=sum(samples)/len(samples),
                                        p95=ordered[(len(ordered)*95+99)//100-1])
        copies = [e for e in selected if e["name"] == "gl.copy_tex_sub_image"]
        copy_bytes = 0
        for e in copies:
            if size := re.search(r"size=(\d+)x(\d+)", e.get("detail", "")):
                copy_bytes += int(size[1]) * int(size[2]) * 4
        work = collections.defaultdict(lambda: {"count": 0, "pc_total_ms": 0., "pc_max_ms": 0.})
        resources = collections.defaultdict(lambda: {"count": 0, "encoded_bytes": 0, "pc_total_ms": 0.})
        for e in selected:
            stage = e["name"]
            if stage == "image.prepare" and (resource := re.fullmatch(
                    r"name=(.*) bytes=(\d+)", e.get("detail", ""))):
                resource_row = resources[resource[1]]
                resource_row["count"] += 1
                resource_row["encoded_bytes"] += int(resource[2])
                resource_row["pc_total_ms"] += e["duration_ns"] / 1e6
            if not (stage in ("render", "vm-poll", "graphics-work", "gc", "io-work",
                              "storage-search", "storage-context", "storage-candidate", "script-bytes")
                    or stage.startswith(("gpu.", "image.", "graphics."))):
                continue
            if stage == "graphics-work":
                stage += " " + e.get("detail", "")
            elif stage == "io-work":
                stage += " " + next((p for p in e.get("detail", "").split()
                                      if p.startswith("kind=")), "kind=unknown")
            elif stage == "gpu.adjust":
                stage += " " + e.get("detail", "").split(" logical=")[0]
            row = work[stage]
            ms = e["duration_ns"] / 1e6
            row["count"] += 1
            row["pc_total_ms"] += ms
            row["pc_max_ms"] = max(row["pc_max_ms"], ms)
            if e["thread"] in indexed:
                times, totals = indexed[e["thread"]]
                start = bisect.bisect_left(times, e["start_ns"])
                end = bisect.bisect_left(times, e["start_ns"] + e["duration_ns"])
                for key, samples in totals.items():
                    row[key] = row.get(key, 0) + samples[end] - samples[start]
        segments.append({"name": name, "from_ms": lo / 1e6, "to_ms": hi / 1e6,
                         "copy_bytes": copy_bytes, "copy_calls": len(copies),
                         "finishes": counts["gl.finish"], "readbacks": counts["gl.read_pixels"],
                         "allocations": counts["gl.tex_image"], "presents": counts["host.present"],
                         "counters": metrics, "stages": dict(work), "resources": dict(resources)})
    error = run.get("error")
    if process.get("timed_out") or process.get("exit_code",0):
        error = error or f"process failed: {process}"
    row = {"case": case["name"], "error": error, "missing": missing,
            "failures": failure, "dropped_events": run["dropped_events"],
            "partial_recording": partial, "segments": segments}
    if compact:
        save(cached, {"case": case, "row": row})
        save(directory / "markers.json", markers)
        events_path.unlink()
        (directory / "trace.json").unlink(missing_ok=True)
    return row


def report(output, manifest, completed_only=False):
    cases = manifest["cases"]
    if completed_only:
        # Read only published compact summaries. Do not parse a live chapter's
        # growing timeline or wait for the entire batch to choose hotspots.
        cases = [case for case in cases if (output / case["name"] / "analysis.json").exists()]
    rows = [analyze(output / case["name"], case) for case in cases]
    recorded_manifest = output / "cases.json"
    jobs = (json.loads(recorded_manifest.read_text(encoding="utf-8")).get("parallel_jobs", 1)
            if recorded_manifest.exists() else manifest.get("parallel_jobs", 1))
    write_report(output, rows, jobs)


def merge(output, inputs):
    rows = []
    cases = []
    jobs = 1
    names = set()
    for directory in inputs:
        manifest = json.loads((directory / "cases.json").read_text(encoding="utf-8"))
        jobs = max(jobs, manifest.get("parallel_jobs", 1))
        for row in json.loads((directory / "batch.json").read_text(encoding="utf-8")):
            if row["case"] in names:
                raise ValueError(f'duplicate case: {row["case"]}; merge distinct workloads, not before/after runs')
            names.add(row["case"])
            rows.append({**row, "recording": str((directory / row["case"]).resolve())})
            case = next(c for c in manifest["cases"] if c["name"] == row["case"])
            cases.append({**case, "recording": rows[-1]["recording"]})
    output.mkdir(parents=True, exist_ok=True)
    save(output / "sources.json", [str(p.resolve()) for p in inputs])
    save(output / "cases.json", {"cases": cases, "parallel_jobs": jobs})
    write_report(output, rows, jobs)


def write_report(output, rows, parallel_jobs=1):
    save(output / "batch.json", rows)
    text = ["# Game workload\n", "PC timings are not Vita frame times. Memory is engine accounting; driver allocations are excluded.\n",
            "| Case / phase | Status | Graphics MiB | Scratch MiB | Copy MiB | Copies | Waits | Readbacks | VM work |",
            "|---|---|---:|---:|---:|---:|---:|---:|---:|"]
    ranked = []
    for row in rows:
        valid = not (row.get("error") or row.get("missing") or row.get("failures") or row.get("dropped_events"))
        story_start = next((s["from_ms"] for s in row["segments"] if s["name"] == "story"), 0)
        for segment in row["segments"]:
            metrics = segment["counters"]
            peak = lambda key: metrics.get(key, {}).get("peak", 0) / 1048576
            vm = metrics.get("vm.work", {}).get("delta", 0)
            text.append(f'| {row["case"]} / {segment["name"]} | {"recorded" if valid else "incomplete"} | '
                        f'{peak("memory.graphics_bytes"):.2f} | {peak("memory.scratch_bytes"):.2f} | '
                        f'{segment["copy_bytes"] / 1048576:.2f} | {segment["copy_calls"]} | '
                        f'{segment["finishes"]} | {segment["readbacks"]} | {vm} |')
            if (not row.get("dropped_events") and not row.get("partial_recording")
                    and segment["name"] != "startup"
                    and segment["from_ms"] >= story_start):
                ranked.append({"case": row["case"], "complete_case": valid, **segment})
        if not valid:
            text.append(f'\n{row["case"]}: {row.get("error") or row.get("missing") or row.get("failures") or "dropped events"}\n')
    ranked.sort(key=lambda s: s["copy_bytes"], reverse=True)
    save(output / "ranked.json", ranked)
    save(output / "rankings.json", {
        "copies": ranked,
        "readbacks": sorted(ranked, key=lambda s:s["readbacks"], reverse=True),
        "waits": sorted(ranked, key=lambda s:s["finishes"], reverse=True),
        "memory": sorted(ranked, key=lambda s:s["counters"].get("memory.graphics_bytes",{}).get("peak",0), reverse=True),
        "vm_work": sorted(ranked, key=lambda s:s["counters"].get("vm.work",{}).get("delta",0), reverse=True),
        "frame_copy_p95": sorted(ranked, key=lambda s:s["counters"].get("frame.copy_bytes",{}).get("p95",0), reverse=True),
    })
    (output / "batch.md").write_text("\n".join(text) + "\n", encoding="utf-8")
    frames = ["# Work between frame submissions\n",
              "Includes image updates and composition since the previous submission. These are work counts, not predicted Vita frame times.\n",
              "| Case / phase | Seconds | Frames | Copy MiB p95 / max | Draws p95 / max | Waits max | Readbacks max |",
              "|---|---:|---:|---:|---:|---:|---:|"]
    for segment in ranked:
        c = segment["counters"]
        if "frame.copy_bytes" not in c:
            continue
        pair = lambda key, scale: f'{c.get(key,{}).get("p95",0)/scale:.1f} / {c.get(key,{}).get("peak",0)/scale:.1f}'
        frames.append(f'| {segment["case"]} / {segment["name"]} | {(segment["to_ms"]-segment["from_ms"])/1000:.2f} | '
                      f'{segment["presents"]} | {pair("frame.copy_bytes",1048576)} | {pair("frame.draws",1)} | '
                      f'{c.get("frame.finishes",{}).get("peak",0)} | {c.get("frame.readbacks",{}).get("peak",0)} |')
    (output / "frame-work.md").write_text("\n".join(frames)+"\n",encoding="utf-8")
    merged = collections.defaultdict(lambda: {"count": 0, "pc_total_ms": 0.,
        "pc_max_ms": 0., "copy_bytes": 0, "copy_calls": 0, "finishes": 0,
        "readbacks": 0, "allocations": 0, "cases": set()})
    resources = collections.defaultdict(lambda: {"count": 0, "encoded_bytes": 0, "pc_total_ms": 0.})
    for segment in ranked:
        for name, values in segment.get("resources", {}).items():
            for key, value in values.items():
                resources[name][key] += value
        for stage, values in segment["stages"].items():
            row = merged[stage]
            row["cases"].add(segment["case"])
            for key in row.keys() - {"cases", "pc_max_ms"}:
                row[key] += values.get(key, 0)
            row["pc_max_ms"] = max(row["pc_max_ms"], values["pc_max_ms"])
    combined = [dict(stage=stage, **{**values, "cases": sorted(values["cases"])})
                for stage, values in merged.items()]
    save(output / "hotspots.json", {
        key: sorted(combined, key=lambda row: row[key], reverse=True)
        for key in ("copy_bytes", "copy_calls", "finishes", "readbacks", "pc_total_ms")})
    save(output / "resources.json", sorted(
        [dict(name=name, **values) for name, values in resources.items()],
        key=lambda row: row["pc_total_ms"], reverse=True))
    table = ["# Combined hotspots\n",
             "Transfer counts include recorded workload portions, including cases that later failed. "
             "For story replays, repeated logo/title setup before the story marker is excluded. "
             "rankings.json marks whether the whole case completed. Interrupted recordings and recordings with dropped events are excluded.\n"]
    table.append("Stage times include child spans; do not add parent commands and their GPU stages together.\n")
    if parallel_jobs > 1:
        table.append("Cases ran concurrently; PC timings include resource contention.\n")
    table += ["| Stage | Calls | PC ms | Max ms | Copy MiB | Copies | Waits | Readbacks | Cases |",
              "|---|---:|---:|---:|---:|---:|---:|---:|---|"]
    for row in sorted(combined, key=lambda row: row["pc_total_ms"], reverse=True):
        stage = row["stage"].replace("graphics-work stage=graphics-work command=", "")
        table.append(f'| {stage} | {row["count"]} | {row["pc_total_ms"]:.1f} | {row["pc_max_ms"]:.1f} | {row["copy_bytes"]/1048576:.1f} | '
                     f'{row["copy_calls"]} | {row["finishes"]} | {row["readbacks"]} | '
                     f'{", ".join(row["cases"])} |')
    (output / "hotspots.md").write_text("\n".join(table)+"\n", encoding="utf-8")
    print(f"Report: {output / 'batch.md'}", flush=True)


def run(manifest_path, output, profiler, selected, jobs=1, compact=False):
    if jobs < 1:
        raise ValueError("jobs must be positive")
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if selected:
        unknown = set(selected) - {c["name"] for c in manifest["cases"]}
        if unknown:
            raise ValueError(f"unknown cases: {sorted(unknown)}")
        manifest["cases"] = [c for c in manifest["cases"] if c["name"] in selected]
    output.mkdir(parents=True, exist_ok=True)
    names = [case["name"] for case in manifest["cases"]]
    if len(names) != len(set(names)) or any(not re.fullmatch(r"[a-zA-Z0-9_-]+", name) for name in names):
        raise ValueError("case names must be unique plain directory names")
    manifest["parallel_jobs"] = jobs
    save(output / "cases.json", manifest)
    analysis_lock = threading.Lock()
    def run_case(case):
        directory = output / case["name"]
        directory.mkdir(exist_ok=False)
        savedata = directory / "savedata"
        if manifest.get("savedata"):
            shutil.copytree(manifest["savedata"], savedata)
        script = directory / "replay.tjs"
        script.write_text(case["script"], encoding="utf-8")
        actions = directory / "actions.json"
        save(actions, case.get("actions", []))
        cmd = [str(profiler.resolve()), "run", "--game", manifest["game"],
               "--startup", manifest.get("startup", "startup.tjs"), "--out", str(directory),
               "--seconds", str(case["seconds"]), "--script", str(script), "--actions", str(actions)]
        if manifest.get("gles_dir"):
            cmd += ["--gles-dir", manifest["gles_dir"]]
        if manifest.get("at9_clock"):
            cmd.append("--at9-clock")
        if compact:
            cmd.append("--no-report")
        if (fps := case.get("fps", manifest.get("fps"))) is not None:
            cmd += ["--fps", str(fps)]
        if (canvas := case.get("canvas_size", manifest.get("canvas_size"))) is not None:
            cmd += ["--canvas-size", canvas]
        if not case.get("effect_sharpen", manifest.get("effect_sharpen", True)):
            cmd += ["--no-effect-sharpen"]
        if case.get("compact_scene", manifest.get("compact_scene", False)):
            cmd += ["--compact-scene"]
        if (interval := case.get("effect_interval_ms", manifest.get("effect_interval_ms"))) is not None:
            cmd += ["--effect-interval-ms", str(interval)]
        print(f'Running {case["name"]} ({case["seconds"]} s)', flush=True)
        with (directory / "capture.log").open("w", encoding="utf-8") as log:
            try:
                result = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, timeout=case["seconds"] + 60)
                save(directory / "process.json", {"exit_code": result.returncode, "timed_out": False})
                print(f'Finished {case["name"]}: {result.returncode}', flush=True)
            except subprocess.TimeoutExpired:
                save(directory / "process.json", {"timed_out": True})
                print(f'Timed out: {case["name"]}', flush=True)
        if compact:
            # Captures run concurrently; analyze one timeline at a time to
            # avoid retaining several chapter-sized event lists in RAM.
            with analysis_lock:
                analyze(directory, case, compact=True)
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        list(pool.map(run_case, manifest["cases"]))
    report(output, manifest)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    p = commands.add_parser("scan")
    p.add_argument("scripts", type=Path)
    p.add_argument("output", type=Path)
    p = commands.add_parser("run")
    p.add_argument("manifest", type=Path)
    p.add_argument("output", type=Path)
    p.add_argument("--profiler", type=Path, default=Path("target/release/krkr-profiler.exe"))
    p.add_argument("--case", action="append", default=[], help="run only this case; repeat to select several")
    p.add_argument("--jobs", type=int, default=1, help="number of concurrent isolated game processes")
    p.add_argument("--compact", action="store_true", help="keep case summaries and markers, discard raw timelines after analysis")
    p = commands.add_parser("report")
    p.add_argument("manifest", type=Path)
    p.add_argument("output", type=Path)
    p.add_argument("--completed-only", action="store_true", help="rank finished compact cases while the batch continues")
    p = commands.add_parser("merge")
    p.add_argument("output", type=Path)
    p.add_argument("inputs", type=Path, nargs="+")
    args = parser.parse_args()
    if args.command == "scan":
        scan(args.scripts, args.output)
    elif args.command == "run":
        run(args.manifest, args.output, args.profiler, args.case, args.jobs, args.compact)
    elif args.command == "merge":
        merge(args.output, args.inputs)
    else:
        report(args.output, json.loads(args.manifest.read_text(encoding="utf-8")), args.completed_only)

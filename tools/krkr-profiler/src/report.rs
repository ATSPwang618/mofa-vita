use krkr_protocol::profile::Event;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::Path,
};

#[derive(Default, Serialize, Deserialize)]
pub struct Summary {
    pub from_ms: f64,
    pub to_ms: Option<f64>,
    pub hotspots: Vec<Hotspot>,
    pub counters: BTreeMap<String, Counter>,
    pub longest: Vec<Slow>,
    pub markers: Vec<Mark>,
}
#[derive(Serialize, Deserialize)]
pub struct Hotspot {
    pub name: String,
    pub thread: String,
    pub count: usize,
    pub total_ms: f64,
    pub self_ms: f64,
    pub max_ms: f64,
    pub p95_ms: f64,
}
#[derive(Default, Serialize, Deserialize)]
pub struct Counter {
    pub peak: u64,
    #[serde(default)]
    pub min: u64,
    pub last: u64,
    pub peak_at_ms: f64,
    #[serde(default)]
    pub min_at_ms: f64,
}
#[derive(Serialize, Deserialize)]
pub struct Slow {
    pub name: String,
    pub thread: String,
    pub at_ms: f64,
    pub duration_ms: f64,
    pub detail: String,
}
#[derive(Serialize, Deserialize)]
pub struct Mark {
    pub name: String,
    pub at_ms: f64,
    pub detail: String,
}
struct Sample {
    thread: u64,
    start: u64,
    end: u64,
    own: u64,
    name: String,
    detail: String,
}

fn group(name: &str, detail: &str) -> String {
    let qualifier = match name {
        "graphics-work" => "command=",
        "io-work" | "io-queue" => "kind=",
        "image-wait" | "image-resolve-phase" => "phase=",
        _ => "",
    };
    if !qualifier.is_empty()
        && let Some(part) = detail.split_whitespace().find(|s| s.starts_with(qualifier))
    {
        return format!("{name} {part}");
    }
    if name == "gpu.adjust" {
        return format!(
            "{name} {}",
            detail.split(" logical=").next().unwrap_or(detail)
        );
    }
    name.into()
}
fn latency(name: &str) -> bool {
    name.contains("wait")
        || name.starts_with("io-queue")
        || name.starts_with("capture.")
        || name == "host.idle"
}

fn summarize(events: &[Event], from_ms: f64, to_ms: Option<f64>) -> Summary {
    let lo = (from_ms * 1_000_000.) as u64;
    let hi = to_ms.map(|v| (v * 1_000_000.) as u64).unwrap_or(u64::MAX);
    let mut result = Summary {
        from_ms,
        to_ms,
        ..Default::default()
    };
    let mut threads = BTreeMap::new();
    let mut samples = Vec::new();
    let mut counters = Vec::new();
    for event in events {
        match event {
            Event::Thread { thread, name } => {
                threads.insert(*thread, name.clone());
            }
            Event::Counter {
                at_ns, name, value, ..
            } if *at_ns <= hi => counters.push((*at_ns, name, *value)),
            Event::Marker {
                at_ns,
                name,
                detail,
                ..
            } if *at_ns >= lo && *at_ns <= hi => {
                result.markers.push(Mark {
                    name: name.clone(),
                    at_ms: *at_ns as f64 / 1e6,
                    detail: detail.clone(),
                });
            }
            Event::Span {
                thread,
                start_ns,
                duration_ns,
                name,
                detail,
            } => {
                let start = (*start_ns).max(lo);
                let end = start_ns.saturating_add(*duration_ns).min(hi);
                if end > start {
                    samples.push(Sample {
                        thread: *thread,
                        start,
                        end,
                        own: end - start,
                        name: group(name, detail),
                        detail: detail.clone(),
                    });
                }
            }
            _ => {}
        }
    }
    counters.sort_by_key(|(at, _, _)| *at);
    for (at, name, value) in counters {
        let sample = || Counter {
            peak: value,
            min: value,
            last: value,
            peak_at_ms: at.max(lo) as f64 / 1e6,
            min_at_ms: at.max(lo) as f64 / 1e6,
        };
        let entry = result.counters.entry(name.clone()).or_insert_with(sample);
        if at < lo {
            *entry = sample();
            continue;
        }
        if value > entry.peak {
            entry.peak = value;
            entry.peak_at_ms = at as f64 / 1e6;
        }
        if value < entry.min {
            entry.min = value;
            entry.min_at_ms = at as f64 / 1e6;
        }
        entry.last = value;
    }
    // Only subtract contained spans on the same thread. Queue/wait latencies
    // can cross calls and threads; they are reported separately, never as CPU time.
    samples.sort_by_key(|s| (s.thread, s.start, std::cmp::Reverse(s.end)));
    let mut stack: Vec<usize> = Vec::new();
    let mut covered = vec![0u64; samples.len()];
    for i in 0..samples.len() {
        if latency(&samples[i].name) {
            continue;
        }
        while let Some(&p) = stack.last() {
            if samples[p].thread == samples[i].thread
                && samples[p].end >= samples[i].end
                && samples[p].start <= samples[i].start
            {
                break;
            }
            stack.pop();
        }
        if let Some(&p) = stack.last() {
            let child_start = samples[i].start.max(covered[p]);
            samples[p].own = samples[p]
                .own
                .saturating_sub(samples[i].end.saturating_sub(child_start));
            covered[p] = covered[p].max(samples[i].end);
        }
        stack.push(i);
    }
    let thread_name = |thread: u64| {
        threads
            .get(&thread)
            .cloned()
            .unwrap_or_else(|| thread.to_string())
    };
    let mut groups: BTreeMap<(u64, String), Vec<&Sample>> = BTreeMap::new();
    for sample in &samples {
        groups
            .entry((sample.thread, sample.name.clone()))
            .or_default()
            .push(sample);
    }
    for ((thread, name), group) in groups {
        let mut durations: Vec<u64> = group.iter().map(|s| s.end - s.start).collect();
        durations.sort_unstable();
        result.hotspots.push(Hotspot {
            name,
            thread: thread_name(thread),
            count: group.len(),
            total_ms: durations.iter().sum::<u64>() as f64 / 1e6,
            self_ms: group.iter().map(|s| s.own).sum::<u64>() as f64 / 1e6,
            max_ms: *durations.last().unwrap() as f64 / 1e6,
            p95_ms: durations[(durations.len() * 95).div_ceil(100).saturating_sub(1)] as f64 / 1e6,
        });
    }
    result
        .hotspots
        .sort_by(|a, b| b.self_ms.total_cmp(&a.self_ms));
    samples.sort_by_key(|s| std::cmp::Reverse(s.end - s.start));
    result.longest = samples
        .iter()
        .filter(|s| !latency(&s.name))
        .take(30)
        .map(|s| Slow {
            name: s.name.clone(),
            thread: thread_name(s.thread),
            at_ms: s.start as f64 / 1e6,
            duration_ms: (s.end - s.start) as f64 / 1e6,
            detail: s.detail.clone(),
        })
        .collect();
    result.markers.sort_by(|a, b| a.at_ms.total_cmp(&b.at_ms));
    result
}

pub fn generate(directory: &Path, from_ms: f64, to_ms: Option<f64>) -> Result<Summary, String> {
    if !from_ms.is_finite() || from_ms < 0. || to_ms.is_some_and(|v| !v.is_finite() || v < from_ms)
    {
        return Err("invalid report time range".into());
    }
    let input =
        BufReader::new(File::open(directory.join("events.jsonl")).map_err(|e| e.to_string())?);
    let events: Vec<Event> = input
        .lines()
        .enumerate()
        .map(|(i, line)| {
            serde_json::from_str(&line.map_err(|e| e.to_string())?)
                .map_err(|e| format!("events.jsonl:{}: {e}", i + 1))
        })
        .collect::<Result<_, _>>()?;
    let summary = summarize(&events, from_ms, to_ms);
    let suffix = if from_ms == 0. && to_ms.is_none() {
        ""
    } else {
        "-range"
    };
    std::fs::write(
        directory.join(format!("summary{suffix}.json")),
        serde_json::to_vec_pretty(&summary).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    let mut text = String::from(
        "Wall-clock durations; nested stages overlap. self_ms excludes contained work on the same thread.\nQueue/wait rows are latency, not CPU usage. Desktop timings are not Vita timings.\n\n",
    );
    use std::fmt::Write as _;
    writeln!(
        text,
        "{:>10} {:>10} {:>8} {:>10} {:>10}  thread / stage",
        "self_ms", "total_ms", "count", "max_ms", "p95_ms"
    )
    .unwrap();
    for row in summary
        .hotspots
        .iter()
        .filter(|r| !latency(&r.name))
        .take(40)
    {
        writeln!(
            text,
            "{:10.3} {:10.3} {:8} {:10.3} {:10.3}  {} / {}",
            row.self_ms, row.total_ms, row.count, row.max_ms, row.p95_ms, row.thread, row.name
        )
        .unwrap();
    }
    text.push_str("\nWaits, queues and capture overhead (total_ms / count / max_ms):\n");
    for row in summary.hotspots.iter().filter(|r| latency(&r.name)) {
        writeln!(
            text,
            "{:.3} / {} / {:.3}  {} / {}",
            row.total_ms, row.count, row.max_ms, row.thread, row.name
        )
        .unwrap();
    }
    text.push_str("\nCounters (byte pools are engine accounting, not physical GPU memory):\n");
    for (name, c) in &summary.counters {
        writeln!(
            text,
            "{name}: peak={} at_ms={:.3} min={} at_ms={:.3} last={}",
            c.peak, c.peak_at_ms, c.min, c.min_at_ms, c.last
        )
        .unwrap();
    }
    text.push_str("\nLongest calls:\n");
    for item in &summary.longest {
        writeln!(
            text,
            "+{:.3}ms {:.3}ms [{}] {} {}",
            item.at_ms, item.duration_ms, item.thread, item.name, item.detail
        )
        .unwrap();
    }
    std::fs::write(directory.join(format!("report{suffix}.txt")), &text)
        .map_err(|e| e.to_string())?;
    if suffix.is_empty() {
        write_trace(directory, &events)?;
    }
    println!("{}", text.lines().take(26).collect::<Vec<_>>().join("\n"));
    println!(
        "Report: {}",
        directory.join(format!("report{suffix}.txt")).display()
    );
    Ok(summary)
}
fn write_trace(directory: &Path, events: &[Event]) -> Result<(), String> {
    let mut output =
        BufWriter::new(File::create(directory.join("trace.json")).map_err(|e| e.to_string())?);
    output
        .write_all(b"{\"traceEvents\":[")
        .map_err(|e| e.to_string())?;
    for (index, event) in events.iter().enumerate() {
        if index != 0 {
            output.write_all(b",").map_err(|e| e.to_string())?;
        }
        if let Event::Span {
            thread,
            start_ns,
            duration_ns,
            name,
            detail,
        } = event
            && (name.contains("wait") || name.starts_with("io-queue"))
        {
            // Queue lifetimes can overlap polls instead of nesting inside them.
            for (i, (phase, at)) in [
                ("b", *start_ns),
                ("e", start_ns.saturating_add(*duration_ns)),
            ]
            .into_iter()
            .enumerate()
            {
                if i != 0 {
                    output.write_all(b",").map_err(|e| e.to_string())?;
                }
                let item = serde_json::json!({"ph":phase,"cat":"latency","id":index,"name":name,"pid":1,"tid":thread,"ts":at as f64/1000.,"args":{"detail":detail}});
                serde_json::to_writer(&mut output, &item).map_err(|e| e.to_string())?;
            }
            continue;
        }
        let event = match event {
            Event::Thread { thread, name } => {
                serde_json::json!({"ph":"M","name":"thread_name","pid":1,"tid":thread,"args":{"name":name}})
            }
            Event::Span {
                thread,
                start_ns,
                duration_ns,
                name,
                detail,
            } => {
                serde_json::json!({"ph":"X","name":name,"pid":1,"tid":thread,"ts":*start_ns as f64/1000.,"dur":*duration_ns as f64/1000.,"args":{"detail":detail}})
            }
            Event::Counter {
                thread,
                at_ns,
                name,
                value,
            } => {
                serde_json::json!({"ph":"C","name":name,"pid":1,"tid":thread,"ts":*at_ns as f64/1000.,"args":{"value":value}})
            }
            Event::Marker {
                thread,
                at_ns,
                name,
                detail,
            } => {
                serde_json::json!({"ph":"i","s":"t","name":name,"pid":1,"tid":thread,"ts":*at_ns as f64/1000.,"args":{"detail":detail}})
            }
        };
        serde_json::to_writer(&mut output, &event).map_err(|e| e.to_string())?;
    }
    output
        .write_all(b"],\"displayTimeUnit\":\"ms\"}")
        .and_then(|_| output.flush())
        .map_err(|e| e.to_string())
}
pub fn compare(before: &Path, after: &Path) -> Result<(), String> {
    let read = |path: &Path| -> Result<Summary, String> {
        let run: serde_json::Value =
            serde_json::from_reader(File::open(path.join("run.json")).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        if !run["error"].is_null() || run["dropped_events"].as_u64() != Some(0) {
            return Err(format!(
                "{}: failed or incomplete capture; inspect run.json first",
                path.display()
            ));
        }
        serde_json::from_reader(File::open(path.join("summary.json")).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    };
    let before = read(before)?;
    let after = read(after)?;
    println!(
        "Same scene and input sequence required. Changes below are observations, not normalized speedups.\n"
    );
    for row in &after.hotspots {
        if let Some(old) = before
            .hotspots
            .iter()
            .find(|v| v.name == row.name && v.thread == row.thread)
        {
            println!(
                "{} / {}: self {:.3} -> {:.3} ms; calls {} -> {}; p95 {:.3} -> {:.3} ms",
                row.thread,
                row.name,
                old.self_ms,
                row.self_ms,
                old.count,
                row.count,
                old.p95_ms,
                row.p95_ms
            );
        }
    }
    for (name, value) in &after.counters {
        if let Some(old) = before.counters.get(name) {
            println!("{name}: peak {} -> {}", old.peak, value.peak);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counter_ranges_include_prior_state_and_order_concurrent_samples() {
        let sample = |at_ns, value| Event::Counter {
            thread: 1,
            at_ns,
            name: "memory".into(),
            value,
        };
        let result = summarize(
            &[sample(90, 20), sample(10, 100), sample(70, 200)],
            0.00005,
            Some(0.0001),
        );
        let memory = &result.counters["memory"];
        assert_eq!((memory.min, memory.peak, memory.last), (20, 200, 20));
        assert_eq!(memory.peak_at_ms, 0.00007);
        let result = summarize(&[sample(10, 100)], 0.00005, Some(0.0001));
        assert_eq!(result.counters["memory"].peak, 100);
    }
    #[test]
    fn file_recording_closes_cleanly_and_exports_overlapping_waits() {
        let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/krkr-profiler-tests");
        std::fs::create_dir_all(&output).unwrap();
        let temp = tempfile::tempdir_in(output).unwrap();
        let capture =
            krkr_protocol::profile::FileCapture::start(&temp.path().join("events.jsonl"), 16)
                .unwrap();
        {
            let _work = krkr_protocol::profile::span("test.work");
        }
        assert_eq!(capture.finish().unwrap(), 0);
        assert!(!krkr_protocol::profile::active());
        let lines = std::fs::read_to_string(temp.path().join("events.jsonl")).unwrap();
        assert!(
            lines
                .lines()
                .all(|line| serde_json::from_str::<Event>(line).is_ok())
        );
        assert!(lines.contains("test.work"));
        let capture =
            krkr_protocol::profile::FileCapture::start(&temp.path().join("drop.jsonl"), 16)
                .unwrap();
        krkr_protocol::profile::marker("drop", || "test".into());
        drop(capture);
        assert!(!krkr_protocol::profile::active());
        assert!(
            std::fs::read_to_string(temp.path().join("drop.jsonl"))
                .unwrap()
                .contains("test")
        );
        write_trace(
            temp.path(),
            &[span(1, 0, 80, "io-queue"), span(1, 50, 100, "vm-poll")],
        )
        .unwrap();
        let trace: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.path().join("trace.json")).unwrap())
                .unwrap();
        let events = trace["traceEvents"].as_array().unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["ph"], "b");
        assert_eq!(events[1]["ph"], "e");
        assert_eq!(events[0]["id"], events[1]["id"]);
        assert_eq!(events[2]["ph"], "X");
    }
    fn span(thread: u64, start: u64, end: u64, name: &str) -> Event {
        Event::Span {
            thread,
            start_ns: start,
            duration_ns: end - start,
            name: name.into(),
            detail: String::new(),
        }
    }
    #[test]
    fn nested_calls_subtract_children_once_and_keep_other_threads_separate() {
        let events = vec![
            span(1, 0, 100, "parent"),
            span(1, 10, 60, "child"),
            span(1, 20, 40, "grandchild"),
            span(2, 0, 100, "other"),
        ];
        let result = summarize(&events, 0., None);
        let own = |name| {
            result
                .hotspots
                .iter()
                .find(|s| s.name == name)
                .unwrap()
                .self_ms
                * 1e6
        };
        assert_eq!(own("parent"), 50.);
        assert_eq!(own("child"), 30.);
        assert_eq!(own("other"), 100.);
    }
    #[test]
    fn range_clips_intervals_and_latency_does_not_consume_parent_work() {
        let events = vec![
            span(1, 0, 100, "parent"),
            span(1, 20, 80, "image-wait"),
            Event::Counter {
                thread: 1,
                at_ns: 40,
                name: "memory".into(),
                value: 123,
            },
        ];
        let result = summarize(&events, 0.00003, Some(0.00009));
        assert_eq!(
            result
                .hotspots
                .iter()
                .find(|s| s.name == "parent")
                .unwrap()
                .self_ms
                * 1e6,
            60.
        );
        assert_eq!(result.counters["memory"].peak, 123);
    }
}

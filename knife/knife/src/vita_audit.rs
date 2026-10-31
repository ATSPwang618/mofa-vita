//! Console aggregation only. No file I/O on the collection/output path.
use std::fmt::Write;
use knife_perf::{Metric, Sample};
use knife_platform_vita::audit;

#[derive(Default, Clone, Copy)]
pub struct FrameProbe {
    pub native_available: bool,
    pub misses: u64,
    pub reload_us: u64,
    pub upload_bytes: u64,
    pub zero_bytes: u64,
    pub gpu_copy_bytes: u64,
    pub wait_us: u64,
    pub draw_us: u64,
    pub end_scene_us: u64,
    pub end_scene_calls: u64,
    pub scenes: u64,
    pub atlas_wait_us: u64,
    pub atlas_wait_calls: u64,
    pub stage_redraws: u64,
    pub stage_reuses: u64,
    pub ps_copies: u64,
    pub ps_copy_us: u64,
    pub ps_copy_bytes: u64,
    pub group_copies: u64,
    pub group_copy_us: u64,
    pub group_copy_bytes: u64,
    pub target_copies: u64,
    pub gpu_copies: u64,
    pub finish_wait_us: u64,
    pub transfer_wait_us: u64,
    pub batch_wait_us: u64,
    pub display_wait_us: u64,
}
impl FrameProbe {
    pub fn capture() -> Self {
        let ps = knife_perf::get(Metric::CopyPs);
        let group = knife_perf::get(Metric::CopyGroup);
        let atlas = knife_perf::get(Metric::AtlasDemandWait);
        let mut value = Self {
            misses: knife_perf::get(Metric::GxtMiss).calls + knife_perf::get(Metric::ImageMiss).calls,
            reload_us: knife_perf::get(Metric::GxtReload).us + knife_perf::get(Metric::ImageReload).us,
            stage_redraws: knife_perf::get(Metric::StageRedraw).calls,
            stage_reuses: knife_perf::get(Metric::StageReuse).calls,
            atlas_wait_us: atlas.us, atlas_wait_calls: atlas.calls,
            ps_copies: ps.calls, ps_copy_us: ps.us, ps_copy_bytes: ps.bytes,
            group_copies: group.calls, group_copy_us: group.us, group_copy_bytes: group.bytes,
            target_copies: knife_perf::get(Metric::CopyTarget).calls,
            ..Self::default()
        };
        if let Ok(rows) = audit::snapshot() {
            value.native_available = true;
            for (name, row) in audit::NATIVE_NAMES.iter().zip(rows) {
                match *name {
                    "upload_rgba" | "upload_r8" | "upload_gxt" | "texture_update" => value.upload_bytes += row.bytes,
                    "zero_linear_source" | "zero_render_target" | "zero_gxt" => value.zero_bytes += row.bytes,
                    "copy_gpu_quad" | "copy_gpu_transfer" => { value.gpu_copy_bytes += row.bytes; value.gpu_copies += row.calls; },
                    "draw_total" | "draw_basic_total" => value.draw_us += row.us,
                    "end_scene" => { value.end_scene_us = row.us; value.end_scene_calls = row.calls; },
                    "begin_scene" => value.scenes = row.calls,
                    "display_queue_finish" | "display_queue_enqueue" => value.display_wait_us += row.us,
                    "batch_slot_wait" => value.batch_wait_us += row.us,
                    _ if name.starts_with("finish_") => value.finish_wait_us += row.us,
                    _ if name.starts_with("transfer_finish_") => value.transfer_wait_us += row.us,
                    _ => {},
                }
            }
            // EndScene and parent draw/copy scopes are deliberately not added here.
            value.wait_us = value.finish_wait_us + value.transfer_wait_us
                + value.batch_wait_us + value.display_wait_us;
        }
        value
    }
    pub fn delta(self, old: Self) -> Self {
        Self { native_available: self.native_available && old.native_available,
            misses: self.misses.saturating_sub(old.misses),
            reload_us: self.reload_us.saturating_sub(old.reload_us),
            upload_bytes: self.upload_bytes.saturating_sub(old.upload_bytes),
            zero_bytes: self.zero_bytes.saturating_sub(old.zero_bytes),
            gpu_copy_bytes: self.gpu_copy_bytes.saturating_sub(old.gpu_copy_bytes),
            wait_us: self.wait_us.saturating_sub(old.wait_us),
            draw_us: self.draw_us.saturating_sub(old.draw_us),
            end_scene_us: self.end_scene_us.saturating_sub(old.end_scene_us),
            end_scene_calls: self.end_scene_calls.saturating_sub(old.end_scene_calls),
            scenes: self.scenes.saturating_sub(old.scenes),
            atlas_wait_us: self.atlas_wait_us.saturating_sub(old.atlas_wait_us),
            atlas_wait_calls: self.atlas_wait_calls.saturating_sub(old.atlas_wait_calls),
            stage_redraws: self.stage_redraws.saturating_sub(old.stage_redraws),
            stage_reuses: self.stage_reuses.saturating_sub(old.stage_reuses),
            ps_copies: self.ps_copies.saturating_sub(old.ps_copies),
            ps_copy_us: self.ps_copy_us.saturating_sub(old.ps_copy_us),
            ps_copy_bytes: self.ps_copy_bytes.saturating_sub(old.ps_copy_bytes),
            group_copies: self.group_copies.saturating_sub(old.group_copies),
            group_copy_us: self.group_copy_us.saturating_sub(old.group_copy_us),
            group_copy_bytes: self.group_copy_bytes.saturating_sub(old.group_copy_bytes),
            target_copies: self.target_copies.saturating_sub(old.target_copies),
            gpu_copies: self.gpu_copies.saturating_sub(old.gpu_copies),
            finish_wait_us: self.finish_wait_us.saturating_sub(old.finish_wait_us),
            transfer_wait_us: self.transfer_wait_us.saturating_sub(old.transfer_wait_us),
            batch_wait_us: self.batch_wait_us.saturating_sub(old.batch_wait_us),
            display_wait_us: self.display_wait_us.saturating_sub(old.display_wait_us),
        }
    }
    fn add(&mut self, value: Self) {
        self.misses = self.misses.saturating_add(value.misses);
        self.reload_us = self.reload_us.saturating_add(value.reload_us);
        self.upload_bytes = self.upload_bytes.saturating_add(value.upload_bytes);
        self.zero_bytes = self.zero_bytes.saturating_add(value.zero_bytes);
        self.gpu_copy_bytes = self.gpu_copy_bytes.saturating_add(value.gpu_copy_bytes);
        self.wait_us = self.wait_us.saturating_add(value.wait_us);
        self.draw_us = self.draw_us.saturating_add(value.draw_us);
        self.end_scene_us = self.end_scene_us.saturating_add(value.end_scene_us);
        self.end_scene_calls = self.end_scene_calls.saturating_add(value.end_scene_calls);
        self.scenes = self.scenes.saturating_add(value.scenes);
        self.atlas_wait_us = self.atlas_wait_us.saturating_add(value.atlas_wait_us);
        self.atlas_wait_calls = self.atlas_wait_calls.saturating_add(value.atlas_wait_calls);
        self.stage_redraws = self.stage_redraws.saturating_add(value.stage_redraws);
        self.stage_reuses = self.stage_reuses.saturating_add(value.stage_reuses);
        self.ps_copies = self.ps_copies.saturating_add(value.ps_copies);
        self.ps_copy_us = self.ps_copy_us.saturating_add(value.ps_copy_us);
        self.ps_copy_bytes = self.ps_copy_bytes.saturating_add(value.ps_copy_bytes);
        self.group_copies = self.group_copies.saturating_add(value.group_copies);
        self.group_copy_us = self.group_copy_us.saturating_add(value.group_copy_us);
        self.group_copy_bytes = self.group_copy_bytes.saturating_add(value.group_copy_bytes);
        self.target_copies = self.target_copies.saturating_add(value.target_copies);
        self.gpu_copies = self.gpu_copies.saturating_add(value.gpu_copies);
        self.finish_wait_us = self.finish_wait_us.saturating_add(value.finish_wait_us);
        self.transfer_wait_us = self.transfer_wait_us.saturating_add(value.transfer_wait_us);
        self.batch_wait_us = self.batch_wait_us.saturating_add(value.batch_wait_us);
        self.display_wait_us = self.display_wait_us.saturating_add(value.display_wait_us);
    }
    pub fn frame_class(self) -> &'static str {
        if self.stage_redraws > 0 { "redraw" }
        else if self.stage_reuses > 0 { "reuse" } else { "other" }
    }
}

fn probe_json(out: &mut String, p: FrameProbe) {
    let _ = write!(out, ",\"deferred_misses\":{}", p.misses);
    let _ = write!(out, ",\"reload_us\":{}", p.reload_us);
    let _ = write!(out, ",\"upload_bytes\":{}", p.upload_bytes);
    let _ = write!(out, ",\"zero_bytes\":{}", p.zero_bytes);
    let _ = write!(out, ",\"gpu_copy_bytes\":{}", p.gpu_copy_bytes);
    let _ = write!(out, ",\"wait_us\":{}", p.wait_us);
    let _ = write!(out, ",\"draw_us\":{}", p.draw_us);
    let _ = write!(out, ",\"end_scene_us\":{}", p.end_scene_us);
    let _ = write!(out, ",\"end_scene_calls\":{}", p.end_scene_calls);
    let _ = write!(out, ",\"scenes\":{}", p.scenes);
    let _ = write!(out, ",\"atlas_wait_us\":{}", p.atlas_wait_us);
    let _ = write!(out, ",\"atlas_wait_calls\":{}", p.atlas_wait_calls);
    let _ = write!(out, ",\"stage_redraws\":{}", p.stage_redraws);
    let _ = write!(out, ",\"stage_reuses\":{}", p.stage_reuses);
    let _ = write!(out, ",\"ps_copies\":{}", p.ps_copies);
    let _ = write!(out, ",\"ps_copy_us\":{}", p.ps_copy_us);
    let _ = write!(out, ",\"ps_copy_bytes\":{}", p.ps_copy_bytes);
    let _ = write!(out, ",\"group_copies\":{}", p.group_copies);
    let _ = write!(out, ",\"group_copy_us\":{}", p.group_copy_us);
    let _ = write!(out, ",\"group_copy_bytes\":{}", p.group_copy_bytes);
    let _ = write!(out, ",\"target_copies\":{}", p.target_copies);
    let _ = write!(out, ",\"gpu_copies\":{}", p.gpu_copies);
    let _ = write!(out, ",\"finish_wait_us\":{}", p.finish_wait_us);
    let _ = write!(out, ",\"transfer_wait_us\":{}", p.transfer_wait_us);
    let _ = write!(out, ",\"batch_wait_us\":{}", p.batch_wait_us);
    let _ = write!(out, ",\"display_wait_us\":{}", p.display_wait_us);
}

#[derive(Clone, Copy)]
pub struct SlowFrame {
    pub frame: u64, pub scene: usize, pub pc: usize, pub work: u64, pub update: u64, pub render: u64,
    pub probe: FrameProbe,
}

#[derive(Default)]
struct ClassTotal {
    frames: u64, native_frames: u64,
    work: u64, update: u64, render: u64, wall: u64, max_work: u64,
    probe: FrameProbe,
}

/// Frames are settled at their next begin_frame boundary so a slow redraw's
/// wall interval is never assigned to the reuse frame which follows it.
#[derive(Default)]
pub struct FrameClasses {
    first_frame: u64, last_frame: u64, totals: [ClassTotal; 3],
}
impl FrameClasses {
    pub fn record(&mut self, value: SlowFrame, wall: u64) {
        if self.first_frame == 0 { self.first_frame = value.frame; }
        self.last_frame = value.frame;
        let index = match value.probe.frame_class() { "redraw" => 0, "reuse" => 1, _ => 2 };
        let total = &mut self.totals[index];
        total.frames += 1;
        total.native_frames += u64::from(value.probe.native_available);
        total.work = total.work.saturating_add(value.work);
        total.update = total.update.saturating_add(value.update);
        total.render = total.render.saturating_add(value.render);
        total.wall = total.wall.saturating_add(wall);
        total.max_work = total.max_work.max(value.work);
        total.probe.add(value.probe);
        total.probe.native_available = total.native_frames == total.frames;
    }
    pub fn emit(&mut self, frame: u64, window_ms: u128) -> std::io::Result<()> {
        let window = std::mem::take(self);
        let frames: u64 = window.totals.iter().map(|value| value.frames).sum();
        let mut line = format!("{{\"kind\":\"frame_classes\",\"schema\":1,\"frame\":{frame},\"first_frame\":{},\"last_frame\":{},\"frames\":{frames},\"window_ms\":{window_ms},\"classes\":{{", window.first_frame, window.last_frame);
        for (i, (name, total)) in ["redraw", "reuse", "other"].iter().zip(window.totals).enumerate() {
            if i > 0 { line.push(','); }
            let _ = write!(line, "\"{name}\":{{\"frames\":{},\"native_frames\":{},\"work_us\":{},\"update_us\":{},\"render_us\":{},\"wall_us\":{},\"max_work_us\":{},\"native_available\":{}",
                total.frames, total.native_frames, total.work, total.update, total.render,
                total.wall, total.max_work, total.probe.native_available);
            probe_json(&mut line, total.probe);
            line.push('}');
        }
        line.push_str("}}\n");
        audit::console(&line)
    }
}

fn bank(frame: u64, domain: &str, names: &[&str], samples: &[Sample], previous: &[Sample]) -> std::io::Result<usize> {
    let mut count = 0;
    for (part, (names, samples)) in names.chunks(8).zip(samples.chunks(8)).enumerate() {
        if samples.iter().all(|s| *s == Sample::default()) { continue; }
        // Values remain cumulative. An omitted unchanged bank has exactly zero
        // delta; the offline reader retains its previous complete observation.
        if previous.get(part * 8..part * 8 + samples.len()) == Some(samples) { continue; }
        let mut line = format!("{{\"kind\":\"audit_bank\",\"frame\":{frame},\"domain\":\"{domain}\",\"part\":{part},\"counters\":");
        knife_perf::bank_json(&mut line, names, samples);
        line.push_str("}\n");
        audit::console(&line)?;
        count += 1;
    }
    Ok(count)
}

/// Cumulative banks plus per-window load hotspots. End marker lets the reader
/// detect dropped console lines instead of interpreting missing data as zero.
pub fn emit(frame: u64, phase: &str, worst: Option<SlowFrame>) -> std::io::Result<()> {
    static PREVIOUS: std::sync::Mutex<(Vec<Sample>, Vec<Sample>, u64, u64, u64)> =
        std::sync::Mutex::new((Vec::new(), Vec::new(), 0, 0, 0));
    let rust = knife_perf::snapshot();
    let native = audit::snapshot().ok();
    let transport = audit::console_stats();
    let mut previous = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner());
    previous.4 += 1;
    // Periodic full observations recover from dropped console/plugin traffic.
    // A queue rejection/write failure also forces the next observation full.
    let full = phase == "startup" || previous.4 % 6 == 0
        || transport.dropped != previous.2 || transport.failures != previous.3;
    let mut banks = bank(frame, "rust", knife_perf::NAMES, &rust, if full { &[] } else { &previous.0 })?;
    let mut native_rows = Vec::new();
    if let Some(native) = &native {
        native_rows = native.iter().map(|s| Sample { calls:s.calls, us:s.us, bytes:s.bytes, aux:s.aux }).collect();
        banks += bank(frame, "native", audit::NATIVE_NAMES, &native_rows, if full { &[] } else { &previous.1 })?;
    }
    previous.0 = rust; previous.1 = native_rows;
    previous.2 = transport.dropped; previous.3 = transport.failures;
    drop(previous);
    let (hotspots, dropped, omitted) = knife_perf::take_hotspots(8);
    for row in &hotspots {
        let mut line = format!("{{\"kind\":\"audit_hotspot\",\"frame\":{frame},\"metric\":");
        knife_perf::json_string(&mut line, knife_perf::NAMES[row.metric]);
        line.push_str(",\"label\":"); knife_perf::json_string(&mut line, &row.label);
        line.push_str(",\"value\":"); knife_perf::sample_json(&mut line, row.sample);
        line.push_str("}\n"); audit::console(&line)?;
    }
    let mut line = format!("{{\"kind\":\"audit\",\"frame\":{frame},\"phase\":\"{phase}\",\"banks\":{banks},\"hotspots\":{},\"hotspots_omitted\":{omitted},\"hotspot_dropped_updates\":{dropped},\"native_available\":{},\"gauges\":{{", hotspots.len(), native.is_some());
    for (i, (name, (value, peak))) in knife_perf::GAUGE_NAMES.iter().zip(knife_perf::gauges()).enumerate() {
        if i > 0 { line.push(','); }
        let _ = write!(line, "\"{name}\":{{\"value\":{value},\"peak\":{peak}}}");
    }
    line.push('}');
    if let Some(w) = worst {
        let p = w.probe;
        let _ = write!(line, ",\"worst\":{{\"frame\":{},\"scene\":{},\"pc\":{},\"work_us\":{},\"update_us\":{},\"render_us\":{},\"frame_class\":\"{}\",\"native_available\":{}",
            w.frame, w.scene, w.pc, w.work, w.update, w.render, p.frame_class(), p.native_available);
        probe_json(&mut line, p);
        line.push('}');
    }
    line.push_str("}\n"); audit::console(&line)
}

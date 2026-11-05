//! Opt-in, allocation-free counters. Only bounded load hotspots allocate labels.
//! Times are CPU wall time, may nest/overlap across workers, and are never GPU busy.
use std::{fmt::Write, sync::{Mutex, atomic::{AtomicBool, AtomicU64, Ordering}}, time::Instant};

pub mod memory_budget;

static ENABLED: AtomicBool = AtomicBool::new(false);
pub fn set_enabled(value: bool) { ENABLED.store(value, Ordering::Relaxed); }
#[inline]
pub fn enabled() -> bool { ENABLED.load(Ordering::Relaxed) }

macro_rules! metrics {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[repr(usize)]
        #[derive(Clone, Copy, Debug)]
        pub enum Metric { $($variant),+, Count }
        pub const NAMES: &[&str] = &[$($name),+];
    };
}
metrics! {
    ArchiveBufferZero => "archive_buffer_zero", ArchiveLock => "archive_lock_wait",
    NxFull => "nx_full", NxRange => "nx_range", RangeSelect => "range_select_copy",
    StageRequest => "stage_request", StageSkipReady => "stage_skip_ready",
    StageSkipFlight => "stage_skip_inflight", StageStart => "stage_thread_start",
    StageComplete => "stage_complete", StageFailed => "stage_read_failed",
    StageSpawnFailed => "stage_spawn_failed", StageHit => "stage_hit",
    StageMiss => "stage_miss", StageMissFlight => "stage_miss_inflight",
    StageEvict => "stage_evict", StageVecArc => "stage_vec_to_arc",
    StageArcVec => "stage_arc_to_vec", UiRetainGxt => "ui_gxt_unused_retain",
    UiRetainOther => "ui_other_retain", GxtRegister => "gxt_register",
    GxtReload => "gxt_reload", ImageReload => "image_reload", ImageFirstUpload => "image_first_upload",
    DecodedCVec => "decoded_c_to_vec", LanguageCrop => "language_crop",
    CpuGrade => "cpu_grade", DeferredPrepare => "deferred_prepare",
    GxtHit => "gxt_resident_hit", GxtMiss => "gxt_resident_miss",
    ImageHit => "image_resident_hit", ImageMiss => "image_resident_miss",
    DeferredEvict => "deferred_evict", DeferredDecode => "deferred_decode",
    DeferredReserve => "deferred_reserve", CommandMerge => "command_merge_copy",
    QuadBuild => "quad_build", DrawRecord => "draw_record", CommandAppend => "command_append",
    DirectStage => "direct_stage", StageReuse => "stage_reuse", StageRedraw => "stage_redraw",
    GroupStageDeferred => "group_stage_deferred", GroupStageAliasFallback => "group_stage_alias_fallback", CopyPs => "copy_ps_snapshot",
    CopyGroup => "copy_group_scratch", CopyTarget => "copy_target",
    MergeShader => "merge_block_shader", MergeBlend => "merge_block_blend",
    MergeTextures => "merge_block_textures", MergeParams => "merge_block_params",
    MergeHaze => "merge_block_haze", MergeCapacity => "merge_block_capacity",
    MergeBoundary => "merge_block_boundary",
    TextCacheHit => "text_cache_hit", TextCacheMiss => "text_cache_miss",
    TextCacheBuild => "text_cache_build", TextCacheFallback => "text_cache_fallback",
    TextCacheEvict => "text_cache_evict", TextCacheSavedDraws => "text_cache_saved_draws",
    TextCacheInvalidate => "text_cache_invalidate",
    GroupCacheHit => "group_cache_hit", GroupCacheMiss => "group_cache_miss",
    GroupCacheBypass => "group_cache_bypass", GroupCacheEvict => "group_cache_evict",
    GroupCacheDrawSaved => "group_cache_draw_saved",
    GxtCpuDecode => "gxt_cpu_decode", SourceCacheHit => "source_cache_hit",
    SourceCacheMiss => "source_cache_miss", SourceCacheSeed => "source_cache_seed",
    SourceCachePrefetch => "source_cache_prefetch", SourceCacheDropped => "source_cache_dropped",
    CachePressureTrim => "cache_pressure_trim",
    GlyphPrewarm => "glyph_prewarm", GlyphRaster => "glyph_raster",
    GlyphPageUpload => "glyph_page_upload", GlyphPrewarmFallback => "glyph_prewarm_fallback",
    StagePrefixHit => "stage_prefix_hit", StagePrefixMiss => "stage_prefix_miss",
    StagePrefixBuild => "stage_prefix_build", StagePrefixBypass => "stage_prefix_bypass",
    StagePrefixDrawSaved => "stage_prefix_draw_saved",
    StageCoverage => "stage_coverage",
    SparseCoverageHit => "sparse_coverage_hit",
    SparseCoverageTiles => "sparse_coverage_tiles",
    SparseCoverageFallback => "sparse_coverage_fallback",
    AtlasLoad => "atlas_load", AtlasDemandWait => "atlas_demand_wait",
    GradeLutBuild => "grade_lut_build", GradeLutHit => "grade_lut_hit", GradeLutMiss => "grade_lut_miss",
    GradePixel => "grade_pixel", GradeResultHit => "grade_result_hit", GradeResultMiss => "grade_result_miss",
    GradeResultEvict => "grade_result_evict",
    BakeDeferred => "bake_deferred", BakeCoalesced => "bake_coalesced",
    BakeFlush => "bake_flush", BakeRebuild => "bake_rebuild", BakeFailed => "bake_failed",
    ArchiveHeaderHit => "archive_header_hit", ArchiveHeaderMiss => "archive_header_miss",
    ArchiveTinyHit => "archive_tiny_hit", ArchiveTinyStore => "archive_tiny_store",
    ArchiveTinyEvict => "archive_tiny_evict",
    StreamResolve => "stream_resolve", StreamIndexHit => "stream_index_hit", StreamStat => "stream_stat",
    Message => "message_total", MessageBackup => "message_backup", MessageResolve => "message_resolve",
    MessageLayout => "message_layout", MessageHistory => "message_history", VoiceSchedule => "voice_schedule",
    AudioStreamOpen => "audio_stream_open",
    PsOp1 => "ps_op_1",
    PsOp2 => "ps_op_2",
    PsOp3 => "ps_op_3",
    PsOp4 => "ps_op_4",
    PsOp5 => "ps_op_5",
    PsOp6 => "ps_op_6",
    PsOp7 => "ps_op_7",
    PsOp8 => "ps_op_8",
    PsOp9 => "ps_op_9",
    PsOp10 => "ps_op_10",
    PsOp11 => "ps_op_11",
    PsOp12 => "ps_op_12",
    PsOp13 => "ps_op_13",
    PsOp14 => "ps_op_14",
    PsOp15 => "ps_op_15",
    PsOp16 => "ps_op_16",
    PsOp17 => "ps_op_17",
    PsOp18 => "ps_op_18",
    PsOp19 => "ps_op_19",
    PsOp20 => "ps_op_20",
    PsOp21 => "ps_op_21",
    PsOp22 => "ps_op_22",
    PsOp23 => "ps_op_23",
    PsOp24 => "ps_op_24",
    CpuReclaimRequest => "cpu_reclaim_request", CpuReclaimFreed => "cpu_reclaim_freed",
    CpuSourceHotRetained => "cpu_source_hot_retained",
    FontInit => "font_init", FontParse => "font_parse", FontRead => "font_read",
    UiFontInit => "ui_font_init", GlyphPageEvict => "glyph_page_evict",
    InputPoll => "input_poll", StoryInput => "story_input", GameUpdate => "game_update",
    UiUpdate => "ui_update", AudioFlush => "audio_flush", SaveStore => "save_store",
    SaveWrite => "save_write", SaveSync => "save_sync", SaveVerify => "save_verify",
    AudioStreamPrepare => "audio_stream_prepare", AudioStreamDecode => "audio_stream_decode",
    AudioStreamPending => "audio_stream_pending", AudioStreamStarved => "audio_stream_starved",
    AudioStreamReady => "audio_stream_ready", AudioStreamCancelled => "audio_stream_cancelled",
    AudioStreamFailed => "audio_stream_failed", AudioStreamLimit => "audio_stream_limit",
    GpuCacheAdmit => "gpu_cache_admit", GpuCacheEvict => "gpu_cache_evict",
    GpuCacheBypass => "gpu_cache_bypass", GroupBatchPlan => "group_batch_plan",
    GroupBatchBypass => "group_batch_bypass", GroupBatchStagePasses => "group_batch_stage_passes",
    GroupBatchRejectPlan => "group_batch_reject_plan",
    GroupBatchRejectAlias => "group_batch_reject_alias",
    GroupBatchRejectCapability => "group_batch_reject_capability",
    GroupBatchRejectAdmission => "group_batch_reject_admission",
    GroupBatchRejectStructure => "group_batch_reject_structure",
    GroupBatchRejectTooFew => "group_batch_reject_too_few",
    GroupBatchRejectTooMany => "group_batch_reject_too_many",
    UiPassCacheRecord => "ui_pass_cache_record", UiPassCacheHit => "ui_pass_cache_hit",
    UiPassCacheBuild => "ui_pass_cache_build", UiPassCacheBypass => "ui_pass_cache_bypass",
    UiPassCacheInvalidate => "ui_pass_cache_invalidate", UiPassCacheQuadsSaved => "ui_pass_cache_quads_saved",
    UiSourcePending => "ui_source_pending", UiSourceReady => "ui_source_ready",
    UiSourceFailed => "ui_source_failed", UiSourceCancelled => "ui_source_cancelled",
    UiGlowCoverage => "ui_glow_coverage",
    // Coverage aux values are estimated physical pixels, never bus bytes.
    BlendChain => "blend_chain", BlendChainCoverage => "blend_chain_coverage",
    BlendChainFallback => "blend_chain_fallback",
    TargetRingEnable => "target_ring_enable", TargetRingRotate => "target_ring_rotate",
    TargetRingBypass => "target_ring_bypass", TargetRingRelease => "target_ring_release",
    StageInputGeometryChanged => "stage_input_geometry_changed",
    StageInputColorChanged => "stage_input_color_changed",
    StageInputMaterialChanged => "stage_input_material_changed",
    StageInputTextureChanged => "stage_input_texture_changed",
    StageRestoreHit => "stage_restore_hit",
    StageRestoreMiss => "stage_restore_miss",
    SceneProducerReuse => "scene_producer_reuse",
    AsyncBakeBatch => "async_bake_batch", AsyncBakePoll => "async_bake_poll",
    AsyncBakeReady => "async_bake_ready", AsyncBakeCommit => "async_bake_commit",
    AsyncBakeCancel => "async_bake_cancel", AsyncBakeFailed => "async_bake_failed",
    RimBatchMerge => "rim_batch_merge",
    VertexBufferReuse => "vertex_buffer_reuse", VertexBufferRecycle => "vertex_buffer_recycle",
    TextCacheDirectGlyphs => "text_cache_direct_glyphs",
    TextCacheTargetReuse => "text_cache_target_reuse",
    TextCachePromote => "text_cache_promote",
    MovieGeometry => "movie_geometry", MovieOpen => "movie_open",
    MovieStartPoll => "movie_start_poll", MovieStartPending => "movie_start_pending",
    MovieStartReady => "movie_start_ready",
    MotionVariantBarrier => "motion_variant_barrier", MotionVariantDecode => "motion_variant_decode",
    ScriptSceneRead => "script_scene_read", ScriptSceneParse => "script_scene_parse",
    ScriptSceneCacheHit => "script_scene_cache_hit", ScriptCommand => "script_command",
    GradeBaseBuild => "grade_base_build", GradeBaseHit => "grade_base_hit",
    SoundEffectDecode => "sound_effect_decode",
    SoundPrefetch => "sound_prefetch", SoundDemandPending => "sound_demand_pending",
    SoundDemandReady => "sound_demand_ready", SoundDemandFailed => "sound_demand_failed",
    SoundDemandCancel => "sound_demand_cancel", SoundPreparedRelease => "sound_prepared_release",
    StreamIndexMiss => "stream_index_miss",
    AudioStreamStarveWall => "audio_stream_starve_wall",
    AudioStreamMissingFrames => "audio_stream_missing_frames",
    AudioStreamStartWall => "audio_stream_start_wall",
    MovieOwnerOpen => "movie_owner_open", MovieOwnerPoll => "movie_owner_poll",
    MovieOwnerControl => "movie_owner_control", MovieOwnerFrame => "movie_owner_frame",
    MovieOwnerClose => "movie_owner_close", MovieFrameCache => "movie_frame_cache",
}
// Console audit banks carry eight counters and use a bounded 0..32 index.
const _: () = assert!(NAMES.len() <= 32 * 8);

#[repr(C)]
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample { pub calls: u64, pub us: u64, pub bytes: u64, pub aux: u64 }
impl Sample {
    pub fn delta(self, previous: Self) -> Self {
        Self { calls: self.calls.saturating_sub(previous.calls), us: self.us.saturating_sub(previous.us),
            bytes: self.bytes.saturating_sub(previous.bytes), aux: self.aux.saturating_sub(previous.aux) }
    }
}
struct Counter { calls: AtomicU64, us: AtomicU64, bytes: AtomicU64, aux: AtomicU64 }
impl Counter {
    const fn new() -> Self { Self { calls: AtomicU64::new(0), us: AtomicU64::new(0),
        bytes: AtomicU64::new(0), aux: AtomicU64::new(0) } }
    fn sample(&self) -> Sample { Sample { calls: self.calls.load(Ordering::Relaxed),
        us: self.us.load(Ordering::Relaxed), bytes: self.bytes.load(Ordering::Relaxed),
        aux: self.aux.load(Ordering::Relaxed) } }
}
static COUNTERS: [Counter; Metric::Count as usize] = [const { Counter::new() }; Metric::Count as usize];
pub fn get(metric: Metric) -> Sample { COUNTERS[metric as usize].sample() }
pub fn snapshot() -> Vec<Sample> { COUNTERS.iter().map(Counter::sample).collect() }
#[inline]
pub fn add(metric: Metric, bytes: u64, aux: u64) {
    if enabled() { record(metric, 0, bytes, aux); }
}
fn record(metric: Metric, us: u64, bytes: u64, aux: u64) {
    let c = &COUNTERS[metric as usize];
    c.calls.fetch_add(1, Ordering::Relaxed); c.us.fetch_add(us, Ordering::Relaxed);
    c.bytes.fetch_add(bytes, Ordering::Relaxed); c.aux.fetch_add(aux, Ordering::Relaxed);
}

pub struct Scope { metric: Metric, start: Option<Instant> }
impl Scope {
    #[inline]
    pub fn new(metric: Metric) -> Self { Self { metric, start: enabled().then(Instant::now) } }
    pub fn active(&self) -> bool { self.start.is_some() }
    pub fn elapsed_us(&self) -> u64 { self.start.map_or(0, |s| s.elapsed().as_micros() as u64) }
    pub fn finish(mut self, bytes: u64, aux: u64) {
        if let Some(start) = self.start.take() { record(self.metric, start.elapsed().as_micros() as u64, bytes, aux); }
    }
    /// Audio-boundary diagnostics never wait for the logger or allocate labels.
    pub fn finish_hot_try(mut self, label: &str, bytes: u64, aux: u64) {
        if let Some(start) = self.start.take() {
            let us = start.elapsed().as_micros() as u64;
            record(self.metric, us, bytes, aux);
            hotspot_try(self.metric, label, us, bytes, aux);
        }
    }
    pub fn finish_hot(mut self, label: &str, bytes: u64, aux: u64) {
        if let Some(start) = self.start.take() {
            let us = start.elapsed().as_micros() as u64;
            record(self.metric, us, bytes, aux);
            hotspot(self.metric, label, us, bytes, aux);
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        // Failed operations still contribute elapsed time, with no successful bytes.
        if let Some(start) = self.start { record(self.metric, start.elapsed().as_micros() as u64, 0, 0); }
    }
}

#[repr(usize)]
#[derive(Clone, Copy)]
pub enum Gauge { StageInflight, StageRequestedBytes, StageReadyBytes, ResidentBytes, EncodedBytes, GradeLutBytes, GradeResultBytes, ArchiveTinyBytes, TextCacheBytes, TextCacheEntries, GroupCacheBytes, SourceCacheBytes, GlyphShadowBytes, StagePrefixBytes, GlyphPageBytes, GlyphPageEntries, AudioStreamChannels, AudioStreamBufferBytes, SoundCacheBytes, SoundCacheEntries, UiPassCacheBytes, UiPassCacheEntries, TargetRingBytes, AsyncBakeLayers, GradeBaseBytes, SoundPreparedBytes, SoundPreparedEntries, MovieFrameCacheBytes, Count }
pub const GAUGE_NAMES: &[&str] = &["stage_inflight", "stage_requested_bytes", "stage_ready_bytes",
    "resident_deferred_bytes", "retained_encoded_bytes", "grade_lut_bytes", "grade_result_bytes", "archive_tiny_bytes",
    "text_cache_bytes", "text_cache_entries", "group_cache_bytes", "source_cache_bytes",
    "glyph_shadow_bytes", "stage_prefix_bytes", "glyph_page_bytes", "glyph_page_entries",
    "audio_stream_channels", "audio_stream_buffer_bytes", "sound_cache_bytes", "sound_cache_entries",
    "ui_pass_cache_bytes", "ui_pass_cache_entries", "target_ring_bytes", "async_bake_layers", "grade_base_bytes", "sound_prepared_bytes", "sound_prepared_entries", "movie_frame_cache_bytes"];
const _: () = assert!(GAUGE_NAMES.len() == Gauge::Count as usize);
static GAUGES: [AtomicU64; Gauge::Count as usize] = [const { AtomicU64::new(0) }; Gauge::Count as usize];
static PEAKS: [AtomicU64; Gauge::Count as usize] = [const { AtomicU64::new(0) }; Gauge::Count as usize];
pub fn gauge_set(g: Gauge, value: u64) {
    if !enabled() { return; }
    GAUGES[g as usize].store(value, Ordering::Relaxed);
    PEAKS[g as usize].fetch_max(value, Ordering::Relaxed);
}
pub fn gauge_add(g: Gauge, amount: u64) {
    if !enabled() { return; }
    let value = GAUGES[g as usize].fetch_add(amount, Ordering::Relaxed) + amount;
    PEAKS[g as usize].fetch_max(value, Ordering::Relaxed);
}
pub fn gauge_sub(g: Gauge, amount: u64) {
    if enabled() { GAUGES[g as usize].fetch_sub(amount, Ordering::Relaxed); }
}
pub fn gauges() -> Vec<(u64, u64)> { GAUGES.iter().zip(&PEAKS)
    .map(|(v,p)| (v.load(Ordering::Relaxed), p.load(Ordering::Relaxed))).collect() }

#[derive(Clone)]
pub struct Hotspot { pub metric: usize, pub label: String, pub sample: Sample }
static HOT: Mutex<Vec<Hotspot>> = Mutex::new(Vec::new());
static HOT_OVERFLOW: AtomicU64 = AtomicU64::new(0);
const HOT_CAP: usize = 128;
const HOT_PER_METRIC: usize = 16;
fn hotspot(metric: Metric, label: &str, us: u64, bytes: u64, aux: u64) {
    let Ok(mut table) = HOT.lock() else { return; };
    let mut end = label.len().min(80);
    while !label.is_char_boundary(end) { end -= 1; }
    let label = &label[..end];
    let row = if let Some(index) = table.iter().position(|r| r.metric == metric as usize && r.label == label) {
        &mut table[index]
    } else {
        if table.len() == HOT_CAP || table.iter().filter(|r| r.metric == metric as usize).count() >= HOT_PER_METRIC {
            HOT_OVERFLOW.fetch_add(1, Ordering::Relaxed); return;
        }
        table.push(Hotspot { metric: metric as usize, label: label.into(), sample: Sample::default() });
        table.last_mut().unwrap()
    };
    row.sample.calls += 1; row.sample.us += us; row.sample.bytes += bytes; row.sample.aux += aux;
}
// A fixed, allocation-free table for realtime audio boundary events. Strings
// are materialized only by the main-thread logger after copying this table.
#[derive(Clone, Copy)]
struct FixedHotspot { metric: usize, label: [u8; 80], len: usize, sample: Sample }
const EMPTY_FIXED: FixedHotspot = FixedHotspot { metric: 0, label: [0; 80], len: 0,
    sample: Sample { calls: 0, us: 0, bytes: 0, aux: 0 } };
struct RealtimeHotspots { rows: [FixedHotspot; 32], len: usize }
const EMPTY_REALTIME: RealtimeHotspots = RealtimeHotspots { rows: [EMPTY_FIXED; 32], len: 0 };
static REALTIME_HOT: Mutex<RealtimeHotspots> = Mutex::new(EMPTY_REALTIME);
fn hotspot_try(metric: Metric, label: &str, us: u64, bytes: u64, aux: u64) {
    let Ok(mut table) = REALTIME_HOT.try_lock() else {
        HOT_OVERFLOW.fetch_add(1, Ordering::Relaxed); return;
    };
    let mut end = label.len().min(80);
    while !label.is_char_boundary(end) { end -= 1; }
    let label = &label.as_bytes()[..end];
    let index = if let Some(index) = table.rows[..table.len].iter()
        .position(|row| row.metric == metric as usize && &row.label[..row.len] == label) { index }
    else {
        if table.len == table.rows.len() { HOT_OVERFLOW.fetch_add(1, Ordering::Relaxed); return; }
        let index = table.len; table.len += 1;
        let row = &mut table.rows[index]; row.metric = metric as usize;
        row.len = label.len(); row.label[..label.len()].copy_from_slice(label); index
    };
    let row = &mut table.rows[index];
    row.sample.calls += 1; row.sample.us += us; row.sample.bytes += bytes; row.sample.aux += aux;
}
/// Per-window bounded table, ranked by CPU time. Overflow remains explicit.
pub fn take_hotspots(limit: usize) -> (Vec<Hotspot>, u64, u64) {
    let mut table = HOT.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows = std::mem::take(&mut *table);
    drop(table);
    let fixed = {
        let mut table = REALTIME_HOT.lock().unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *table, EMPTY_REALTIME)
    };
    rows.extend(fixed.rows[..fixed.len].iter().map(|row| Hotspot { metric: row.metric,
        // Input truncation above always preserves the original UTF-8 boundary.
        label: std::str::from_utf8(&row.label[..row.len]).expect("bounded hotspot UTF-8").into(), sample: row.sample }));
    rows.sort_by_key(|r| std::cmp::Reverse(r.sample.us));
    let omitted = rows.len().saturating_sub(limit) as u64;
    rows.truncate(limit);
    (rows, HOT_OVERFLOW.swap(0, Ordering::Relaxed), omitted)
}

pub fn json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c { '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
            c if c < ' ' => { let _ = write!(out, "\\u{:04x}", c as u32); }, _ => out.push(c) }
    }
    out.push('"');
}
pub fn sample_json(out: &mut String, s: Sample) {
    let _ = write!(out, "{{\"calls\":{},\"us\":{},\"bytes\":{},\"aux\":{}}}", s.calls, s.us, s.bytes, s.aux);
}
pub fn bank_json(out: &mut String, names: &[&str], samples: &[Sample]) {
    out.push('{'); let mut first = true;
    for (name, sample) in names.iter().zip(samples) {
        if *sample == Sample::default() { continue; }
        if !first { out.push(','); } first = false;
        json_string(out, name); out.push(':'); sample_json(out, *sample);
    }
    out.push('}');
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disabled_scopes_are_inert_and_enabled_bytes_exceed_u32_without_wrapping() {
        set_enabled(false);
        let before = get(Metric::NxRange);
        Scope::new(Metric::NxRange).finish(10, 1);
        assert_eq!(get(Metric::NxRange), before);
        set_enabled(true);
        Scope::new(Metric::NxRange).finish_hot("atlas\"\\\n", 10_556_590_462, 65_632);
        let delta = get(Metric::NxRange).delta(before);
        assert_eq!((delta.calls, delta.bytes, delta.aux), (1, 10_556_590_462, 65_632));
        let (hot, _, _) = take_hotspots(8); assert_eq!(hot.len(), 1);
        let mut quoted = String::new(); json_string(&mut quoted, &hot[0].label);
        assert_eq!(quoted, "\"atlas\\\"\\\\\\u000a\"");
        for i in 0..70 { Scope::new(Metric::NxRange).finish_hot(&i.to_string(), 1, 1); }
        let (rows, dropped, omitted) = take_hotspots(8);
        assert_eq!(rows.len(), 8); assert_eq!(dropped, 54); assert_eq!(omitted, 8);
        assert_eq!(take_hotspots(8).0.len(), 0);
        set_enabled(false);
    }
}

pub mod optimizations;

/// Called at actual command execution, not merely at abandoned recording.
pub fn record_ps(op: u32) {
    const OPS: &[Metric] = &[Metric::PsOp1, Metric::PsOp2, Metric::PsOp3, Metric::PsOp4, Metric::PsOp5, Metric::PsOp6, Metric::PsOp7, Metric::PsOp8, Metric::PsOp9, Metric::PsOp10, Metric::PsOp11, Metric::PsOp12, Metric::PsOp13, Metric::PsOp14, Metric::PsOp15, Metric::PsOp16, Metric::PsOp17, Metric::PsOp18, Metric::PsOp19, Metric::PsOp20, Metric::PsOp21, Metric::PsOp22, Metric::PsOp23, Metric::PsOp24];
    if let Some(metric) = op.checked_sub(1).and_then(|i| OPS.get(i as usize)) { add(*metric, 0, 0); }
}

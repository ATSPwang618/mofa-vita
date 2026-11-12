//! Windowless native Vita entry using the game's ordinary Game/Ui state machines.
use crate::{assets, game, motion, save, savedata, ui, vita_input::Input, vita_watchdog as watchdog};
use anyhow::{Context, Result};
use knife_platform_vita::{asset_check, boot_error, gxm::Gxm};
use knife_render::Renderer;
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const ASSETS: &str = "ux0:data/WitchontheHolyNight/assets";

// Performance switch and log live beside the game's own data root, next to
// assets/, saves/ and the runtime shader cache.
const PERF_ROOT: &str = "ux0:data/WitchontheHolyNight";

/// Build identity recorded in the log header, so two logs can never be confused.
unsafe extern "C" {
    fn kv_set_performance_clock(arm: *mut i32, bus: *mut i32, gpu: *mut i32, xbar: *mut i32);
    fn kv_clock_mhz(arm: *mut u32, bus: *mut u32, gpu: *mut u32);
}

/// Measured clock triple, published before the log header is written.
static CLOCKS: std::sync::OnceLock<(u32, u32, u32)> = std::sync::OnceLock::new();
/// Return codes from the clock requests, so a refusal is visible in the log.
static CLOCK_RC: std::sync::OnceLock<(i32, i32, i32, i32)> = std::sync::OnceLock::new();

/// Build identity baked at compile time so every log states which package produced it.
const BUILD_TAG: &str = match option_env!("KNIFE_BUILD_TAG") {
    Some(tag) => tag,
    None => "dev",
};

fn build_identity() -> String {
    let mut tag = String::new();
    knife_perf::json_string(&mut tag, BUILD_TAG);
    format!("{{\"kind\":\"build_identity\",\"build_tag\":{tag},\"sound_cache_limit\":{}}}\n",
        knife_perf::memory_budget::CPU_SOUND_BYTES)
}

fn quoted(value: &str) -> String {
    let mut out = String::new();
    knife_perf::json_string(&mut out, value);
    out
}

const TITLE_ID: &str = match option_env!("KNIFE_VITA_TITLE_ID") {
    Some(id) => id,
    None => "KNIF00540",
};

// Console-only measurement, fixed histogram and bounded detail every five seconds.
// Disabled runs do not take the additional snapshots or format samples.
struct PerfLog {
    started: Instant,
    window: Instant,
    frames: knife_platform_vita::frame_timing::FrameWindow,
    before: crate::vita_audit::FrameProbe,
    classes: crate::vita_audit::FrameClasses,
    pending_class: Option<crate::vita_audit::SlowFrame>,
    class_started: Instant,
    worst: Option<crate::vita_audit::SlowFrame>,
    previous_console_us: u64,
    snapshot_us: u64,
}
impl PerfLog {
    fn open() -> std::io::Result<Option<Self>> {
        if option_env!("KNIFE_VITA_QUIET") == Some("1") { return Ok(None); }
        if !knife_platform_vita::perf_switch::console_enabled(Path::new(PERF_ROOT),
            option_env!("KNIFE_PERF_CONSOLE") == Some("1"))? { return Ok(None); }
        knife_platform_vita::audit::console(&build_identity())?;
        let (scene, canvas) = knife_render::performance_dimensions();
        let profile = "540p-native-unorm-v2";
        let quoted = |value: &str| { let mut out = String::new(); knife_perf::json_string(&mut out, value); out };
        // Read the loader-visible heap/stack sizes: a zero here proves a constant
        // was dropped by --gc-sections, which no build receipt can catch.
        let memory = knife_platform_vita::runtime_memory();
        let header = format!(concat!("{{\"kind\":\"header\",\"schema\":3,\"guest_renderer\":\"direct GXM\",",
            "\"newlib_heap_bytes\":{},\"libc_heap_bytes\":{},\"main_stack_bytes\":{},",
            "\"main_priority\":{},",
            "\"build_tag\":{},\"arm_mhz\":{},\"bus_mhz\":{},\"gpu_mhz\":{},",
            "\"clock_rc\":[{},{},{},{}],\"scene_divisor\":{},\"scene_mode\":{},",
            "\"ui_tex_error\":{},",
            "\"title_id\":\"{}\",\"profile\":{},\"asset_manifest_sha256\":{},\"flag\":\"{}\",",
            "\"scene_size\":[{},{}],\"composite_size\":[{},{}],\"resolution_source\":\"code\",\"color_contract\":\"encoded-unorm8-uchar4\",\"asset_gxt_codec\":9,",
            "\"audio_pcm_pool_bytes\":23040,\"audio_queue_blocks\":4,",
            "\"audio_block_us\":20000,\"audio_mix_scope\":\"ready PCM mix; stream IO/decode on independent channels\",",
            "\"audio_stream_channels\":8,\"audio_stream_queue_blocks\":10,\"audio_stream_pool_blocks\":12,",
            "\"audio_bgm_queue_blocks\":50,\"audio_bgm_pool_blocks\":52,\"audio_bgm_prefetch_ms\":1000,\"audio_bgm_pcm_pool_bytes\":399360,",
            "\"audio_stream_pool_ceiling_bytes\":3194880,\"audio_stream_pool_ceiling_scope\":\"8 simultaneous BGM-sized pools; normal voice pool is92160 bytes\",\"audio_stream_missing_scope\":\"per-stream absent PCM sample frames; excludes intentional pauses; device underrun is independent\",",
            "\"frame_work_excludes_pacer\":true,\"render_is_cpu_submission_including_waits\":true,",
            "\"memory_scope\":\"GXM mappings and separate JPEG temporary CDRAM; excludes Rust and codec heap\",",
            "\"worker_time_scope\":\"archive read plus selected software/hardware decode\",\"movie_owner_scope\":\"native open/poll/control/frame/close run on one persistent worker; frame cache is main-thread GPU submission plus fence\",",
            "\"jpeg_hardware_cdram_reserve\":3145728,",
            "\"image_worker_audio_counters\":\"cumulative u32; wrap possible\",\"interval_ms\":5000,\"sink\":\"sceClibPrintf\",\"audit_schema\":1,\"audit_counter_bits\":64,\"frame_work_excludes_logger\":true}}\n"),
            memory.newlib_heap_bytes, memory.libc_heap_bytes, memory.main_stack_bytes,
            memory.main_priority, quoted(BUILD_TAG), CLOCKS.get().copied().unwrap_or((0, 0, 0)).0,
            CLOCKS.get().copied().unwrap_or((0, 0, 0)).1, CLOCKS.get().copied().unwrap_or((0, 0, 0)).2,
            CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).0, CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).1,
            CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).2, CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).3,
            knife_render::scene_divisor(), quoted("fixed-540p"),
            quoted(&crate::ui::last_atlas_error().unwrap_or_default()),
            TITLE_ID, quoted(profile), quoted(knife_platform_vita::audit::ASSET_MANIFEST), "console", scene[0], scene[1], canvas[0], canvas[1]);
        let header = header.trim_end().strip_suffix('}').expect("header JSON object").to_owned()
            + &format!(",\"optimization_mask\":{},\"optimizations\":{},\"stage_signature\":\"complete-v1\",\"frame_classes_schema\":1,\"frame_classes_scope\":\"redraw/reuse/other by actual stage events; wall settled at next frame probe boundary; first_frame/last_frame authoritative\",\"named_wait_scope\":\"finish+transfer+batch+display; EndScene separate; copy/draw parent scopes overlap\"}}\n",
                knife_perf::optimizations::mask(), quoted(&knife_perf::optimizations::names()));
        let header = header.trim_end().strip_suffix('}').expect("header object").to_owned()
            + &format!(concat!(",\"logger_transport\":\"bounded-worker-v1\",\"audit_bank_policy\":\"changed-cumulative-v1\",",
                "\"previous_console_scope\":\"main collect/enqueue; printf is console_worker_last_us\",",
                "\"language_fixed\":{},\"cache_budgets\":{{\"cpu_source\":{},\"cpu_graded\":{},\"cpu_grade_base\":{},\"cpu_sound\":{},\"cpu_sound_prepare\":{},\"cpu_sound_pins\":{},\"gpu_resource\":{},\"gpu_text\":{},\"gpu_text_entries\":{},\"gpu_group\":{},\"gpu_target_ring\":{},\"gpu_movie_frame\":{},\"gpu_total_with_jpeg\":109051904}}}}\n"),
                2,
                knife_perf::memory_budget::CPU_SOURCE_BYTES, knife_perf::memory_budget::CPU_GRADED_BYTES,
                knife_perf::memory_budget::CPU_GRADE_BASE_BYTES, knife_perf::memory_budget::CPU_SOUND_BYTES,
                knife_perf::memory_budget::CPU_SOUND_PREPARE_BYTES, knife_perf::memory_budget::CPU_SOUND_PREPARED_BYTES,
                knife_perf::memory_budget::GPU_RESOURCE_BYTES, knife_perf::memory_budget::GPU_TEXT_BYTES,
                knife_perf::memory_budget::GPU_TEXT_ENTRIES, knife_perf::memory_budget::GPU_GROUP_BYTES,
                knife_perf::memory_budget::GPU_TARGET_RING_BYTES, knife_perf::memory_budget::GPU_MOVIE_FRAME_BYTES);
        knife_platform_vita::audit::console(&header)?;
        knife_perf::set_enabled(true);
        knife_platform_vita::audit::set_enabled(true);
        Ok(Some(Self { started: Instant::now(), window: Instant::now(), frames: Default::default(),
            before: Default::default(), classes: Default::default(), pending_class: None,
            class_started: Instant::now(), worst: None, previous_console_us: 0, snapshot_us: 0 }))
    }
    fn begin_frame(&mut self) {
        let started = Instant::now();
        if let Some(previous) = self.pending_class.take() {
            self.classes.record(previous, started.duration_since(self.class_started).as_micros() as u64);
        }
        self.class_started = started;
        self.before = crate::vita_audit::FrameProbe::capture();
        self.snapshot_us += started.elapsed().as_micros() as u64;
    }
    fn finish_startup(&mut self) -> std::io::Result<()> {
        let started = Instant::now();
        let batch = knife_platform_vita::audit::ConsoleBatch::begin();
        crate::vita_audit::emit(0, "startup", None)?;
        batch.finish()?;
        self.previous_console_us = started.elapsed().as_micros() as u64;
        self.window = Instant::now();
        Ok(())
    }

    fn record(&mut self, frame: u64, work: u64, update: u64, render: u64, wall: u64,
        game: &game::Game, renderer: &Renderer, ui: &ui::Ui) -> std::io::Result<bool> {
        self.frames.record(work, update, render, wall);
        let snapshot_started = Instant::now();
        let probe = crate::vita_audit::FrameProbe::capture().delta(self.before);
        self.snapshot_us += snapshot_started.elapsed().as_micros() as u64;
        let observed = crate::vita_audit::SlowFrame { frame, scene: game.scene_index,
            pc: game.current_pc(), work, update, render, probe };
        if self.worst.is_none_or(|w| work > w.work) { self.worst = Some(observed); }
        self.pending_class = Some(observed);
        if self.window.elapsed() < Duration::from_secs(5) { return Ok(true); }
        let console_started = Instant::now();
        let batch = knife_platform_vita::audit::ConsoleBatch::begin();
        // A short identity survives late attachment and stays below chunk size.
        knife_platform_vita::audit::console(&build_identity())?;
        let elapsed = self.window.elapsed().as_millis();
        let sample = self.frames.take();
        let (image, worker) = game.assets.image_performance();
        let codec = knife_platform_vita::image::decode_stats();
        let audio = knife_platform_vita::audio_output::metrics::snapshot();
        let gpu = renderer.vita_performance_stats().ok();
        let gpu_work = renderer.vita_work_stats_json();
        let gpu_timing = renderer.vita_gpu_timing().unwrap_or_default();
        let io = knife_core::io_metrics::snapshot();
        let (deferred_items, deferred_item_us, deferred_upload_us, deferred_bake_us) =
            renderer.vita_deferred_timing();
        let passes = renderer.vita_pass_summary(6);
        let (exec_us, present_us, submitted_frames, quads, small_quads, mid_quads) =
            renderer.vita_frame_breakdown();
        let (large_quads, _) = (knife_render::large_quad_count(), 0u64);
        let (main, cdram, parameter) = gpu.map(|g| (g.main_mapped_bytes,
            g.cdram_mapped_bytes, g.parameter_reserved_bytes)).unwrap_or_default();
        let line = format!(concat!("{{\"kind\":\"sample\",\"elapsed_ms\":{},\"window_ms\":{},",
            "\"frame\":{},\"frames\":{},\"scene\":{},\"pc\":{},",
            "\"work_us\":{},\"update_us\":{},\"render_us\":{},\"wall_us\":{},",
            "\"max_work_us\":{},\"p95_work_upper_us\":{},\"over_16ms\":{},\"over_33ms\":{},",
            "\"demand_decodes\":{},\"demand_us\":{},\"worker_reused\":{},\"demand_wait_us\":{},",
            "\"uploads\":{},\"upload_us\":{},\"max_upload_us\":{},\"prefetch_skipped\":{},",
            "\"gxt_images\":{},\"decoded_images\":{},",
            "\"worker_threads\":{},\"worker_completed\":{},\"worker_stale\":{},",
            "\"queue_full\":{},\"worker_failed\":{},\"pending\":{},\"ready\":{},\"active\":{},",
            "\"peak_pending\":{},\"peak_ready\":{},\"worker_read_decode_us\":{},",
            "\"worker_max_read_decode_us\":{},\"gxm_stats_available\":{},",
            "\"gxm_draws\":{},\"gxm_frames\":{},",
            "\"gpu_work\":{},",
            "\"gxm_main_mapped_bytes\":{},\"gxm_cdram_mapped_bytes\":{},\"gxm_parameter_reserved_bytes\":{},",
            "\"card_read_us\":{},\"card_read_calls\":{},\"card_read_bytes\":{},\"max_card_read_us\":{},",
            "\"nx_inflate_us\":{},\"nx_inflate_calls\":{},",
            "\"gpu_finish_us\":{},\"gpu_finish_calls\":{},\"gpu_end_scene_us\":{},\"gpu_end_scene_calls\":{},",
            "\"gpu_display_finish_us\":{},\"gpu_display_finish_calls\":{},",
            "\"gpu_target_create_us\":{},\"gpu_target_create_calls\":{},",
            "\"begin_target_us\":{},\"begin_target_calls\":{},\"clear_us\":{},",
            "\"draw_cpu_us\":{},\"draw_gxm_us\":{},\"draw_calls\":{},",
            "\"deferred_items\":{},\"deferred_item_us\":{},\"deferred_upload_us\":{},",
            "\"deferred_bake_us\":{},\"exec_us\":{},\"present_submit_us\":{},",
            "\"submitted_frames\":{},\"quads\":{},\"quads_small\":{},\"quads_mid\":{},",
            "\"quads_large\":{},\"stage_repeats\":\"{}\",\"layer_changes\":\"{}\",",
            "\"group_shapes\":\"{}\",\"pass_need\":\"{}\",\"pass_top\":\"{}\",",
            "\"jpeg_hw_candidates\":{},\"jpeg_hw_ok\":{},\"jpeg_fallback_profile\":{},",
            "\"jpeg_fallback_busy\":{},\"jpeg_fallback_budget\":{},\"jpeg_fallback_init\":{},",
            "\"jpeg_fallback_alloc\":{},\"jpeg_fallback_driver\":{},\"jpeg_fallback_contract\":{},",
            "\"software_png_ok\":{},\"software_jpeg_ok\":{},\"jpeg_work_cdram\":{},",
            "\"jpeg_peak_work_cdram\":{},\"jpeg_last_hw_error\":{},",
            "\"jpeg_contract_mask\":{},\"jpeg_info_size\":{},\"jpeg_info_y_pitch\":{},",
            "\"jpeg_info_chroma_pitch\":{},\"jpeg_info_color_space\":{},",
            "\"audio_submitted\":{},\"audio_output\":{},\"audio_underruns\":{},\"audio_idle\":{},",
            "\"audio_queue_full\":{},\"audio_peak_queued\":{},\"audio_mix_blocks\":{},",
            "\"audio_mix_max_us\":{},\"audio_mix_late\":{},\"audio_clipped_samples\":{},",
            "\"p50_work_upper_us\":{},\"p99_work_upper_us\":{},\"previous_console_us\":{},\"probe_snapshot_us\":{}}}\n"),
            self.started.elapsed().as_millis(), elapsed, frame, sample.frames, game.scene_index, game.current_pc(),
            sample.work_us, sample.update_us, sample.render_us, sample.wall_us,
            sample.max_work_us, sample.p95_work_upper_us, sample.over_16ms, sample.over_33ms,
            image.demand_decodes, image.demand_us, image.worker_reused, image.demand_wait_us,
            image.uploads, image.upload_us, image.max_upload_us, image.skipped,
            image.gxt_images, image.decoded_images,
            worker.threads_started, worker.completed, worker.stale, worker.queue_full, worker.failed,
            worker.pending, worker.ready, worker.active, worker.peak_pending, worker.peak_ready,
            worker.decode_us, worker.max_decode_us, gpu.is_some(),
            gpu.map(|g| g.draws).unwrap_or(0), gpu.map(|g| g.frames).unwrap_or(0),
            gpu_work,
            main, cdram, parameter,
            io.card_read_us, io.card_read_calls, io.card_read_bytes, io.max_card_read_us,
            io.inflate_us, io.inflate_calls,
            gpu_timing.finish_us, gpu_timing.finish_calls,
            gpu_timing.end_scene_us, gpu_timing.end_scene_calls,
            gpu_timing.display_finish_us, gpu_timing.display_finish_calls,
            gpu_timing.target_create_us, gpu_timing.target_create_calls,
            gpu_timing.begin_target_us, gpu_timing.begin_target_calls, gpu_timing.clear_us,
            gpu_timing.draw_cpu_us, gpu_timing.draw_gxm_us, gpu_timing.draw_calls,
            deferred_items, deferred_item_us, deferred_upload_us, deferred_bake_us,
            exec_us, present_us, submitted_frames, quads, small_quads, mid_quads, large_quads,
            crate::game::Game::stage_repeat_summary(), crate::game::Game::layer_change_summary(),
            crate::game::Game::group_shape_summary(), knife_render::pass_need_summary(), passes,
            codec.jpeg_hardware_candidates, codec.jpeg_hardware_ok, codec.jpeg_fallback_profile,
            codec.jpeg_fallback_busy, codec.jpeg_fallback_budget, codec.jpeg_fallback_init,
            codec.jpeg_fallback_alloc, codec.jpeg_fallback_driver, codec.jpeg_fallback_contract,
            codec.software_png_ok, codec.software_jpeg_ok, codec.hw_current_cdram,
            codec.hw_peak_cdram, codec.last_hw_error, codec.last_contract_mask,
            codec.last_info_size, codec.last_info_y_pitch, codec.last_info_chroma_pitch,
            codec.last_info_color_space, audio.submitted, audio.output, audio.underruns, audio.idle,
            audio.queue_full, audio.peak_queued, audio.mix_blocks, audio.mix_max_us, audio.mix_late,
            audio.clipped_samples, sample.p50_work_upper_us, sample.p99_work_upper_us, self.previous_console_us, self.snapshot_us);
        let transport = knife_platform_vita::audit::console_stats();
        let memory = knife_platform_vita::memory_available().ok();
        let available = memory.unwrap_or_default();
        // UI is separate from script scene/PC: title-hosted load/config pages
        // can all report scene=0, PC=0. Capture only at the five-second boundary.
        let (screen, title_state, title_cursor, overlay, menu_state, menu_cursor) = ui.vita_diagnostic_state();
        let quote = |text: &str| { let mut out = String::new(); knife_perf::json_string(&mut out, text); out };
        let ui_state = format!(concat!("{{\"screen\":{},\"title_state\":{},\"title_cursor\":{},",
            "\"overlay\":{},\"menu_state\":{},\"menu_cursor\":{}}}"),
            quote(&screen), title_state, title_cursor, quote(&overlay), menu_state, menu_cursor);
        let line = line.trim_end().strip_suffix('}').expect("sample object").to_owned()
            + &format!(concat!(",\"console_worker_last_us\":{},\"console_worker_total_us\":{},\"console_batches\":{},",
                "\"console_dropped_batches\":{},\"console_write_failures\":{},\"console_queued_bytes\":{},",
                "\"language_id\":{},\"memory_available\":{},\"kernel_main_free\":{},\"kernel_cdram_free\":{},\"heap_used\":{},\"heap_headroom\":{},\"ui_at_window_end\":{}}}\n"),
                transport.last_us, transport.total_us, transport.completed, transport.dropped, transport.failures,
                transport.queued_bytes, game.assets.language(), memory.is_some(), available.kernel_main,
                available.kernel_cdram, available.heap_used, available.heap_headroom, ui_state);
        knife_platform_vita::audit::console(&line)?;
        let compact = format!("{{\"kind\":\"frame_window\",\"frame\":{frame},\"frames\":{},\"elapsed_ms\":{},\"window_ms\":{},\"scene\":{},\"pc\":{},\"work_us\":{},\"render_us\":{},\"update_us\":{},\"wall_us\":{},\"max_work_us\":{},\"p50_work_upper_us\":{},\"p95_work_upper_us\":{},\"p99_work_upper_us\":{},\"over_16ms\":{},\"over_33ms\":{}}}\n",
            sample.frames,self.started.elapsed().as_millis(),elapsed,game.scene_index,game.current_pc(),
            sample.work_us,sample.render_us,sample.update_us,sample.wall_us,sample.max_work_us,
            sample.p50_work_upper_us,sample.p95_work_upper_us,sample.p99_work_upper_us,sample.over_16ms,sample.over_33ms);
        knife_platform_vita::audit::console(&compact)?;
        self.classes.emit(frame, elapsed)?;
        crate::vita_audit::emit(frame, "window", self.worst.take())?;
        batch.finish()?;
        self.previous_console_us = console_started.elapsed().as_micros() as u64;
        self.window = Instant::now();
        Ok(true)
    }
}

struct DevLog(bool);
impl DevLog {
    const PATH: &'static str = "ux0:data/knife-woh-dev/game-runtime.log";
    fn open() -> Result<Self> {
        if option_env!("KNIFE_VITA_DEV_DIAGNOSTICS") != Some("1") { return Ok(Self(false)); }
        std::fs::create_dir_all("ux0:data/knife-woh-dev")
            .context("create DEV game diagnostic directory")?;
        std::fs::OpenOptions::new().create(true).truncate(true).write(true)
            .open(Self::PATH).context("open DEV game runtime log")?;
        Ok(Self(true))
    }
    fn event(&mut self, text: &str) {
        if self.0 {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(Self::PATH) {
                let _ = writeln!(file, "{text}");
                let _ = file.sync_all();
            }
        }
    }
}

/// On hardware a panic otherwise only aborts, leaving a core with no symbols. This
/// prints the location and message to the console so one run identifies it.
fn install_panic_hook() {
    if option_env!("KNIFE_VITA_QUIET") == Some("1") {
        std::panic::set_hook(Box::new(|_| {}));
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".into());
        let message = info.payload().downcast_ref::<&str>().map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let mut line = String::from("{\"kind\":\"panic\",\"location\":");
        knife_perf::json_string(&mut line, &location);
        line.push_str(",\"message\":");
        let message: String = message.chars().take(1000).collect();
        knife_perf::json_string(&mut line, &message);
        line.push_str("}\n");
        let _ = knife_platform_vita::audit::console(&line);
    }));
}

pub fn run() -> Result<()> {
    let mut log = DevLog::open()?;
    if log.0 {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            DevLog(true).event(&format!("PANIC {info}"));
            previous(info);
        }));
    }
    log.event("BOOT begin");
    let result = run_game(&mut log);
    match &result {
        Ok(()) => log.event("RESULT PASS"),
        Err(error) => {
            log.event(&format!("ERROR {error:#}"));
            log.event("RESULT FAIL");
        },
    }
    result
}

fn run_game(log: &mut DevLog) -> Result<()> {
    install_panic_hook();
    savedata::set_language_override(2);
    let limit = if option_env!("KNIFE_VITA_DEV_DIAGNOSTICS") == Some("1") {
        option_env!("KNIFE_VITA_DEV_MAX_FRAMES").and_then(|v| v.parse::<u64>().ok()).filter(|v| *v > 0)
    } else { None };
    let mut frame = 0u64;
    // Standard game clocks (ARM 444 / BUS 222 / GPU 222). The port previously ran at
    // the 111 MHz GPU default, which made every render pass cost 12-20 ms.
    let mut arm_mhz = 0u32;
    let mut bus_mhz = 0u32;
    let mut gpu_mhz = 0u32;
    let (mut rc_arm, mut rc_bus, mut rc_gpu, mut rc_xbar) = (0i32, 0, 0, 0);
    unsafe {
        kv_set_performance_clock(&mut rc_arm, &mut rc_bus, &mut rc_gpu, &mut rc_xbar);
        kv_clock_mhz(&mut arm_mhz, &mut bus_mhz, &mut gpu_mhz);
    }
    let _ = CLOCKS.set((arm_mhz, bus_mhz, gpu_mhz));
    let _ = CLOCK_RC.set((rc_arm, rc_bus, rc_gpu, rc_xbar));
    // Full-scene targets are fixed to the compiled 540p asset profile.
    // Runtime configuration no longer changes scene or composite dimensions.

    let optimization_file = format!("{PERF_ROOT}/optimization-profile.txt");
    let file_profile = match std::fs::read_to_string(&optimization_file) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error).context("reading optimization profile"),
    };
    let compiled_profile = option_env!("KNIFE_VITA_OPTIMIZATIONS")
        .unwrap_or(knife_perf::optimizations::HARDWARE_TEST_PROFILE);
    let selected = knife_perf::optimizations::resolve_startup(compiled_profile, file_profile.as_deref())
        .map_err(anyhow::Error::msg)?;
    knife_perf::optimizations::set(selected.mask);
    knife_platform_vita::audit::set_options(selected.mask);
    let profile_source = if selected.legacy_expanded { "legacy-stock-expanded" }
        else if file_profile.is_some() { "file" } else { "compiled-default" };
    log.event(&format!("OPTIMIZATIONS source={profile_source} legacy-expanded={} mask={} effective={}",
        selected.legacy_expanded, selected.mask, knife_perf::optimizations::names()));

    log.event("SCENE fixed=960x540 composite=960x540 source=code");
    // Emit the selected dimensions before archive, font and renderer startup.
    let mut perf = match PerfLog::open() {
        Ok(log) => log,
        Err(error) => { tracing::warn!("Optional Vita console metrics disabled: {error}"); None },
    };
    if perf.is_some() {
        let record = format!(concat!("{{\"kind\":\"optimization_profile\",\"build_tag\":{},",
            "\"compiled_profile\":{},\"source\":{},\"legacy_expanded\":{},\"legacy_profile\":{},",
            "\"optimization_mask\":{},\"optimizations\":{}}}\n"),
            quoted(BUILD_TAG), quoted(compiled_profile), quoted(profile_source), selected.legacy_expanded,
            quoted(if selected.legacy_expanded { file_profile.as_deref().unwrap_or("").trim() } else { "" }),
            selected.mask, quoted(&knife_perf::optimizations::names()));
        if let Err(error) = knife_platform_vita::audit::console(&record) {
            tracing::warn!("Optimization profile console record unavailable: {error}");
        }
        if let Err(error) = watchdog::start() {
            tracing::warn!("Runtime watchdog unavailable: {error}");
        }
    }
    let assets_dir = PathBuf::from(ASSETS);
    let check_started = Instant::now();
    match asset_check::check_assets(&assets_dir) {
        Ok(report) => {
            log.event(&format!("ASSETS profile={} key-files={} read-bytes={} check-ms={:.3}",
                "540p", report.files, report.read_bytes, check_started.elapsed().as_secs_f64() * 1000.));
            log.event("ASSETS verified Witch/EN+Chengkong");
        },
        Err(error) => {
            // Run before archives/fonts/renderer are loaded. The diagnostic uses
            // embedded ASCII glyphs and one 480x270 image, with no asset fallback.
            log.event(&format!("ASSET_CHECK REJECT {error}"));
            boot_error::show(&error, limit).map_err(anyhow::Error::msg)
                .context("display asset startup error")?;
            return Err(error.into());
        }
    }
    let build = assets::detect_build(&assets_dir);
    assets::select_build(build);
    save::set_file(save::default_file(build));
    savedata::set_system_language(1); // English default; save/profile resolves English and Chengkong.
    let nso = assets::exe_tables(build)?;
    let language = savedata::SaveFile::load().language();
    let assets = assets::Assets::open(&assets_dir, &nso, language)
        .context("open Vita game archives and language profile")?;
    log.event("ARCHIVES opened");
    let gxm = Gxm::new()
        .map_err(anyhow::Error::msg)
        .context("initialize direct GXM")?;
    let mut renderer = Renderer::new_with_gxm(gxm).context("initialize game renderer")?;
    if let Ok(rows) = assets.soft_lut_rows() {
        renderer.set_soft_lut(&rows);
    }
    log.event("GXM renderer ready");
    let motions = motion::MotionTable::load(&nso)?;
    let (font, bubble) = game::Game::load_fonts(&assets, &renderer)?;
    log.event("FONTS ready");
    let mut game = game::Game::new(assets, motions, font, &renderer);
    if let Some(font) = bubble {
        game.set_bubble_font(font);
    }
    log.event("GAME ready");
    let mut ui = ui::Ui::new(&mut game, &renderer);
    ui.boot(&mut game, &renderer, false);
    ui.warm_runtime_font(&game, &renderer);
    let mut input = Input::new(
        &mut game,
        Path::new(PERF_ROOT).join("story-font-size.txt"),
        Path::new(PERF_ROOT).join("l2-r2-mode.txt"),
    )?;
    log.event("UI booted; input ready");
    if let Some(perf_log) = &mut perf {
        if let Err(error) = perf_log.finish_startup() {
            tracing::warn!("Performance console stopped: {error}");
            perf = None;
            knife_perf::set_enabled(false);
            knife_platform_vita::audit::set_enabled(false);
        }
    }
    let mut last = Instant::now();
    let mut last_logged_buttons = 0;
    let mut last_save_focus = None;
    // Read once, never poll the storage card while playing. An explicit request
    // captures one normal production Stage without advancing its clock in replay.
    let mut gpu_replay = match std::fs::read_to_string(format!("{PERF_ROOT}/gpu-replay.txt")) {
        Ok(text) => match text.trim().split_whitespace().collect::<Vec<_>>().as_slice() {
            ["current"] => Some((None, 0usize)),
            [scene, pc] => scene.parse::<usize>().ok().zip(pc.parse::<usize>().ok())
                .map(|(scene, pc)| (Some(scene), pc)),
            _ => None,
        },
        Err(_) => None,
    };
    loop {
        watchdog::frame(frame + 1);
        watchdog::game(&game);
        watchdog::phase(watchdog::PERF_LOG);
        if let Some(log) = &mut perf { log.begin_frame(); }
        let now = Instant::now();
        let wall_us = perf.as_ref().map(|_| (now - last).as_micros() as u64).unwrap_or(0);
        let step = (now - last).as_secs_f32().mul_add(1000.0, 0.0).min(100.0);
        last = now;
        watchdog::phase(watchdog::INPUT);
        { let _scope = knife_perf::Scope::new(knife_perf::Metric::InputPoll);
            input.poll(&mut game, &mut ui, &renderer)?;
        }
        game.paused = ui.game_paused() || input.font_menu_open();
        let (held, advance) = ui.held_input();
        game.set_held(held, advance);
        game.flow_replay = ui.flow_replay();
        if !game.paused {
            ui.rewind_before_script(&mut game);
        }
        watchdog::phase(watchdog::STORY_INPUT);
        if !input.font_menu_open() { let _scope = knife_perf::Scope::new(knife_perf::Metric::StoryInput);
            ui.story_input(&mut game, &renderer, step);
        }
        watchdog::phase(watchdog::GAME_UPDATE);
        { let _scope = knife_perf::Scope::new(knife_perf::Metric::GameUpdate);
            game.update(&renderer, step);
        }
        watchdog::game(&game);
        watchdog::phase(watchdog::UI_UPDATE);
        { let _scope = knife_perf::Scope::new(knife_perf::Metric::UiUpdate);
            ui.update(&mut game, &renderer, step);
        }
        watchdog::phase(watchdog::AUDIO_FLUSH);
        { let _scope = knife_perf::Scope::new(knife_perf::Metric::AudioFlush);
            game.sound.flush();
        }
        game.system_menu_shown = ui.system_menu_shown();
        let update_us = perf.as_ref().map(|_| now.elapsed().as_micros() as u64).unwrap_or(0);
        let render_started = perf.as_ref().map(|_| Instant::now());
        watchdog::game(&game);
        watchdog::phase(watchdog::RENDER);
        if gpu_replay.is_some_and(|(scene, pc)| scene.is_none_or(|s| game.scene_index == s)
            && game.current_pc() >= pc && !ui.game_paused()) {
            if let Err(error) = game.vita_gpu_replay_arm() {
                let _ = knife_platform_vita::audit::console(&format!("{{\"kind\":\"gpu_replay_error\",\"error\":{}}}\n", quoted(&error.to_string())));
            }
            gpu_replay = None;
        }
        game.render(&mut renderer, &mut |game, renderer, quads| {
            ui.draw(game, renderer, quads);
            input.draw_font_menu(game, renderer, quads);
        });
        // Protect progress before next-frame input can leave/reset this fault.
        ui.protect_resource_checkpoint(&mut game);
        let replay_capture = game.vita_gpu_replay_take_capture();
        let replayed = replay_capture.is_some();
        if let Some(capture) = replay_capture {
            let report = capture.and_then(|frozen| crate::vita_gpu_replay::run(
                &mut game, &mut renderer, frozen, |_, _| {
                    watchdog::frame(frame + 1);
                    watchdog::phase(watchdog::RENDER);
                }));
            match report {
                Ok(report) => for line in report.json_lines() {
                    let _ = knife_platform_vita::audit::console(&(line + "\n"));
                },
                Err(error) => {
                    let _ = knife_platform_vita::audit::console(&format!("{{\"kind\":\"gpu_replay_error\",\"error\":{}}}\n", quoted(&format!("{error:#}"))));
                },
            }
            // The diagnostic's wall time must not advance the story clock or be
            // classified as a production redraw frame in the next window.
            last = Instant::now();
            if let Some(perf_log) = &mut perf {
                perf_log.pending_class = None;
                perf_log.window = Instant::now();
                perf_log.frames = Default::default();
                perf_log.classes = Default::default();
                perf_log.worst = None;
                perf_log.begin_frame();
            }
        }
        frame += 1;
        watchdog::phase(watchdog::CACHE_MAINTENANCE);
        game.vita_cache_maintenance(&renderer, frame);
        watchdog::phase(watchdog::PERF_LOG);
        if let (false, Some(perf_log), Some(render_started)) = (replayed, &mut perf, render_started) {
            let render_us = render_started.elapsed().as_micros() as u64;
            let work_us = now.elapsed().as_micros() as u64;
            match perf_log.record(frame, work_us, update_us, render_us, wall_us, &game, &renderer, &ui) {
                Ok(true) => {},
                Ok(false) => perf = None,
                Err(error) => {
                    tracing::warn!("Optional Vita console metrics stopped: {error}");
                    perf = None;
                    knife_perf::set_enabled(false);
                    knife_platform_vita::audit::set_enabled(false);
                },
            }
        }
        if frame == 1 { log.event("FRAME 1 submitted"); }
        if perf.is_some() {
            let focus = ui.vita_save_focus();
            if focus != last_save_focus {
                let state = focus.map_or_else(|| "null".to_owned(), |v| format!(concat!(
                    "{{\"state\":{},\"saving\":{},\"page\":{},\"slot\":{},",
                    "\"col\":{},\"row\":{},\"tabrow\":{},\"focus\":{},\"hit\":{},\"frame_texture_ready\":{}}}"
                ), v[0], v[1] != 0, v[2], v[3], v[4], v[5], v[6] != 0, v[7], v[8], v[9] != 0));
                let _ = knife_platform_vita::audit::console(&format!(
                    "{{\"kind\":\"save_focus\",\"frame\":{frame},\"selection\":{state}}}\n"));
                last_save_focus = focus;
            }
        }
        if frame % 120 == 0 { log.event(&format!("FRAME {frame} submitted")); }
        if log.0 {
            let buttons = input.diagnostic_buttons();
            let changed = buttons != last_logged_buttons;
            if changed {
                log.event(&format!("INPUT {{\"frame\":{frame},\"buttons\":{buttons}}}"));
                last_logged_buttons = buttons;
            }
            if frame == 1 || frame % 120 == 0 || changed {
                let (screen, title_state, title_cursor, overlay, menu_state, menu_cursor) = ui.vita_diagnostic_state();
                let (page_messages, page_chars, page_waiting, page_visible, page_revealed, page_order) = game.vita_diagnostic_page();
                let save = ui.vita_diagnostic_save(&game);
                let backlog = ui.vita_diagnostic_backlog(&game);
                // These Debug labels contain only typed enum names/numbers;
                // no game text or external strings enter this JSON record.
                log.event(&format!(concat!(
                    "STATE {{\"frame\":{},\"screen\":\"{}\",\"title_state\":{},",
                    "\"title_cursor\":{},\"overlay\":\"{}\",\"menu_state\":{},\"menu_cursor\":{},\"scene\":{},\"pc\":{},",
                    "\"wait\":\"{:?}\",\"paused\":{},\"ended\":{},\"menu_lock\":{},",
                    "\"page_messages\":{},\"page_chars\":{},\"page_waiting\":{},",
                    "\"page_visible\":{},\"page_revealed\":{},\"page_order\":{},\"buttons\":{},\"save\":{},\"backlog\":{}}}"
                ), frame, screen, title_state, title_cursor, overlay, menu_state, menu_cursor, game.scene_index,
                    game.current_pc(), game.wait, game.paused, game.ended, game.menu_lock,
                    page_messages, page_chars, page_waiting, page_visible, page_revealed, page_order, buttons, save, backlog));
            }
            if matches!(frame, 600 | 720 | 840 | 1440 | 1800 | 2400 | 2640 | 2760 | 3000 | 3600) {
                let observed_at = Instant::now();
                log.event(&game.vita_diagnostic_render(&renderer, frame));
                log.event(&format!("RENDER_CHECKPOINT frame={frame} elapsed_ms={:.3}", observed_at.elapsed().as_secs_f64() * 1000.0));
            }
        }
        if limit.is_some_and(|max| frame >= max) {
            log.event(&format!("FINITE RUN COMPLETE frames={frame}"));
            return Ok(());
        }
        watchdog::phase(watchdog::PACER);
        if let Some(wait) = Duration::from_micros(16_667).checked_sub(last.elapsed()) {
            std::thread::sleep(wait);
        }
    }
}

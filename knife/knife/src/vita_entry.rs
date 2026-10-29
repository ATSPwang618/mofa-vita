//! Windowless native Vita entry using the game's ordinary Game/Ui state machines.
use crate::{assets, game, motion, save, savedata, ui, vita_input::Input};
use anyhow::{Context, Result};
use knife_platform_vita::{asset_check, boot_error, gxm::Gxm};
use knife_render::Renderer;
use std::{
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[cfg(not(feature = "assets-540p"))]
const ASSETS: &str = "ux0:data/knife-woh/assets";
#[cfg(feature = "assets-540p")]
const ASSETS: &str = "ux0:data/knife-woh-540p/assets";

// Performance switch and log live beside the variant's own data root so the 270p
// and 540p packages can coexist without overwriting each other's log.
#[cfg(not(feature = "assets-540p"))]
const PERF_ROOT: &str = "ux0:data/knife-woh";
#[cfg(feature = "assets-540p")]
const PERF_ROOT: &str = "ux0:data/knife-woh-540p";

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

const TITLE_ID: &str = match option_env!("KNIFE_VITA_TITLE_ID") {
    Some(id) => id,
    None => "KNIF00270",
};

// Optional hardware measurement: one file handle, fixed histogram, one buffered
// aggregate every five seconds. Disabled runs do not sample or format per frame.
struct PerfLog {
    file: BufWriter<File>,
    started: Instant,
    window: Instant,
    frames: knife_platform_vita::frame_timing::FrameWindow,
    bytes: usize,
}
impl PerfLog {
    const MAX_BYTES: usize = 256 * 1024;
    fn open(scene_mode: &str) -> std::io::Result<Option<Self>> {
        let Some(flag) = knife_platform_vita::perf_switch::find_enabled(Path::new(PERF_ROOT))?
            else { return Ok(None); };
        let file = File::create(format!("{PERF_ROOT}/performance.log"))?;
        // Read the loader-visible heap/stack sizes: a zero here proves a constant
        // was dropped by --gc-sections, which no build receipt can catch.
        let memory = knife_platform_vita::runtime_memory();
        let header = format!(concat!("{{\"kind\":\"header\",\"schema\":2,\"guest_renderer\":\"direct GXM\",",
            "\"newlib_heap_bytes\":{},\"libc_heap_bytes\":{},\"main_stack_bytes\":{},",
            "\"main_priority\":{},",
            "\"build_tag\":\"{}\",\"arm_mhz\":{},\"bus_mhz\":{},\"gpu_mhz\":{},",
            "\"clock_rc\":[{},{},{},{}],\"scene_divisor\":{},\"scene_mode\":\"{}\",",
            "\"ui_tex_error\":\"{}\",",
            "\"title_id\":\"{}\",\"profile\":\"540p-native-v1\",\"flag\":\"{}\",",
            "\"scene_size\":[960,540],\"composite_size\":[960,540],",
            "\"audio_pcm_pool_bytes\":23040,\"audio_queue_blocks\":4,",
            "\"audio_block_us\":20000,\"audio_mix_scope\":\"mix render including decode\",",
            "\"frame_work_excludes_pacer\":true,\"render_is_cpu_submission_including_waits\":true,",
            "\"memory_scope\":\"GXM mappings and separate JPEG temporary CDRAM; excludes Rust and codec heap\",",
            "\"worker_time_scope\":\"archive read plus selected software/hardware decode\",",
            "\"jpeg_hardware_cdram_reserve\":3145728,",
            "\"image_worker_audio_counters\":\"cumulative u32; wrap possible\",\"interval_ms\":5000,\"max_bytes\":262144}}\n"),
            memory.newlib_heap_bytes, memory.libc_heap_bytes, memory.main_stack_bytes,
            memory.main_priority, BUILD_TAG, CLOCKS.get().copied().unwrap_or((0, 0, 0)).0,
            CLOCKS.get().copied().unwrap_or((0, 0, 0)).1, CLOCKS.get().copied().unwrap_or((0, 0, 0)).2,
            CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).0, CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).1,
            CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).2, CLOCK_RC.get().copied().unwrap_or((0, 0, 0, 0)).3,
            knife_render::scene_divisor(), scene_mode,
            crate::ui::last_atlas_error().unwrap_or_default().replace('"', "'"),
            TITLE_ID, flag.to_string_lossy());
        let mut file = BufWriter::with_capacity(4096, file);
        file.write_all(header.as_bytes())?;
        file.flush()?;
        Ok(Some(Self { file, started: Instant::now(), window: Instant::now(),
            frames: Default::default(), bytes: header.len() }))
    }

    fn record(&mut self, frame: u64, work: u64, update: u64, render: u64, wall: u64,
        game: &game::Game, renderer: &Renderer) -> std::io::Result<bool> {
        self.frames.record(work, update, render, wall);
        if self.window.elapsed() < Duration::from_secs(5) { return Ok(true); }
        let elapsed = self.window.elapsed().as_millis();
        let sample = self.frames.take();
        let (image, worker) = game.assets.image_performance();
        let codec = knife_platform_vita::image::decode_stats();
        let audio = knife_platform_vita::audio_output::metrics::snapshot();
        let gpu = renderer.vita_performance_stats().ok();
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
            "\"audio_mix_max_us\":{},\"audio_mix_late\":{},\"audio_clipped_samples\":{}}}\n"),
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
            audio.clipped_samples);
        if self.bytes + line.len() + 80 > Self::MAX_BYTES {
            self.file.write_all(b"{\"kind\":\"limit\",\"reason\":\"256 KiB log bound; gameplay continues\"}\n")?;
            self.file.flush()?;
            return Ok(false);
        }
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?; // no sync_all and no reopen on the frame path
        self.bytes += line.len();
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
/// appends the location and message to the performance log so one run identifies it.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info.location().map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "unknown".into());
        let message = info.payload().downcast_ref::<&str>().map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        let line = format!("{{\"kind\":\"panic\",\"location\":\"{}\",\"message\":\"{}\"}}\n",
            location.replace('"', "'"), message.replace('"', "'").replace('\n', " "));
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true).append(true).open(format!("{PERF_ROOT}/performance.log"))
        {
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
        eprintln!("{line}");
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
    // Optional density override so one package can A/B the scene chain on hardware.
    // Read before the log header so the header states which mode actually applied.
    let scene_mode_text = std::fs::read_to_string(format!("{PERF_ROOT}/scene-mode.txt")).ok();
    let mode = scene_mode_text.as_deref().map(|text| text.trim().to_ascii_lowercase());
    // Default: scene chain at 480x270 and the composite/UI at 720x405 (upscaled once by
    // the present blit). The composite is the largest per-frame pass (30 ms at 960x540),
    // while the scene chain is already cheap at 480x270.
    let scene_divisor = match mode.as_deref() {
        Some(text) if text.starts_with("quarter") || text == "4" => 4,
        Some(text) if text.starts_with("native") || text == "1" => 1,
        Some(text) if text.starts_with("full") => 1,
        _ => 2,
    };
    let canvas_scale = match mode.as_deref() {
        Some(text) if text.starts_with("full") || text.starts_with("half")
            || text.starts_with("quarter") || text.starts_with("native")
            || text == "1" || text == "2" || text == "4" => (1, 1),
        _ => (3, 8),
    };
    knife_render::set_canvas_scale(canvas_scale.0, canvas_scale.1);
    knife_render::set_scene_divisor(scene_divisor);

    let scene_mode_label = scene_mode_text.as_deref().map(str::trim).filter(|t| !t.is_empty())
        .unwrap_or("none");
    log.event(&format!("SCENE divisor={scene_divisor} canvas={}x file={scene_mode_label:?}",
        if canvas_scale == (1, 1) { "native".to_string() } else { format!("{}/{}", canvas_scale.0, canvas_scale.1) }));
    // Create/flush the opt-in header before archive, font and renderer startup,
    // so even an early failure leaves the selected build/profile visible.
    let mut perf = match PerfLog::open(scene_mode_label) {
        Ok(log) => log,
        Err(error) => { tracing::warn!("Optional Vita performance log disabled: {error}"); None },
    };
    let assets_dir = PathBuf::from(ASSETS);
    let check_started = Instant::now();
    match asset_check::check_assets(&assets_dir) {
        Ok(report) => {
            log.event(&format!("ASSETS 270p key-files={} read-bytes={} check-ms={:.3}",
                report.files, report.read_bytes, check_started.elapsed().as_secs_f64() * 1000.));
            log.event("ASSETS verified Witch/EN+Chengkong");
        },
        Err(error) => {
            // Run before archives/fonts/renderer are loaded. The diagnostic uses
            // embedded ASCII glyphs and one 480x270 image, with no asset fallback.
            log.event(&format!("ASSET_CHECK REJECT {error}"));
            boot_error::show(&error, limit).map_err(anyhow::Error::msg)
                .context("display 270p asset startup error")?;
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
    let mut input = Input::new()?;
    log.event("UI booted; input ready");
    let mut last = Instant::now();
    let mut last_logged_buttons = 0;
    loop {
        let now = Instant::now();
        let wall_us = perf.as_ref().map(|_| (now - last).as_micros() as u64).unwrap_or(0);
        let step = (now - last).as_secs_f32().mul_add(1000.0, 0.0).min(100.0);
        last = now;
        input.poll(&mut game, &mut ui, &renderer)?;
        game.paused = ui.game_paused();
        let (held, advance) = ui.held_input();
        game.set_held(held, advance);
        game.flow_replay = ui.flow_replay();
        if !game.paused {
            ui.rewind_before_script(&mut game);
        }
        ui.story_input(&mut game, &renderer, step);
        game.update(&renderer, step);
        ui.update(&mut game, &renderer, step);
        game.sound.flush();
        game.system_menu_shown = ui.system_menu_shown();
        let update_us = perf.as_ref().map(|_| now.elapsed().as_micros() as u64).unwrap_or(0);
        let render_started = perf.as_ref().map(|_| Instant::now());
        game.render(&mut renderer, &mut |game, renderer, quads| {
            ui.draw(game, renderer, quads)
        });
        frame += 1;
        if let (Some(perf_log), Some(render_started)) = (&mut perf, render_started) {
            let render_us = render_started.elapsed().as_micros() as u64;
            let work_us = now.elapsed().as_micros() as u64;
            match perf_log.record(frame, work_us, update_us, render_us, wall_us, &game, &renderer) {
                Ok(true) => {},
                Ok(false) => perf = None,
                Err(error) => {
                    tracing::warn!("Optional Vita performance log stopped: {error}");
                    perf = None;
                },
            }
        }
        if frame == 1 { log.event("FRAME 1 submitted"); }
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
        if let Some(wait) = Duration::from_micros(16_667).checked_sub(last.elapsed()) {
            std::thread::sleep(wait);
        }
    }
}

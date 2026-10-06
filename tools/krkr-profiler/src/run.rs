use clap::Args;
use glow::HasContext;
use krkr_protocol::{graphics::Size, profile, window};
use serde::Deserialize;
use std::{
    fs::File,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

// The same ES2 loader and GL call counters used by renderer regressions.
#[path = "../../../crates/render-gles2/tests/support/mod.rs"]
#[allow(dead_code)]
mod egl;
#[path = "../../../crates/render-gles2/tests/support/traffic.rs"]
mod traffic;

#[derive(Args)]
pub struct Options {
    #[arg(long)]
    game: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = "startup.tjs")]
    startup: String,
    /// UTF-8 TJS appended after startup, without editing game files.
    #[arg(long)]
    script: Option<PathBuf>,
    /// Timed input sequence in JSON; positions use game coordinates.
    #[arg(long)]
    actions: Option<PathBuf>,
    #[arg(long, default_value_t = 30)]
    seconds: u64,
    /// Maximum presentation rate. Use 0 for an uncapped workload run.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Physical density for script canvases; presentation stays 960x544.
    #[arg(long, default_value = "960x544", value_parser = canvas_size)]
    canvas_size: Size,
    /// Also reduce the final scene, including text and UI.
    #[arg(long)]
    compact_scene: bool,
    /// Disable effect-canvas sharpening for a comparison run.
    #[arg(long)]
    no_effect_sharpen: bool,
    /// Minimum interval for Mahoyo's ActionManager, in milliseconds.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=1000))]
    effect_interval_ms: Option<u32>,
    /// Directory containing libEGL.dll and libGLESv2.dll.
    #[arg(long)]
    gles_dir: Option<PathBuf>,
    /// Advance AT9 timing with silence. Audio decoding cost is excluded.
    #[arg(long)]
    at9_clock: bool,
    /// Leave raw events for a batch analyzer, without exporting a duplicate timeline.
    #[arg(long)]
    no_report: bool,
    #[arg(long, default_value = "warn")]
    log_level: krkr_protocol::diagnostics::Level,
    /// Export the desktop picture to a silent AVC movie (requires FFmpeg).
    #[arg(long)]
    video_output: Option<PathBuf>,
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: PathBuf,
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=60))]
    video_fps: u32,
    #[arg(long, default_value = "libx264")]
    video_encoder: String,
    /// Target AVC bitrate in bits per second, for encoders other than libx264.
    #[arg(long, default_value_t = 6_000_000, value_parser = clap::value_parser!(u32).range(100_000..=50_000_000))]
    video_bitrate: u32,
    /// Render every movie frame at a fixed script-clock tick, regardless of host speed.
    #[arg(long)]
    video_offline: bool,
    /// Keep source canvases at native resolution during offline capture.
    #[arg(long, requires = "video_offline")]
    native_canvas_storage: bool,
    /// Begin offline capture when this global integer becomes nonzero.
    #[arg(long, requires = "video_offline")]
    video_ready_global: Option<String>,
    /// Exact offline movie duration; defaults to --seconds.
    #[arg(long, requires = "video_offline")]
    video_duration_ms: Option<u64>,
    /// Save a settled screenshot whenever this global positive integer changes.
    #[arg(long, requires = "video_offline")]
    capture_global: Option<String>,
    /// Finish after this many screenshots from --capture-global.
    #[arg(long, requires = "capture_global")]
    capture_count: Option<u32>,
    /// Multiply desktop recording budgets; 1 retains the Vita defaults.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=16))]
    memory_scale: u32,
}

impl Options {
    fn effect_sharpen(&self) -> bool {
        !self.native_canvas_storage
            && !self.no_effect_sharpen
            && (self.canvas_size.width < 960 || self.canvas_size.height < 544)
    }
}

fn canvas_size(value: &str) -> Result<Size, String> {
    let (width, height) = value.split_once('x').ok_or("use WIDTHxHEIGHT")?;
    let size = Size {
        width: width.parse().map_err(|_| "invalid canvas width")?,
        height: height.parse().map_err(|_| "invalid canvas height")?,
    };
    if size.width == 0 || size.height == 0 || size.width > 960 || size.height > 544 {
        return Err("canvas size must fit within 960x544 and be nonzero".into());
    }
    Ok(size)
}
#[derive(Deserialize)]
struct Action {
    at_ms: u64,
    #[serde(flatten)]
    input: Input,
}
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Input {
    Click { x: i32, y: i32 },
    Move { x: i32, y: i32 },
    KeyDown { key: u32 },
    KeyUp { key: u32 },
    Mark { label: String },
    Screenshot,
}

pub fn run(options: Options) -> Result<(), String> {
    if options.seconds == 0 {
        return Err("--seconds must be positive".into());
    }
    if let Some(directory) = &options.gles_dir {
        let directory = directory.canonicalize().map_err(|e| e.to_string())?;
        for name in ["libEGL.dll", "libGLESv2.dll"] {
            if !directory.join(name).is_file() {
                return Err(format!("missing {}", directory.join(name).display()));
            }
        }
        let paths = std::iter::once(directory)
            .chain(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ))
            .collect::<Vec<_>>();
        // This runs before starting the VM, audio, recorder or EGL threads.
        unsafe {
            std::env::set_var(
                "PATH",
                std::env::join_paths(paths).map_err(|e| e.to_string())?,
            );
        }
    }
    let root = options.game.canonicalize().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&options.out).map_err(|e| e.to_string())?;
    let out = options.out.canonicalize().map_err(|e| e.to_string())?;
    let data = out.join("savedata");
    let script = options
        .script
        .as_ref()
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|e| e.to_string())?;
    let mut actions: Vec<Action> = options
        .actions
        .as_ref()
        .map(|p| {
            serde_json::from_reader(File::open(p).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())
        })
        .transpose()?
        .unwrap_or_default();
    actions.sort_by_key(|a| a.at_ms);
    let recording = profile::FileCapture::start(&out.join("events.jsonl"), 65536)?;
    krkr_protocol::diagnostics::set_level(options.log_level);
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        capture(&options, &root, &out, &data, script.as_deref(), actions)
    }))
    .unwrap_or_else(|error| {
        Err(error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "capture panicked".into()))
    });
    let dropped = recording.finish()?;
    let metadata = serde_json::json!({
        "schema": 1, "game": root, "startup": options.startup,
        "version": env!("CARGO_PKG_VERSION"),
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
        "host": std::env::consts::OS, "renderer": "Vita host / desktop GLES2",
        "display": [960,544],
        "canvas_size": [options.canvas_size.width, options.canvas_size.height],
        "native_canvas_storage": options.native_canvas_storage,
        "effect_interval_ms": options.effect_interval_ms,
        "compact_scene": options.compact_scene,
        "effect_sharpen": options.effect_sharpen(),
        "frame_limit_fps": options.fps,
        "memory_scale": options.memory_scale,
        "graphics_budget_bytes": krkr_host_vita::memory::GRAPHICS_BYTES * options.memory_scale as usize,
        "scratch_budget_bytes": krkr_host_vita::memory::SCRATCH_BYTES * options.memory_scale as usize,
        "vita_newlib_heap_bytes": krkr_host_vita::memory::NEWLIB_HEAP_BYTES,
        "audio": if options.at9_clock { "AT9 clock substitution" } else { "portable decoder / silent output" },
        "script": options.script, "actions": options.actions, "elapsed_ms": started.elapsed().as_secs_f64()*1000.,
        "video_output": options.video_output, "video_fps": options.video_fps,
        "video_bitrate": options.video_bitrate,
        "video_offline": options.video_offline,
        "video_ready_global": options.video_ready_global,
        "video_duration_ms": options.video_duration_ms,
        "capture_global": options.capture_global, "capture_count": options.capture_count,
        "dropped_events": dropped, "error": result.as_ref().err(),
    });
    std::fs::write(
        out.join("run.json"),
        serde_json::to_vec_pretty(&metadata).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    if !options.no_report {
        super::report::generate(&out, 0., None)?;
    }
    if dropped != 0 {
        eprintln!("recording lost {dropped} events; memory peaks and timings may be incomplete");
    }
    result
}

#[derive(Clone, Default)]
struct RecordingClock(Arc<AtomicU64>);
impl tjs_runtime::clock::Clock for RecordingClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::Acquire))
    }
}

fn capture(
    options: &Options,
    root: &std::path::Path,
    out: &std::path::Path,
    data: &std::path::Path,
    script: Option<&str>,
    actions: Vec<Action>,
) -> Result<(), String> {
    let context = egl::Context::sized(960, 544);
    let inspect = context.gl();
    profile::marker("renderer", || unsafe {
        inspect.get_parameter_string(glow::RENDERER)
    });
    let wake = krkr_host_vita::wake::Wake::new()?;
    let notifier = wake.clone();
    let scale = options.memory_scale as usize;
    let mut limits = window::Limits::default();
    limits.staging_bytes *= scale;
    let (client, host) = window::channel(limits, Arc::new(move || notifier.signal()));
    let mut config = krkr_host_vita::memory::graphics_config(host.staging_budget());
    if scale != 1 {
        let graphics =
            krkr_protocol::budget::Budget::new(krkr_host_vita::memory::GRAPHICS_BYTES * scale);
        graphics.set_profile_name("memory.graphics_bytes");
        config.resident = graphics.child(krkr_host_vita::memory::RESIDENT_BYTES * scale);
        config.scratch = graphics.child(krkr_host_vita::memory::SCRATCH_BYTES * scale);
    }
    config.work_framebuffer = true;
    config.canvas_limit = (!options.native_canvas_storage).then_some(options.canvas_size);
    config.compact_scene = options.compact_scene;
    config.effect_sharpen = options.effect_sharpen();
    config.small_canvas_edge = 64;
    let gpu = unsafe { krkr_render_gles2::Gpu::new(context.gl_with(traffic::intercept), config) }
        .map_err(|e| e.to_string())?;
    traffic::reset();
    let mut windows = krkr_host_vita::window::Windows::new(host, gpu);
    let stopped = Arc::new(AtomicBool::new(false));
    let audio = super::audio::Output::new(stopped.clone());
    std::thread::scope(|scope| {
        let mut movie = options
            .video_output
            .as_ref()
            .map(|path| {
                super::movie::Export::new(
                    path,
                    &options.ffmpeg,
                    options.video_fps,
                    &options.video_encoder,
                    options.video_bitrate,
                )
            })
            .transpose()?;
        let cancel = stopped.clone();
        let output = audio.clone();
        let clock = RecordingClock::default();
        let vm_clock = clock.clone();
        let (idle_send, idle_receive) = std::sync::mpsc::channel();
        let (step_send, step_receive) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("krkr-vm".into())
            .stack_size(4 * 1024 * 1024)
            .spawn_scoped(scope, move || {
                if options.video_offline {
                    let setup = move |engine: &mut krkr_engine::Engine<RecordingClock>| {
                        engine
                            .set_bitmap_memory_limit(krkr_host_vita::memory::BITMAP_BYTES * scale)
                            .map_err(|e| e.to_string())?;
                        engine.set_audio_output(output).map_err(|e| e.to_string())?;
                        if options.at9_clock {
                            engine.set_audio_decoder_backend(super::audio::At9Clock);
                        }
                        engine.set_video_backend(krkr_video_ffmpeg::Ffmpeg);
                        Ok(())
                    };
                    krkr_host_vita::bootstrap::run_with_clock(
                        root,
                        krkr_host_vita::bootstrap::Options {
                            startup: &options.startup,
                            debug: false,
                            data: Some(data),
                            after_startup: script,
                            effect_interval_ms: options.effect_interval_ms,
                        },
                        Some(client),
                        &cancel,
                        vm_clock,
                        setup,
                        |engine| {
                            let global_integer = |name: &str| {
                                let heap = &engine.runtime().heap;
                                heap.object(engine.global())
                                    .ok()
                                    .and_then(|object| {
                                        object.members().find_map(|(symbol, value)| {
                                            heap.symbol(symbol)
                                                .is_ok_and(|units| {
                                                    units.iter().copied().eq(name.encode_utf16())
                                                })
                                                .then_some(value)
                                                .and_then(|value| {
                                                    if let tjs_core::Value::Int(value) = value {
                                                        Some(value)
                                                    } else {
                                                        None
                                                    }
                                                })
                                        })
                                    })
                                    .unwrap_or(0)
                            };
                            let ready = options
                                .video_ready_global
                                .as_ref()
                                .is_none_or(|name| global_integer(name) != 0);
                            let capture = options
                                .capture_global
                                .as_ref()
                                .map_or(0, |name| global_integer(name));
                            if idle_send.send((ready, capture)).is_err() {
                                return Ok(true);
                            }
                            while !cancel.load(Ordering::Acquire) {
                                match step_receive.recv_timeout(Duration::from_millis(50)) {
                                    Ok(()) => break,
                                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                                    Err(_) => break,
                                }
                            }
                            Ok(true)
                        },
                    )
                } else {
                    krkr_host_vita::bootstrap::run_with_options(
                        root,
                        krkr_host_vita::bootstrap::Options {
                            startup: &options.startup,
                            debug: false,
                            data: Some(data),
                            after_startup: script,
                            effect_interval_ms: options.effect_interval_ms,
                        },
                        Some(client),
                        &cancel,
                        |engine| {
                            engine
                                .set_bitmap_memory_limit(
                                    krkr_host_vita::memory::BITMAP_BYTES * scale,
                                )
                                .map_err(|e| e.to_string())?;
                            engine.set_audio_output(output).map_err(|e| e.to_string())?;
                            if options.at9_clock {
                                engine.set_audio_decoder_backend(super::audio::At9Clock);
                            }
                            engine.set_video_backend(krkr_video_ffmpeg::Ffmpeg);
                            Ok(())
                        },
                    )
                }
            })
            .map_err(|e| e.to_string())?;
        let started = Instant::now();
        if movie.is_some() {
            profile::marker("capture.video-start", || "frame zero".into());
        }
        let mut action = 0;
        let mut sampled = Instant::now();
        let mut maintenance = Instant::now();
        let mut previous_frame = [0; 5];
        let frame_period =
            (options.fps != 0).then(|| Duration::from_secs_f64(1.0 / f64::from(options.fps)));
        let mut next_present = started;
        let mut offline_frame = 0u64;
        let mut movie_started = None;
        let mut offline_elapsed = Duration::ZERO;
        let mut capture_index = 0i64;
        let mut capture_count = 0u32;
        let movie_duration =
            Duration::from_millis(options.video_duration_ms.unwrap_or(options.seconds * 1000));
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            while !worker.is_finished()
                && (options.video_offline
                    || started.elapsed() < Duration::from_secs(options.seconds))
            {
                let offline_ready = if options.video_offline {
                    idle_receive.try_recv().ok()
                } else {
                    None
                };
                let mut advance_offline = false;
                {
                    let _span = profile::span("host.pump");
                    windows.pump()?;
                }
                let rendered = windows.render()?;
                if rendered {
                    // A pbuffer swap does not wait for vblank. Match a bounded
                    // display rate so faster draws do not inflate animation
                    // frame counts and obscure traffic comparisons.
                    if let Some(period) = frame_period.filter(|_| !options.video_offline) {
                        let remaining = next_present.saturating_duration_since(Instant::now());
                        if !remaining.is_zero() {
                            let _span = profile::span("host.frame_wait");
                            std::thread::sleep(remaining);
                        }
                        next_present = Instant::now() + period;
                    }
                    let _span = profile::span("host.present");
                    context.swap();
                    for (previous, (name, value)) in previous_frame.iter_mut().zip([
                        ("frame.draws", traffic::draw_calls()),
                        ("frame.copy_bytes", traffic::stored_pixels() * 4),
                        ("frame.finishes", traffic::finish_calls()),
                        ("frame.allocations", traffic::texture_allocations()),
                        ("frame.readbacks", traffic::read_calls()),
                    ]) {
                        profile::counter(name, value.saturating_sub(*previous) as u64);
                        *previous = value;
                    }
                }
                if rendered || maintenance.elapsed() >= Duration::from_millis(50) {
                    let _span = profile::span("host.maintain");
                    windows.graphics.maintain()?;
                    maintenance = Instant::now();
                }
                if options.video_offline {
                    if let Some((ready, capture)) = offline_ready {
                        let now = Duration::from_nanos(clock.0.load(Ordering::Acquire));
                        if capture > 0 && capture != capture_index {
                            screenshot(&inspect, &out.join(format!("capture-{capture:04}.png")))?;
                            capture_index = capture;
                            capture_count += 1;
                            if options
                                .capture_count
                                .is_some_and(|count| capture_count >= count)
                            {
                                break;
                            }
                        }
                        if ready {
                            let first = *movie_started.get_or_insert(now);
                            offline_elapsed = now.saturating_sub(first);
                            if offline_elapsed >= movie_duration {
                                break;
                            }
                            if let Some(movie) = &mut movie {
                                movie.sample_next(&inspect)?;
                            }
                        } else if now >= Duration::from_secs(120) {
                            return Err(
                                "offline capture did not become ready within 120 script seconds"
                                    .into(),
                            );
                        }
                        offline_frame += 1;
                        clock.0.store(
                            (u128::from(offline_frame) * 1_000_000_000
                                / u128::from(options.video_fps)) as u64,
                            Ordering::Release,
                        );
                        advance_offline = true;
                    }
                } else if let Some(movie) = &mut movie {
                    movie.sample(&inspect, started.elapsed())?;
                }
                while action < actions.len()
                    && (!options.video_offline || movie_started.is_some())
                    && actions[action].at_ms
                        <= if options.video_offline {
                            offline_elapsed.as_millis() as u64
                        } else {
                            started.elapsed().as_millis() as u64
                        }
                {
                    let entry = &actions[action];
                    profile::marker("input", || {
                        format!("index={action} scheduled_ms={}", entry.at_ms)
                    });
                    match &entry.input {
                        Input::Mark { label } => profile::marker("bookmark", || label.clone()),
                        Input::Screenshot => {
                            screenshot(&inspect, &out.join(format!("frame-{action:04}.png")))?
                        }
                        input => {
                            let id = windows
                                .focused()
                                .ok_or("timed input has no focused game window")?;
                            let events = match *input {
                                Input::Click { x, y } => vec![
                                    window::Input::MouseMove { x, y, shift: 0 },
                                    window::Input::MouseDown {
                                        x,
                                        y,
                                        button: 0,
                                        shift: 0,
                                    },
                                    window::Input::MouseUp {
                                        x,
                                        y,
                                        button: 0,
                                        shift: 0,
                                    },
                                    window::Input::Click { x, y },
                                ],
                                Input::Move { x, y } => {
                                    vec![window::Input::MouseMove { x, y, shift: 0 }]
                                }
                                Input::KeyDown { key } => {
                                    vec![window::Input::KeyDown { key, shift: 0 }]
                                }
                                Input::KeyUp { key } => {
                                    vec![window::Input::KeyUp { key, shift: 0 }]
                                }
                                _ => unreachable!(),
                            };
                            for event in events {
                                windows.post(id, event)?;
                            }
                        }
                    }
                    action += 1;
                }
                // Deliver scheduled inputs while the VM remains parked at
                // this boundary, so host speed cannot reorder the next tick.
                if advance_offline {
                    step_send.send(()).map_err(|_| "offline VM stopped")?;
                }
                if sampled.elapsed() >= Duration::from_millis(100) {
                    sample_counters();
                    sampled = Instant::now();
                }
                if !rendered {
                    let _span = profile::span("host.idle");
                    wake.wait(Duration::from_millis(8))?;
                }
            }
            sample_counters();
            screenshot(&inspect, &out.join("last-frame.png"))
        }));
        stopped.store(true, Ordering::Release);
        windows
            .host
            .disconnect("Performance recording finished".into());
        worker.thread().unpark();
        let vm = worker.join().map_err(|_| "VM worker panicked");
        let audio = audio.join();
        if let Some(movie) = movie {
            movie.finish()?;
        }
        match result {
            Ok(result) => result?,
            Err(error) => std::panic::resume_unwind(error),
        }
        audio?;
        let vm = vm?;
        match vm {
            Ok(0) => Ok(()),
            Ok(code) => Err(format!("game exited with code {code}")),
            Err(error) if error.contains("Performance recording finished") => Ok(()),
            Err(error) => Err(error),
        }
    })
}
fn sample_counters() {
    crate::heap::sample();
    for (name, value) in [
        ("gl.draws", traffic::draw_calls()),
        ("gl.copy_bytes", traffic::stored_pixels() * 4),
        ("gl.load_bytes", traffic::loaded_pixels() * 4),
        ("gl.readbacks", traffic::read_calls()),
        ("gl.texture_allocations", traffic::texture_allocations()),
        ("gl.finishes", traffic::finish_calls()),
        ("gl.viewport_calls", traffic::viewport_calls()),
        ("gl.viewport_changes", traffic::viewport_changes()),
    ] {
        profile::counter(name, value as u64);
    }
    if let Some(usage) = memory_stats::memory_stats() {
        profile::counter("process.rss_bytes", usage.physical_mem as u64);
    }
}
fn screenshot(gl: &glow::Context, path: &std::path::Path) -> Result<(), String> {
    let _span = profile::span("capture.screenshot");
    let mut pixels = vec![0; 960 * 544 * 4];
    unsafe {
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.read_pixels(
            0,
            0,
            960,
            544,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
        let error = gl.get_error();
        if error != glow::NO_ERROR {
            return Err(format!("screenshot GLES error {error:#x}"));
        }
    }
    let image = image::RgbaImage::from_raw(960, 544, pixels).unwrap();
    image::imageops::flip_vertical(&image)
        .save(path)
        .map_err(|e| e.to_string())
}

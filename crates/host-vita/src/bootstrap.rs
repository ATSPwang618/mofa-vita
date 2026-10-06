//! Script boot path on the VM worker. Window commands wake the EGL thread.
use krkr_engine::{
    EngineEvent,
    assets::{Vfs, local, name},
    debug::LogOutput,
    system::SystemConfig,
};
use std::{collections::BTreeMap, fmt::Write, num::NonZeroUsize, path::Path, time::Duration};
use tjs_core::{RunBudget, SourceMap};
use tjs_runtime::{Runtime, RuntimeExit};

pub fn run(root: &Path) -> Result<i32, String> {
    run_with_windows(root, None, &std::sync::atomic::AtomicBool::new(false))
}
pub fn run_with_windows(
    root: &Path,
    windows: Option<krkr_protocol::window::Client>,
    stopped: &std::sync::atomic::AtomicBool,
) -> Result<i32, String> {
    run_configured(root, "startup.tjs", false, windows, stopped)
}
pub fn run_configured(
    root: &Path,
    startup: &str,
    debug: bool,
    windows: Option<krkr_protocol::window::Client>,
    stopped: &std::sync::atomic::AtomicBool,
) -> Result<i32, String> {
    run_with_options(
        root,
        Options {
            startup,
            debug,
            data: None,
            after_startup: None,
            effect_interval_ms: None,
        },
        windows,
        stopped,
        |_| Ok(()),
    )
}

pub struct Options<'a> {
    pub startup: &'a str,
    pub debug: bool,
    pub data: Option<&'a Path>,
    pub after_startup: Option<&'a str>,
    /// Update interval for the game's ActionManager; timers and text are unchanged.
    pub effect_interval_ms: Option<u32>,
}

pub fn run_with_options(
    root: &Path,
    options: Options<'_>,
    windows: Option<krkr_protocol::window::Client>,
    stopped: &std::sync::atomic::AtomicBool,
    setup: impl FnOnce(
        &mut krkr_engine::Engine<tjs_runtime::clock::MonotonicClock>,
    ) -> Result<(), String>,
) -> Result<i32, String> {
    run_with_clock(
        root,
        options,
        windows,
        stopped,
        tjs_runtime::clock::MonotonicClock::default(),
        setup,
        |_| Ok(false),
    )
}

/// Run with a caller-owned script clock, for deterministic offline capture.
/// The idle hook runs after script work, IO completion and scene publication.
/// It can wait for presentation, advance the clock and return true to poll
/// again immediately; false retains the normal host wait policy.
pub fn run_with_clock<C: tjs_runtime::clock::Clock + 'static>(
    root: &Path,
    options: Options<'_>,
    windows: Option<krkr_protocol::window::Client>,
    stopped: &std::sync::atomic::AtomicBool,
    clock: C,
    setup: impl FnOnce(&mut krkr_engine::Engine<C>) -> Result<(), String>,
    mut idle: impl FnMut(&mut krkr_engine::Engine<C>) -> Result<bool, String>,
) -> Result<i32, String> {
    use crate::watchdog::{Stage, Watchdog, scope};
    let tracing = krkr_protocol::diagnostics::enabled();
    let _watchdog = Watchdog::start_named(tracing, "krkr-vm")?;
    let boot = scope(Stage::VmBoot);
    let Options {
        startup,
        debug,
        data,
        after_startup,
        effect_interval_ms,
    } = options;
    #[cfg(target_os = "vita")]
    crate::files::install();
    if !crate::launcher::valid_startup(startup) {
        return Err("invalid startup file name".into());
    }
    let data = data
        .map(Path::to_path_buf)
        .unwrap_or_else(|| root.join("savedata"));
    std::fs::create_dir_all(&data).map_err(|e| e.to_string())?;
    let directory = local::directory(root).map_err(|e| e.to_string())?;
    let mut logs = crate::logging::Logs::new(debug);
    if debug {
        logs.console(&name::units(&format!("Starting {startup}")));
    }
    let requested = name::units(startup);
    let mut vfs = Vfs::for_project(root, &requested, crate::memory::storage_limits())
        .map_err(|e| e.to_string())?;
    let requested = if name::split_archive(&requested).1.is_some() {
        name::normalize(&requested, &directory).map_err(|e| e.to_string())?
    } else {
        requested
    };
    let entry = vfs.plan(&requested).map_err(|e| e.to_string())?.name;
    let mut bootstrap = String::new();
    let patch = root.join("patch.tjs");
    match std::fs::metadata(&patch) {
        Ok(meta) if meta.is_file() => {
            // A root compatibility file must not be shadowed by an XP3 member.
            let patch = local::absolute(&patch).map_err(|e| e.to_string())?;
            let patch = local::units(&patch).map_err(|e| e.to_string())?;
            append_storage(&mut bootstrap, &patch);
            if debug {
                logs.console(&name::units(&format!(
                    "Compatibility patch: {}",
                    String::from_utf16_lossy(&patch)
                )));
            }
        }
        Ok(_) => return Err(format!("script is not a file: {}", patch.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("{}: {error}", patch.display())),
    }
    if let Some(interval) = effect_interval_ms {
        // Register before startup: Mahoyo creates its KAG window asynchronously.
        // Its ActionManager evaluates keys against the current wall-clock tick.
        writeln!(
            bootstrap,
            "Scripts.afterLoad('actionmanager.tjs', function(storage) {{\n\
             global.__krkrEffectTuner = new Timer(function(e) {{\n\
             if(typeof global.kag == 'undefined' || typeof kag.actmgr == 'undefined') return;\n\
             kag.actmgr.interval = Math.max(kag.actmgr.interval, {interval});\n\
             e.target.enabled = false; invalidate e.target; delete global.__krkrEffectTuner;\n\
             }}, '');\n\
             __krkrEffectTuner.interval = 100; __krkrEffectTuner.capacity = 1; __krkrEffectTuner.enabled = true;\n\
             }});"
        )
        .unwrap();
    }
    append_storage(&mut bootstrap, &entry);
    if let Some(script) = after_startup {
        bootstrap.push_str(script);
    }
    let mut sources = SourceMap::new();
    let source = sources
        .add_utf8("<Vita startup>", &bootstrap)
        .map_err(|e| e.to_string())?;
    let mut preprocessor = tjs_front::Preprocessor::default();
    krkr_engine::configure_preprocessor(&mut preprocessor);
    preprocessor.set_default("krkr_vita", 1);
    let module = tjs_front::compile_with_preprocessor(&sources, source, &mut preprocessor)
        .map_err(|e| e.to_string())?;
    let mut runtime = Runtime::with_sources(sources, preprocessor);
    krkr_engine::storages::install(&mut runtime.heap, vfs).map_err(|e| e.to_string())?;
    krkr_engine::install(&mut runtime, logs).map_err(|e| e.to_string())?;
    krkr_plugins::register(&mut runtime.heap).map_err(|e| e.to_string())?;
    let config = SystemConfig {
        title: name::units("KRKR"),
        exe_name: name::units("app0:/eboot.bin"),
        exe_path: directory.clone(),
        data_path: local::directory(&data).map_err(|e| e.to_string())?,
        personal_path: directory.clone(),
        app_data_path: directory,
        saved_games_path: local::units(&data).map_err(|e| e.to_string())?,
        host: None,
        arguments: BTreeMap::new(),
        continuous_interval: Duration::from_millis(16),
    };
    let mut engine = crate::create_engine_with_clock(runtime, config, clock)?;
    setup(&mut engine)?;
    if let Some(client) = &windows {
        engine
            .attach_windows(client.clone())
            .map_err(|e| e.to_string())?;
    }
    engine
        .start(&module)
        .map_err(|_| "cannot start Vita script")?;
    let sleeper = crate::wake::Wake::new()?;
    let notifier = sleeper.clone();
    engine
        .set_waker(std::sync::Arc::new(move || notifier.signal()))
        .map_err(|e| e.to_string())?;
    let mut profile_sample = krkr_protocol::profile::active().then(std::time::Instant::now);
    let mut profile_ticks = 0u8;
    drop(boot);
    loop {
        if stopped.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(0);
        }
        if let Some(error) = engine.window_host_error() {
            return Err(error);
        }
        if let Some(error) = engine.audio_host_error() {
            return Err(error);
        }
        let poll_timer = krkr_protocol::diagnostics::Timer::start();
        let work_before = engine.work_executed();
        let poll = scope(Stage::VmPoll);
        let event = engine.poll(
            RunBudget::new(10_000).unwrap(),
            NonZeroUsize::new(64).unwrap(),
        );
        let idle_boundary = matches!(&event, EngineEvent::Idle)
            || matches!(&event, EngineEvent::Waiting { request, .. }
                if request.mode == tjs_core::WaitMode::Event);
        drop(poll);
        poll_timer.report(|| {
            format!(
                "stage=vm-poll work={}",
                engine.work_executed() - work_before
            )
        });
        match event {
            EngineEvent::Terminated(code) => return Ok(code),
            EngineEvent::Completed { context, result }
            | EngineEvent::Timer {
                context, result, ..
            }
            | EngineEvent::AsyncTrigger {
                context, result, ..
            }
            | EngineEvent::System {
                context, result, ..
            }
            | EngineEvent::Sound {
                context, result, ..
            }
            | EngineEvent::Video {
                context, result, ..
            }
            | EngineEvent::Window {
                context, result, ..
            } => {
                match result {
                    RuntimeExit::Fault(error) => {
                        return Err(format_error(&engine.runtime().sources, &error));
                    }
                    RuntimeExit::Thrown(error) => {
                        return Err(format_error(&engine.runtime().sources, &error.diagnostic));
                    }
                    _ => {}
                }
                engine.take_result(context);
            }
            EngineEvent::Waiting { request, .. } if !engine.owns_wait(request) => {
                return Err("native wait requires a Vita host service".into());
            }
            _ => {}
        }
        let gc_timer = krkr_protocol::diagnostics::Timer::start();
        let collection = scope(Stage::VmCollect);
        engine.collect_if_needed();
        drop(collection);
        gc_timer.report(|| "stage=gc".into());
        if let Some(last) = &mut profile_sample
            && last.elapsed() >= Duration::from_millis(100)
        {
            let heap = &engine.runtime().heap;
            let counts = heap.counts();
            for (name, value) in [
                ("vm.objects", counts.objects),
                ("vm.strings", counts.strings),
                ("vm.octets", counts.octets),
                ("vm.symbols", counts.symbols),
                ("vm.gc_debt_bytes", heap.allocation_debt()),
            ] {
                krkr_protocol::profile::counter(name, value as u64);
            }
            krkr_protocol::profile::counter("vm.work", engine.work_executed());
            profile_ticks += 1;
            if profile_ticks == 10 {
                profile_ticks = 0;
                krkr_protocol::profile::marker("vm.stack", || {
                    engine
                        .active_diagnostic()
                        .map_or_else(String::new, |sample| {
                            format_error(&engine.runtime().sources, &sample)
                        })
                });
            }
            *last = std::time::Instant::now();
        }
        if idle_boundary
            && engine.pending_host_operations() == 0
            && engine.sleep_duration() != Some(Duration::ZERO)
            && idle(&mut engine)?
        {
            continue;
        }
        let _wait = scope(Stage::VmWait);
        match engine.sleep_duration() {
            Some(delay) if !delay.is_zero() => {
                sleeper.wait(delay.min(Duration::from_millis(50)))?
            }
            Some(_) => {}
            None if engine.pending_operations() != 0 || engine.application_running() => {
                sleeper.wait(Duration::from_millis(50))?
            }
            None => return Ok(0),
        }
    }
}

fn format_error(sources: &SourceMap, error: &tjs_core::Diagnostic) -> String {
    let mut text = error.to_string();
    let location = |span: tjs_core::Span| {
        let source = sources.get(span.source())?;
        let (line, column) = source.line_column(span.start())?;
        Some(format!("{}:{line}:{column}", source.name()))
    };
    if let Some(at) = error.span.and_then(location) {
        write!(text, "\n  at {at}").unwrap();
    }
    for frame in &error.trace {
        if let Some(at) = frame.span.and_then(location) {
            write!(text, "\n  in {} ({at})", frame.function).unwrap();
        }
    }
    text
}

fn append_storage(bootstrap: &mut String, path: &[u16]) {
    bootstrap.push_str("Scripts.execStorage(\"");
    for unit in path {
        write!(bootstrap, "\\x{unit:04x}").unwrap();
    }
    bootstrap.push_str("\");\n");
}

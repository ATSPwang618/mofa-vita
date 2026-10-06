#[cfg(all(debug_assertions, target_os = "vita"))]
compile_error!("krkr-vita must be built with --release");

#[cfg(target_os = "vita")]
#[allow(unsafe_code)]
mod native {
    use krkr_host_vita::watchdog::{Stage, Watchdog, scope};
    #[used]
    #[unsafe(export_name = "sceUserMainThreadStackSize")]
    static MAIN_STACK: u32 = 2 * 1024 * 1024;
    #[used]
    #[unsafe(export_name = "sceLibcHeapSize")]
    static LIBC_HEAP: u32 = 16 * 1024 * 1024;
    #[used]
    #[unsafe(export_name = "_newlib_heap_size_user")]
    static NEWLIB_HEAP: u32 = krkr_host_vita::memory::NEWLIB_HEAP_BYTES as u32;

    pub fn run() -> Result<i32, String> {
        std::panic::set_hook(Box::new(|info| eprintln!("[VITA][PANIC] {info}")));
        krkr_host_vita::files::install();
        let _clocks = krkr_host_vita::power::Clocks::acquire()?;
        // EGL stays on the main thread; VM, archive IO and audio have their own
        // workers. Native input and protocol drawing share one ordered UI loop.
        let graphics = krkr_host_vita::pvr::PvrContext::new()?;
        let mut controller = krkr_host_vita::input::native::Controller::new()?;
        let mut message = String::new();
        loop {
            let Some(selection) =
                krkr_host_vita::launcher::choose(&graphics, &mut controller, &message)?
            else {
                return Ok(0);
            };
            krkr_protocol::diagnostics::set_enabled(selection.engine_logs);
            let watchdog = Watchdog::start(selection.engine_logs)?;
            let root = selection.directory;
            krkr_protocol::diagnostic!(
                "[VITA][BOOT] game={} startup={} script_logs={} engine_logs={} stats={}",
                root.display(),
                selection.startup,
                selection.script_logs,
                selection.engine_logs,
                selection.show_stats
            );
            // Declared after EGL: all renderer resources are dropped while its
            // context is still current, including startup and worker failures.
            let sleeper = krkr_host_vita::wake::Wake::new()?;
            let notifier = sleeper.clone();
            let wake = std::sync::Arc::new(move || notifier.signal());
            let (client, host) = krkr_protocol::window::channel(Default::default(), wake.clone());
            let mut config = krkr_host_vita::memory::graphics_config(host.staging_budget());
            config.canvas_limit = Some(selection.render_quality.canvas_size());
            config.effect_sharpen = selection.render_quality.effect_interval_ms().is_some();
            let renderer = unsafe { krkr_render_gles2::Gpu::new(graphics.glow(), config) }
                .map_err(|error| error.to_string())?;
            let mut windows = krkr_host_vita::window::Windows::new(host, renderer);
            windows.set_show_stats(selection.show_stats);
            let outcome: Result<i32, String> = (|| {
                let mut input = krkr_host_vita::input::State::new(std::time::Instant::now());
                input.set_pointer_speed(selection.cursor_speed);
                input.configure(&root, selection.language);
                let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let cancel = stopped.clone();
                let worker = std::thread::Builder::new()
                    .name("krkr-vm".into())
                    .stack_size(krkr_host_vita::memory::VM_STACK_BYTES)
                    .spawn(move || {
                        let result = krkr_host_vita::bootstrap::run_with_options(
                            &root,
                            krkr_host_vita::bootstrap::Options {
                                startup: &selection.startup,
                                debug: selection.script_logs,
                                data: None,
                                after_startup: None,
                                effect_interval_ms: selection.render_quality.effect_interval_ms(),
                            },
                            Some(client),
                            &cancel,
                            |_| Ok(()),
                        );
                        krkr_protocol::diagnostic!("[VITA][VM] worker END {result:?}");
                        wake();
                        result
                    })
                    .map_err(|e| e.to_string())?;
                let result: Result<(), String> = (|| {
                    let mut maintenance = std::time::Instant::now();
                    let mut exit_since = None;
                    while !worker.is_finished() {
                        let pump = scope(Stage::Pump);
                        windows.pump()?;
                        drop(pump);
                        let input_stage = scope(Stage::Input);
                        let sample = controller.sample()?;
                        let now = std::time::Instant::now();
                        if sample.buttons & 0x9 == 0x9 {
                            if now.duration_since(*exit_since.get_or_insert(now))
                                >= std::time::Duration::from_secs(1)
                            {
                                break;
                            }
                        } else {
                            exit_since = None;
                        }
                        input.poll(sample, now, &mut windows)?;
                        drop(input_stage);
                        if windows.render()? {
                            let swap = scope(Stage::Swap);
                            graphics.swap()?;
                            drop(swap);
                            windows.graphics.maintain()?;
                            maintenance = std::time::Instant::now();
                        } else {
                            if maintenance.elapsed() >= std::time::Duration::from_millis(50) {
                                windows.graphics.maintain()?;
                                maintenance = std::time::Instant::now();
                            }
                            let _wait = scope(Stage::Wait);
                            sleeper.wait(std::time::Duration::from_millis(8))?;
                        }
                    }
                    Ok(())
                })();
                stopped.store(true, std::sync::atomic::Ordering::Release);
                let interrupted = !worker.is_finished();
                if interrupted || result.is_err() {
                    windows.host.disconnect(
                        result
                            .as_ref()
                            .err()
                            .cloned()
                            .unwrap_or_else(|| "Game closed from controller".into()),
                    );
                }
                worker.thread().unpark();
                let joining = scope(Stage::JoinVm);
                let outcome = worker
                    .join()
                    .map_err(|_| "Vita VM thread panicked".to_string());
                drop(joining);
                result?;
                if interrupted { Ok(0) } else { outcome? }
            })();
            krkr_protocol::diagnostic!("[VITA][BOOT] game END {outcome:?}");
            if let Err(error) = &outcome {
                krkr_host_vita::memory::trace_free("game failure");
                // Preserve allocation context in the external console logger.
                let gpu = &windows.graphics.gpu;
                let detail = format!(
                    "{error}\nresident={}/{} scratch={}/{} staging={}/{} bytes\n{}\n",
                    gpu.resident.used(),
                    gpu.resident.limit(),
                    gpu.scratch.used(),
                    gpu.scratch.limit(),
                    gpu.staging.used(),
                    gpu.staging.limit(),
                    krkr_host_vita::memory::free_report()
                );
                krkr_protocol::log!(Error, "[VITA][FAILURE] {detail}");
            }
            drop(watchdog);
            krkr_protocol::diagnostics::set_enabled(false);
            message = match outcome {
                Ok(0) => String::new(),
                Ok(code) => format!("游戏退出，状态码 {code}。"),
                Err(error) => error,
            };
        }
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(target_os = "vita")]
    {
        match native::run() {
            Ok(code) => std::process::ExitCode::from(code as u8),
            Err(error) => {
                eprintln!("krkr-vita: {error}");
                std::process::ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(target_os = "vita"))]
    {
        eprintln!("Build this host with cargo vita build vpk --release -p krkr-host-vita");
        std::process::ExitCode::FAILURE
    }
}

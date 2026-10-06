use crate::{Execution, diagnostic, execution::Usage};
use krkr_engine::EngineEvent;
use std::{
    num::NonZeroUsize,
    time::{Duration, Instant},
};
use tjs_core::{Module, RunBudget};
use tjs_runtime::{Runtime, RuntimeExit};

pub(crate) fn execute(
    runtime: Runtime,
    module: &Module,
    options: &Execution,
    compile_time: Duration,
    windows: Option<krkr_engine::protocol::window::Client>,
    project: Option<&crate::game::Project>,
) -> Result<std::process::ExitCode, String> {
    let mut system = krkr_engine::system::SystemConfig::for_process().map_err(|e| e.to_string())?;
    if let Some(project) = project {
        system.exe_path =
            krkr_engine::assets::local::directory(&project.root).map_err(|e| e.to_string())?;
        system.data_path =
            krkr_engine::assets::local::directory(&project.data).map_err(|e| e.to_string())?;
    }
    let application = windows.is_some() || project.is_some();
    let mut engine = krkr_host_desktop::create_engine(runtime, system, windows)?;
    krkr_plugins::register(&mut engine.runtime_mut().heap).map_err(|e| e.to_string())?;
    let root = if application {
        engine.start(module)
    } else {
        engine.submit(module)
    }
    .map_err(|_| "engine context capacity reached")?;
    let started = Instant::now();
    let controls = NonZeroUsize::new(64).expect("positive control budget");
    let mut display = None;
    let mut exit_code = std::process::ExitCode::SUCCESS;
    loop {
        if let Some(error) = engine
            .window_host_error()
            .or_else(|| engine.audio_host_error())
        {
            engine.reset();
            return Err(error);
        }
        let remaining = options.remaining(engine.work_executed());
        if remaining == 0 {
            engine.reset();
            return Err(
                "CLI instruction limit reached; adjust --max-instructions to allow more execution"
                    .into(),
            );
        }
        let budget = RunBudget::new(remaining).expect("positive remainder");
        let event = engine.poll(budget, controls);
        match event {
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
                    RuntimeExit::Finished(value) => {
                        if context == root {
                            display = Some(
                                engine
                                    .runtime()
                                    .heap
                                    .display(value)
                                    .map_err(|e| e.to_string())?,
                            );
                        }
                    }
                    RuntimeExit::Fault(error) => {
                        let _ = krkr_engine::debug::on_error(&mut engine.runtime_mut().heap);
                        return Err(diagnostic::render(&engine.runtime().sources, &error));
                    }
                    RuntimeExit::Thrown(error) => {
                        let _ = krkr_engine::debug::on_error(&mut engine.runtime_mut().heap);
                        return Err(diagnostic::render(
                            &engine.runtime().sources,
                            &error.diagnostic,
                        ));
                    }
                    _ => unreachable!("completed context"),
                }
                engine.take_result(context);
            }
            EngineEvent::Waiting { request, .. } => {
                if !engine.owns_wait(request) {
                    engine.reset();
                    return Err("external native wait requires a matching host service".into());
                }
            }
            EngineEvent::Terminated(code) => {
                exit_code = std::process::ExitCode::from(code as u8);
                break;
            }
            EngineEvent::Idle | EngineEvent::Yielded => {}
        }
        engine.collect_if_needed();
        match engine.sleep_duration() {
            Some(delay) if !delay.is_zero() => {
                std::thread::park_timeout(delay.min(Duration::from_millis(50)))
            }
            Some(_) => {}
            None if engine.pending_operations() != 0
                || engine.window_count() != 0
                || engine.application_running() =>
            {
                std::thread::park_timeout(Duration::from_millis(50))
            }
            None if display.is_some() => break,
            None => return Err("engine has no runnable work or pending deadline".into()),
        }
    }
    if let Some(display) = display {
        println!("{display}");
    }
    if options.stats {
        crate::print_stats(
            &Usage {
                instructions: engine.instructions_executed(),
                work: engine.work_executed(),
            },
            &engine.runtime().heap,
            module.register_count(),
            compile_time,
            started.elapsed(),
        );
    }
    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{num::NonZeroU32, sync::Arc};

    #[test]
    fn desktop_startup_policy_reaches_the_host_loop_and_preserves_delayed_exit_codes() {
        for (allow_windowless, expected) in [(false, 0), (true, 13)] {
            let mut runtime = Runtime::new();
            let script = format!(
                "System.exitOnNoWindowStartup={};var t=new Timer(%[action:function(e){{global.System.exit(13);}}]);t.interval=50;t.enabled=true;42;",
                !allow_windowless,
            );
            let source = runtime
                .sources
                .add_utf8("desktop startup", &script)
                .unwrap();
            let module = tjs_front::compile(&runtime.sources, source).unwrap();
            let runtime = crate::create_runtime(runtime.sources, Default::default()).unwrap();
            let options = Execution {
                slice: NonZeroU32::new(10000).unwrap(),
                max_instructions: NonZeroU32::new(100000),
                stats: false,
            };
            // The real worker loop uses its window protocol endpoint. These
            // startup scripts create no OS window or graphics device.
            let (client, _host) =
                krkr_engine::protocol::window::channel(Default::default(), Arc::new(|| {}));
            let code = execute(
                runtime,
                &module,
                &options,
                Duration::ZERO,
                Some(client),
                None,
            )
            .unwrap();
            assert_eq!(code, std::process::ExitCode::from(expected));
        }
    }
}

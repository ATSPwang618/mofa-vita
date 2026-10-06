mod cli;
mod diagnostic;
mod engine_execution;
mod execution;
mod game;
mod input;
mod logging;
mod storage;

use std::{mem::size_of, process::ExitCode, time::Instant};

use clap::Parser;
use cli::{Cli, Command, Execution, Host, LanguageCommand, ScriptCommand};
use execution::{Usage, drain_finalizers, execute};
use input::Input;
use tjs_core::{Diagnostic, Instruction, SourceMap, Value, Vm};
use tjs_front::{compiler, parser};

enum Action {
    Execute(Execution, Option<Host>),
    Tokens,
    Ast,
    Disasm,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    krkr_engine::protocol::diagnostics::set_enabled(cli.debug);
    if let Some(level) = cli.log_level {
        krkr_engine::protocol::diagnostics::set_level(level);
    }
    krkr_host_desktop::window::set_show_stats(cli.show_stats);
    let mut preprocessor = tjs_front::Preprocessor::default();
    for (name, value) in cli.definitions {
        preprocessor.set(&name, value);
    }
    match command(cli.command, &mut preprocessor) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn command(
    command: Command,
    preprocessor: &mut tjs_front::Preprocessor,
) -> Result<ExitCode, String> {
    let script = |command, host| match command {
        ScriptCommand::Eval { source, execution } => (
            Input::Expression(source),
            Action::Execute(execution.diagnostic(), host),
        ),
        ScriptCommand::Run { file, execution } => (
            Input::File(file),
            Action::Execute(execution.diagnostic(), host),
        ),
    };
    let (input, action) = match command {
        Command::Play(options) => return game::play(options, std::mem::take(preprocessor)),
        Command::Engine { host, command } => script(command, Some(host)),
        Command::Tjs { command } => match command {
            LanguageCommand::Script(command) => script(command, None),
            LanguageCommand::Tokens(source) => (source.into_input(), Action::Tokens),
            LanguageCommand::Ast(source) => (source.into_input(), Action::Ast),
            LanguageCommand::Disasm(source) => (source.into_input(), Action::Disasm),
        },
    };
    run(input, action, preprocessor)
}

fn run(
    input: Input,
    action: Action,
    preprocessor: &mut tjs_front::Preprocessor,
) -> Result<ExitCode, String> {
    if matches!(&action, Action::Execute(_, Some(_))) {
        krkr_engine::configure_preprocessor(preprocessor);
    }
    let (name, content) = input::read(&input)?;
    let start = Instant::now();
    let mut sources = SourceMap::new();
    let module = match content {
        input::Content::Bytecode(bytes) => {
            if matches!(action, Action::Tokens | Action::Ast) {
                return Err(
                    "tokens/ast require source text; use disasm to inspect bytecode".into(),
                );
            }
            tjs_runtime::bytecode::decode_with_preprocessor(
                &bytes,
                &name,
                &mut sources,
                preprocessor,
            )
            .map_err(|error| diagnostic::render(&sources, &error))?
            .0
        }
        input::Content::Source(units) => {
            let source = sources
                .add_utf16(name, units)
                .map_err(|error| error.to_string())?;
            let show = |error: Diagnostic| diagnostic::render(&sources, &error);
            let (program, lexed) =
                parser::parse_source_with_preprocessor(&sources, source, preprocessor)
                    .map_err(&show)?;
            if matches!(action, Action::Tokens) {
                for token in lexed.tokens() {
                    println!(
                        "{}..{} {:?}",
                        token.span.start().get(),
                        token.span.end().get(),
                        token.kind
                    );
                }
                for trivia in lexed.trivia() {
                    println!(
                        "trivia {}..{} {:?}",
                        trivia.span.start().get(),
                        trivia.span.end().get(),
                        trivia.kind
                    );
                }
                return Ok(ExitCode::SUCCESS);
            }
            if matches!(action, Action::Ast) {
                println!("{program:#?}");
                return Ok(ExitCode::SUCCESS);
            }
            compiler::compile(&sources, &program).map_err(&show)?
        }
    };
    let compile_time = start.elapsed();
    if matches!(action, Action::Disasm) {
        print!("{}", module.disassemble());
        return Ok(ExitCode::SUCCESS);
    }
    let Action::Execute(options, host) = action else {
        unreachable!("inspection commands return before execution")
    };
    let preprocessor = std::mem::take(preprocessor);
    if host == Some(Host::Desktop) {
        return krkr_host_desktop::window::run(move |host| {
            let runtime = create_runtime(sources, preprocessor)?;
            engine_execution::execute(runtime, &module, &options, compile_time, Some(host), None)
        });
    }
    if host == Some(Host::Headless) {
        let runtime = create_runtime(sources, preprocessor)?;
        return engine_execution::execute(runtime, &module, &options, compile_time, None, None);
    }
    let mut runtime = tjs_runtime::Runtime::with_sources(sources, preprocessor);
    execute_plain(&mut runtime, &module, &options, compile_time)
}
fn create_runtime(
    sources: SourceMap,
    preprocessor: tjs_front::Preprocessor,
) -> Result<tjs_runtime::Runtime, String> {
    let mut runtime = tjs_runtime::Runtime::with_sources(sources, preprocessor);
    let vfs = krkr_engine::assets::Vfs::new(
        &std::env::current_dir().map_err(|error| error.to_string())?,
        Default::default(),
    )
    .map_err(|error| error.to_string())?;
    krkr_engine::storages::install(&mut runtime.heap, vfs).map_err(|error| error.to_string())?;
    krkr_engine::install(&mut runtime, logging::Logs).map_err(|error| error.to_string())?;
    Ok(runtime)
}
fn execute_plain(
    runtime: &mut tjs_runtime::Runtime,
    module: &tjs_core::Module,
    options: &Execution,
    compile_time: std::time::Duration,
) -> Result<ExitCode, String> {
    let mut vm = Vm::new(module);
    let start = Instant::now();
    let mut usage = Usage::default();
    let result = execute(&mut vm, runtime, options, &mut usage);
    let result = result.map_err(|error| diagnostic::render(&runtime.sources, &error))?;
    let display = runtime
        .heap
        .display(result)
        .map_err(|error| error.to_string())?;
    drop(vm);
    runtime.collect([]);
    drain_finalizers(runtime, options, &mut usage, None, &[])
        .map_err(|error| diagnostic::render(&runtime.sources, &error))?;
    let elapsed = start.elapsed();
    println!("{display}");
    if options.stats {
        print_stats(
            &usage,
            &runtime.heap,
            module.register_count(),
            compile_time,
            elapsed,
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn print_stats(
    usage: &Usage,
    heap: &tjs_core::Heap,
    entry_registers: u32,
    compile_time: std::time::Duration,
    elapsed: std::time::Duration,
) {
    eprintln!(
        "compile_us={} execute_us={} instructions={} work_units={} entry_registers={} value_bytes={} instruction_bytes={}",
        compile_time.as_micros(),
        elapsed.as_micros(),
        usage.instructions,
        usage.work,
        entry_registers,
        size_of::<Value>(),
        size_of::<Instruction>()
    );
    eprintln!(
        "heap_live={:?} heap_capacity={:?}",
        heap.counts(),
        heap.capacities()
    );
}

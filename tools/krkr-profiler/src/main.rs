mod audio;
mod heap;
mod images;
mod movie;
mod report;
mod run;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Record and compare krkr performance with the Vita GLES path"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Run a game offscreen and record its timeline, allocations and graphics traffic.
    Run(Box<run::Options>),
    /// Measure image preparation without starting a game or a graphics context.
    Images(images::Options),
    /// Rebuild the report; timestamps are relative to capture start.
    Report {
        directory: PathBuf,
        #[arg(long, default_value_t = 0.0)]
        from_ms: f64,
        #[arg(long)]
        to_ms: Option<f64>,
    },
    /// Compare reports from two recordings.
    Compare { before: PathBuf, after: PathBuf },
}
fn main() -> std::process::ExitCode {
    let result = match Cli::parse().command {
        Command::Run(options) => std::thread::Builder::new()
            .name("graphics".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || run::run(*options))
            .map_err(|e| e.to_string())
            .and_then(|thread| thread.join().map_err(|_| "capture worker panicked".into()))
            .and_then(|result| result),
        Command::Images(options) => images::run(options),
        Command::Report {
            directory,
            from_ms,
            to_ms,
        } => report::generate(&directory, from_ms, to_ms).map(|_| ()),
        Command::Compare { before, after } => report::compare(&before, &after),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

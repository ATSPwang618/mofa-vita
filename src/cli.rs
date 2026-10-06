//! Runtime and diagnostic command arguments, independent of resource conversion.
use crate::input::Input;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::{num::NonZeroU32, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "Run and inspect TJS scripts with the krkr engine")]
pub(crate) struct Cli {
    /// Enable engine diagnostic messages and Debug console/file output.
    #[arg(long, global = true)]
    pub debug: bool,
    /// Console verbosity; trace includes individual slow calls.
    #[arg(long, global = true)]
    pub log_level: Option<krkr_engine::protocol::diagnostics::Level>,
    /// Show FPS, process RAM and renderer allocation usage (updated once a second).
    #[arg(long, global = true)]
    pub show_stats: bool,
    /// Set a preprocessor symbol (NAME defaults to 1; values are signed decimal).
    #[arg(short = 'D', long = "define", global = true, value_name = "NAME[=VALUE]", value_parser = definition)]
    pub definitions: Vec<(String, i32)>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Run or inspect TJS without engine classes or host services.
    Tjs {
        #[command(subcommand)]
        command: LanguageCommand,
    },
    /// Debug a script with engine classes and host services.
    Engine {
        #[arg(long, global = true, value_enum, default_value = "headless")]
        host: Host,
        #[command(subcommand)]
        command: ScriptCommand,
    },
    /// Start a game: resource filter, compatibility patch, then entry script.
    Play(Play),
}

#[derive(Subcommand)]
pub(crate) enum LanguageCommand {
    #[command(flatten)]
    Script(ScriptCommand),
    /// Show tokens and comment trivia.
    Tokens(SourceInput),
    /// Show the parsed syntax tree.
    Ast(SourceInput),
    /// Show register instructions and function metadata.
    Disasm(SourceInput),
}

#[derive(Subcommand)]
pub(crate) enum ScriptCommand {
    /// Execute source text (including its semicolons).
    Eval {
        source: String,
        #[command(flatten)]
        execution: Execution,
    },
    /// Execute a UTF-8 or BOM-marked UTF-16 file.
    Run {
        file: PathBuf,
        #[command(flatten)]
        execution: Execution,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Host {
    Headless,
    Desktop,
}

#[derive(Args)]
pub(crate) struct Play {
    pub root: PathBuf,
    /// Script or XP3 package (opens its startup.tjs); also accepts archive.xp3>script.tjs.
    #[arg(long, default_value = "startup.tjs")]
    pub entry: String,
    /// Compatibility script relative to the game root; default: root/patch.tjs if present.
    #[arg(long, conflicts_with = "no_patch")]
    pub patch: Option<PathBuf>,
    /// Run this script after startup, for a desktop replay or inspection session.
    #[arg(long)]
    pub after_startup: Option<PathBuf>,
    #[arg(long)]
    pub no_patch: bool,
    /// Script extraction filter relative to the game root; default: root/xp3filter.tjs if present.
    #[arg(long, conflicts_with = "no_filter")]
    pub xp3_filter: Option<PathBuf>,
    #[arg(long)]
    pub no_filter: bool,
    /// Default text encoding for scripts without a BOM.
    #[arg(long, default_value = "utf-8")]
    pub encoding: String,
    /// Save directory; default: game-root/savedata. Relative paths use the game root.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
    #[arg(long, value_enum, default_value = "desktop")]
    pub host: Host,
    #[command(flatten)]
    pub execution: Execution,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
pub(crate) struct SourceInput {
    /// Read source from this file.
    file: Option<PathBuf>,
    /// Read source directly from an argument.
    #[arg(long)]
    expr: Option<String>,
}

impl SourceInput {
    pub fn into_input(self) -> Input {
        match self.expr {
            Some(source) => Input::Expression(source),
            None => Input::File(self.file.expect("clap requires one source input")),
        }
    }
}

#[derive(Args)]
pub(crate) struct Execution {
    /// Work units per VM slice: instructions, exception searches and continuations.
    #[arg(long, default_value = "10000")]
    pub slice: NonZeroU32,
    /// Total work limit; scripts default to 10000000, play has no cumulative limit.
    #[arg(long)]
    pub max_instructions: Option<NonZeroU32>,
    /// Print coarse compilation/execution timings and layout sizes.
    #[arg(long)]
    pub stats: bool,
}

impl Execution {
    pub fn diagnostic(mut self) -> Self {
        self.max_instructions
            .get_or_insert(NonZeroU32::new(10_000_000).unwrap());
        self
    }

    pub fn remaining(&self, executed: u64) -> u32 {
        self.max_instructions.map_or(self.slice.get(), |limit| {
            u64::from(limit.get())
                .saturating_sub(executed)
                .min(u64::from(self.slice.get())) as u32
        })
    }
}

fn definition(input: &str) -> Result<(String, i32), String> {
    let (name, value) = input.split_once('=').unwrap_or((input, "1"));
    let mut units = name.encode_utf16();
    let start = |u| matches!(u, 65..=90 | 97..=122 | 95 | 0x100..=0xffff);
    if !units.next().is_some_and(start) || !units.all(|u| start(u) || matches!(u, 48..=57)) {
        return Err("expected a TJS preprocessor symbol".into());
    }
    let value = value
        .parse::<i32>()
        .map_err(|_| "expected a signed 32-bit decimal value")?;
    Ok((name.into(), value))
}

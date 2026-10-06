use clap::{Args, Parser, Subcommand, ValueEnum};
mod helper_ui;
use krkr_convert::{archive, media};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(version, about = "XP3 archives and extension-consistent game media")]
struct Cli {
    /// Concurrent file workers (defaults to available CPUs, up to 64).
    #[arg(short = 'j', long, global = true, value_parser = clap::value_parser!(u16).range(1..=64))]
    jobs: Option<u16>,
    /// Hide terminal progress bars. JSON is always written separately.
    #[arg(long, global = true)]
    no_progress: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Normalize media filenames from content and generate internal links; no transcoding.
    Normalize {
        source: PathBuf,
        /// Defaults to a sibling <source>-normalized directory.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        tools: MediaTools,
    },
    /// Interactively prepare a whole game, choosing conversion parameters.
    Helper {
        /// Game directory; prompted when omitted.
        source: Option<PathBuf>,
        #[command(flatten)]
        tools: MediaTools,
    },
    /// Build a separate PSV asset tree; scripts keep their logical coordinates.
    Psv {
        source: PathBuf,
        /// Lossy GPU textures for selected images (relative glob, repeatable).
        /// BC1 for opaque images, BC3 for transparency using built-in Crunch/rgbcx.
        #[arg(long)]
        texture_glob: Vec<String>,
        /// Automatically screen static images; uncertain candidates stay lossless.
        #[arg(long, conflicts_with = "texture_glob")]
        texture_auto: bool,
        /// Encoding effort; all modes retain the same quality rejection thresholds.
        #[arg(long, value_enum, default_value_t = krkr_convert::bc::Quality::Balanced)]
        texture_quality: krkr_convert::bc::Quality,
        /// Texture storage: losslessly packed BC (recommended), or raw native BC.
        #[arg(long, value_enum, default_value = "bc-crunch")]
        texture_storage: krkr_convert::bc::Storage,
        /// ATRAC9 for selected mono/stereo audio (repeatable relative glob).
        #[arg(long)]
        at9_glob: Vec<String>,
        /// Original game canvas (for example 1920x1080).
        #[arg(long, value_parser = krkr_convert::psv::dimensions)]
        canvas: krkr_protocol::graphics::Size,
        #[arg(long, default_value = "960x544", value_parser = krkr_convert::psv::dimensions)]
        size: krkr_protocol::graphics::Size,
        /// Defaults to a sibling <source>-psv directory.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        tools: MediaTools,
    },
    /// Recursively compare resource suffixes with their actual content; emit JSON.
    Probe {
        source: PathBuf,
        /// Write JSON here instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        tools: MediaTools,
    },
    /// Transcode content to its existing suffix in place; use normalize to retain codecs and create links.
    Adjust {
        report: PathBuf,
        /// Write the per-file result JSON here instead of stdout.
        #[arg(short, long)]
        output: Option<PathBuf>,
        #[command(flatten)]
        tools: MediaTools,
    },
    /// Unpack XP3, decrypt in place, or pack directories.
    Xp3 {
        #[command(subcommand)]
        command: Xp3,
    },
}
#[derive(Args)]
struct MediaTools {
    #[arg(long, default_value = "ffprobe")]
    ffprobe: PathBuf,
    #[arg(long, default_value = "ffmpeg")]
    ffmpeg: PathBuf,
}
impl From<MediaTools> for media::Tools {
    fn from(value: MediaTools) -> Self {
        Self {
            ffprobe: value.ffprobe,
            ffmpeg: value.ffmpeg,
        }
    }
}
#[derive(Subcommand)]
enum Xp3 {
    /// Extract each XP3 beside itself, into a folder without .xp3.
    Unpack {
        /// One or more archive paths or quoted wildcard patterns.
        #[arg(required = true, num_args = 1..)]
        inputs: Vec<PathBuf>,
        /// Optional extraction script for encrypted entries; protection flags alone do not require one.
        #[arg(long)]
        xp3_filter: Option<PathBuf>,
        /// Defaults to the filter script's parent.
        #[arg(long, requires = "xp3_filter")]
        filter_root: Option<PathBuf>,
        #[arg(long, default_value = "utf-8")]
        encoding: String,
    },
    /// Apply xp3filter.tjs and atomically replace each archive with a plaintext XP3.
    Decrypt {
        #[arg(required = true, num_args = 1..)]
        inputs: Vec<PathBuf>,
        /// Defaults to xp3filter.tjs beside each input archive. DLLs are not supported.
        #[arg(long)]
        xp3_filter: Option<PathBuf>,
        /// Defaults to the filter script's parent.
        #[arg(long)]
        filter_root: Option<PathBuf>,
        #[arg(long, default_value = "utf-8")]
        encoding: String,
        #[arg(long, value_enum, default_value = "zlib")]
        compression: Compression,
    },
    /// Pack a directory into a sibling <directory-name>.xp3.
    Pack {
        source: PathBuf,
        /// Pack each immediate child directory into a separate sibling XP3.
        #[arg(long, alias = "multi")]
        each: bool,
        #[arg(long, value_enum, default_value = "zlib")]
        compression: Compression,
    },
}
#[derive(Clone, Copy, ValueEnum)]
enum Compression {
    None,
    Zlib,
    Auto,
}
impl From<Compression> for krkr_assets::xp3::Compression {
    fn from(value: Compression) -> Self {
        match value {
            Compression::None => Self::None,
            Compression::Zlib => Self::Zlib,
            Compression::Auto => Self::Auto,
        }
    }
}
fn json(value: &impl serde::Serialize, output: Option<&Path>) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    if let Some(path) = output {
        let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
        let mut temp = tempfile::NamedTempFile::new_in(absolute.parent().ok_or("output parent")?)
            .map_err(|e| e.to_string())?;
        temp.write_all(&bytes).map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist_noclobber(&absolute)
            .map_err(|e| format!("{}: {}", path.display(), e.error))?;
    } else {
        std::io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
fn run(command: Command, jobs: Option<u16>) -> Result<(), String> {
    match command {
        Command::Normalize {
            source,
            output,
            tools,
        } => {
            let output = output.unwrap_or_else(|| {
                let mut name = source.file_name().unwrap_or_default().to_owned();
                name.push("-normalized");
                source.with_file_name(name)
            });
            let tools = tools.into();
            let prepared = krkr_convert::helper::Prepared::new(&source, &output, None)?;
            let inventory = prepared.inspect(&tools)?;
            json(
                &prepared.convert(inventory, krkr_convert::helper::Target::Normalize, &tools)?,
                None,
            )
        }
        Command::Helper { source, tools } => helper_ui::run(source, tools.into(), jobs),
        Command::Psv {
            texture_glob,
            texture_auto,
            texture_quality,
            texture_storage,
            at9_glob,
            source,
            canvas,
            size,
            output,
            tools,
        } => {
            let at9 = if at9_glob.is_empty() {
                None
            } else {
                Some(krkr_convert::at9::Options {
                    tool: krkr_convert::encoders::audio()?,
                    globs: at9_glob,
                })
            };
            let report = krkr_convert::psv::build(
                &source,
                output.as_deref(),
                &krkr_convert::psv::Options {
                    at9,
                    texture_globs: texture_glob,
                    texture_auto,
                    texture_quality,
                    texture_storage,
                    canvas,
                    target: size,
                },
                &tools.into(),
                |_| {},
            )?;
            json(&report, None)
        }
        Command::Probe {
            source,
            output,
            tools,
        } => {
            let report = media::inspect(&source, &tools.into())?;
            json(&report, output.as_deref())?;
            if report
                .entries
                .iter()
                .any(|e| e.consistency == media::Consistency::Unreadable)
            {
                return Err("unreadable media found; see probe JSON".into());
            }
            Ok(())
        }
        Command::Adjust {
            report,
            output,
            tools,
        } => {
            // Check an explicit report destination before making replacements.
            if output.as_ref().is_some_and(|p| p.exists()) {
                return Err("output already exists".into());
            }
            if let Some(path) = &output {
                let absolute = std::path::absolute(path).map_err(|e| e.to_string())?;
                // Reserve and drop a temporary file to check the directory before
                // any media is replaced; the final report is still published atomically.
                let _check = tempfile::NamedTempFile::new_in(
                    absolute.parent().ok_or("output parent missing")?,
                )
                .map_err(|e| e.to_string())?;
            }
            let report = krkr_convert::adjust::read_report(&report)?;
            let result = krkr_convert::adjust::apply(&report, &tools.into(), |_| {})?;
            json(&result, output.as_deref())?;
            if result.failed() {
                Err("some files could not be adjusted; see result JSON".into())
            } else {
                Ok(())
            }
        }
        Command::Xp3 { command } => match command {
            Xp3::Unpack {
                inputs,
                xp3_filter,
                filter_root,
                encoding,
            } => {
                let options = xp3_filter.map(|script| archive::FilterOptions {
                    script: Some(script),
                    root: filter_root,
                    encoding,
                });
                archive::unpack_with_filter(
                    &inputs,
                    options.as_ref(),
                    krkr_convert::progress::println,
                )
            }
            Xp3::Pack {
                source,
                each,
                compression,
            } => archive::pack(
                &source,
                each,
                compression.into(),
                krkr_convert::progress::println,
            ),
            Xp3::Decrypt {
                inputs,
                xp3_filter,
                filter_root,
                encoding,
                compression,
            } => archive::decrypt(
                &inputs,
                &archive::FilterOptions {
                    script: xp3_filter,
                    root: filter_root,
                    encoding,
                },
                compression.into(),
                krkr_convert::progress::println,
            ),
        },
    }
}
fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(64)
}
fn main() -> ExitCode {
    let cli = Cli::parse();
    krkr_convert::progress::set_enabled(!cli.no_progress);
    let result = if matches!(&cli.command, Command::Helper { .. }) {
        // Helper asks for a worker count before starting any resource work.
        run(cli.command, cli.jobs)
    } else {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.jobs.map(usize::from).unwrap_or_else(default_jobs))
            .build()
            .map_err(|e| format!("cannot start worker pool: {e}"))
            .and_then(|pool| pool.install(|| run(cli.command, cli.jobs)))
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

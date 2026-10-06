//! Game launch policy and host assembly. Storage and filter behavior live in libraries.
use crate::{
    cli::{Host, Play},
    diagnostic, engine_execution, logging,
};
use krkr_engine::{
    assets::{Vfs, local, name},
    scripts, storages,
};
use std::{
    fmt::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Instant,
};
use tjs_core::SourceMap;
use tjs_runtime::Runtime;

pub(crate) struct Project {
    pub root: PathBuf,
    pub data: PathBuf,
}

pub(crate) fn play(
    options: Play,
    mut preprocessor: tjs_front::Preprocessor,
) -> Result<ExitCode, String> {
    let root = std::path::absolute(&options.root).map_err(|e| e.to_string())?;
    if !root.is_dir() {
        return Err(format!("game root is not a directory: {}", root.display()));
    }
    let project = Project {
        data: root.join(options.data_dir.as_deref().unwrap_or(Path::new("savedata"))),
        root,
    };
    let patch = script_path(
        &project.root,
        options.patch.as_deref(),
        "patch.tjs",
        options.no_patch,
    )?;
    let filter = script_path(
        &project.root,
        options.xp3_filter.as_deref(),
        "xp3filter.tjs",
        options.no_filter,
    )?;
    krkr_engine::configure_preprocessor(&mut preprocessor);
    let launch = move |windows| {
        let started = Instant::now();
        let mut entry = options.entry;
        if !entry.contains('>')
            && entry
                .rsplit_once('.')
                .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("xp3"))
        {
            entry.push_str(">startup.tjs");
        }
        let requested = name::units(&entry);
        let mut vfs = Vfs::for_project(&project.root, &requested, Default::default())
            .map_err(|e| e.to_string())?;
        let encoding = name::units(&options.encoding);
        // Install before any entry/patch resource is read. The filter script runs
        // in its own decoder VM, never as ordinary game startup code.
        if let Some(path) = filter {
            let provider =
                krkr_plugins::script_filter(&path, &project.root, &encoding, vfs.limits())
                    .map_err(|e| format!("{}: {e}", path.display()))?;
            vfs.set_filter(Some(provider));
            if krkr_engine::protocol::diagnostics::enabled() {
                eprintln!("XP3 filter: {}", path.display());
            }
        }
        let requested = if name::split_archive(&requested).1.is_some() {
            name::normalize(
                &requested,
                &local::directory(&project.root).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?
        } else {
            requested
        };
        let entry = vfs.plan(&requested).map_err(|e| e.to_string())?.name;
        if krkr_engine::protocol::diagnostics::enabled() {
            eprintln!(
                "Project: {}",
                String::from_utf16_lossy(vfs.current_directory())
            );
            eprintln!("Entry: {}", String::from_utf16_lossy(&entry));
        }
        let mut bootstrap = String::new();
        if let Some(path) = patch {
            let path = local::units(&path).map_err(|e| e.to_string())?;
            // Absolute local name: an archive patch.tjs cannot shadow the user's file.
            writeln!(bootstrap, "Scripts.execStorage({});", literal(&path)).unwrap();
            if krkr_engine::protocol::diagnostics::enabled() {
                eprintln!("Compatibility patch: {}", String::from_utf16_lossy(&path));
            }
        }
        writeln!(bootstrap, "Scripts.execStorage({});", literal(&entry)).unwrap();
        if let Some(path) = options.after_startup {
            let path = std::path::absolute(path).map_err(|e| e.to_string())?;
            let path = local::units(&path).map_err(|e| e.to_string())?;
            writeln!(bootstrap, "Scripts.execStorage({});", literal(&path)).unwrap();
        }
        let mut sources = SourceMap::new();
        let source = sources
            .add_utf8("<game startup>", &bootstrap)
            .map_err(|e| e.to_string())?;
        let module = tjs_front::compile_with_preprocessor(&sources, source, &mut preprocessor)
            .map_err(|e| diagnostic::render(&sources, &e))?;
        let mut runtime = Runtime::with_sources(sources, preprocessor);
        storages::install(&mut runtime.heap, vfs).map_err(|e| e.to_string())?;
        krkr_engine::install(&mut runtime, logging::Logs).map_err(|e| e.to_string())?;
        scripts::set_text_encoding(&mut runtime.heap, encoding).map_err(|e| e.to_string())?;
        engine_execution::execute(
            runtime,
            &module,
            &options.execution,
            started.elapsed(),
            windows,
            Some(&project),
        )
    };
    if options.host == Host::Desktop {
        krkr_host_desktop::window::run(move |host| launch(Some(host)))
    } else {
        launch(None)
    }
}

fn script_path(
    root: &Path,
    requested: Option<&Path>,
    default: &str,
    disabled: bool,
) -> Result<Option<PathBuf>, String> {
    if disabled {
        return Ok(None);
    }
    let path = root.join(requested.unwrap_or(Path::new(default)));
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(Some(path)),
        Err(e) if requested.is_none() && e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Ok(_) => Err(format!("script is not a file: {}", path.display())),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn literal(units: &[u16]) -> String {
    let mut text = String::from("\"");
    for unit in units {
        write!(text, "\\x{unit:04x}").unwrap();
    }
    text.push('"');
    text
}

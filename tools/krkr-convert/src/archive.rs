//! Batch XP3 workflows; archive parsing and filter execution remain shared with
//! the engine. Each completed archive is published independently.
use crate::{
    files,
    media::{Result, at, hash},
    progress::Progress,
};
use krkr_assets::{
    Limits, name,
    xp3::{Compression, FilterFactory, offline},
};
use rayon::prelude::*;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub struct FilterOptions {
    pub script: Option<PathBuf>,
    pub root: Option<PathBuf>,
    pub encoding: String,
}

pub fn expand(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if inputs.is_empty() {
        return Err("at least one XP3 path or pattern is required".into());
    }
    let mut paths = BTreeSet::new();
    for input in inputs {
        let text = input.to_str().ok_or("non-Unicode input path")?;
        let matches = if input.exists() || !text.contains(['*', '?', '[']) {
            vec![input.clone()]
        } else {
            let options = glob::MatchOptions {
                case_sensitive: false,
                require_literal_separator: true,
                require_literal_leading_dot: false,
            };
            let found = glob::glob_with(text, options)
                .map_err(|e| at(input, e))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| at(input, e))?;
            if found.is_empty() {
                return Err(at(input, "pattern matched no files"));
            }
            found
        };
        for path in matches {
            files::regular(&path, false)?;
            if !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("xp3"))
            {
                return Err(at(&path, "expected an .xp3 archive"));
            }
            paths.insert(fs::canonicalize(&path).map_err(|e| at(&path, e))?);
        }
    }
    Ok(paths.into_iter().collect())
}

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(at(path, e)),
        Ok(_) => Err(at(path, "output already exists")),
    }
}

fn batch<T: Send>(items: Vec<T>, action: impl Fn(T) -> Result<()> + Send + Sync) -> Result<()> {
    // Indexed collection keeps diagnostics deterministic and waits for every
    // worker before temporary outputs can be dropped.
    let errors: Vec<_> = items
        .into_par_iter()
        .map(action)
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(Result::err)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

pub fn unpack(inputs: &[PathBuf], progress: impl FnMut(&str) + Send) -> Result<()> {
    unpack_with_filter(inputs, None, progress)
}

pub fn unpack_with_filter(
    inputs: &[PathBuf],
    options: Option<&FilterOptions>,
    progress: impl FnMut(&str) + Send,
) -> Result<()> {
    let progress = Mutex::new(progress);
    let archives = expand(inputs)?;
    // Reject output collisions before extracting the first archive.
    let mut outputs = BTreeSet::new();
    for source in &archives {
        let output = source.with_extension("");
        absent(&output)?;
        if !outputs.insert(output.to_string_lossy().to_ascii_lowercase()) {
            return Err(at(&output, "duplicate extraction destination"));
        }
    }
    batch(archives, |source| {
        let output = source.with_extension("");
        let bar = Progress::new(
            format!("unpack {}", source.file_name().unwrap().to_string_lossy()),
            None,
        );
        let filter = options
            .map(|options| provider(&source, options))
            .transpose()?;
        let result = offline::unpack_archive_with_progress(
            &source,
            &output,
            filter.as_deref(),
            Limits::default(),
            &|event| bar.archive(event),
        )
        .map_err(|e| at(&source, e))?;
        progress.lock().unwrap()(&format!(
            "{} files, {} bytes -> {}",
            result.files,
            result.bytes,
            output.display()
        ));
        Ok(())
    })
}

pub(crate) fn provider(source: &Path, options: &FilterOptions) -> Result<Arc<dyn FilterFactory>> {
    let script = options
        .script
        .clone()
        .unwrap_or_else(|| source.parent().unwrap().join("xp3filter.tjs"));
    let script = fs::canonicalize(&script).map_err(|e| at(&script, e))?;
    let root = options
        .root
        .clone()
        .unwrap_or_else(|| script.parent().unwrap().to_owned());
    let root = fs::canonicalize(&root).map_err(|e| at(&root, e))?;
    files::regular(&root, true)?;
    krkr_plugins::script_filter_checked(
        &script,
        &root,
        &name::units(&options.encoding),
        Limits::default(),
    )
    .map_err(|e| at(&script, e))
}

pub fn decrypt(
    inputs: &[PathBuf],
    options: &FilterOptions,
    compression: Compression,
    progress: impl FnMut(&str) + Send,
) -> Result<()> {
    let progress = Mutex::new(progress);
    let archives = expand(inputs)?;
    batch(archives, |source| {
        let bar = Progress::new(
            format!("decrypt {}", source.file_name().unwrap().to_string_lossy()),
            None,
        );
        let filter = provider(&source, options)?;
        let original = hash(&source)?;
        let result = files::replace(&source, &original, |output| {
            offline::decrypt_archive_with_progress(
                &source,
                output,
                filter.as_ref(),
                compression,
                Limits::default(),
                &|event| bar.archive(event),
            )
            .map_err(|e| at(&source, e))
        })?;
        progress.lock().unwrap()(&format!(
            "decrypted {} files, {} bytes -> {}",
            result.files,
            result.bytes,
            source.display()
        ));
        Ok(())
    })
}

pub fn pack(
    source: &Path,
    each: bool,
    compression: Compression,
    progress: impl FnMut(&str) + Send,
) -> Result<()> {
    let progress = Mutex::new(progress);
    files::regular(source, true)?;
    let source = fs::canonicalize(source).map_err(|e| at(source, e))?;
    let mut directories = if each {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&source).map_err(|e| at(&source, e))? {
            let path = entry.map_err(|e| at(&source, e))?.path();
            if path.is_dir() {
                files::regular(&path, true)?;
                paths.push(path);
            }
        }
        if paths.is_empty() {
            return Err(at(&source, "no immediate child directories"));
        }
        paths
    } else {
        vec![source]
    };
    directories.sort();
    let jobs: Vec<_> = directories
        .into_iter()
        .map(|source| {
            let mut filename = source
                .file_name()
                .ok_or("pack source must have a directory name")?
                .to_owned();
            filename.push(".xp3");
            let output = source.with_file_name(filename);
            absent(&output)?;
            Ok((source, output))
        })
        .collect::<Result<_>>()?;
    batch(jobs, |(source, output)| {
        let bar = Progress::new(
            format!("pack {}", source.file_name().unwrap().to_string_lossy()),
            None,
        );
        let result = offline::pack_directory_with_progress(
            &source,
            &output,
            compression,
            Limits::default(),
            &|event| bar.archive(event),
        )
        .map_err(|e| at(&source, e))?;
        progress.lock().unwrap()(&format!(
            "{} files, {} bytes -> {}",
            result.files,
            result.bytes,
            output.display()
        ));
        Ok(())
    })
}

use crate::media::{Result, at, hash};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn regular(path: &Path, directory: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path).map_err(|e| at(path, e))?;
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let linked = meta.file_type().is_symlink();
    if linked
        || if directory {
            !meta.is_dir()
        } else {
            !meta.is_file()
        }
    {
        return Err(at(
            path,
            if directory {
                "expected a regular directory"
            } else {
                "expected a regular file"
            },
        ));
    }
    Ok(())
}

/// Resolve report entries component by component, rejecting links and Windows
/// alternate streams as well as absolute and parent-relative paths.
pub(crate) fn resource(root: &Path, name: &str) -> Result<PathBuf> {
    if name.is_empty() || name.contains(['\\', ':']) {
        return Err(format!("invalid resource path: {name}"));
    }
    let mut path = root.to_owned();
    let parts: Vec<_> = name.split('/').collect();
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || *part == "." || *part == ".." || part.ends_with(['.', ' ']) {
            return Err(format!("invalid resource path: {name}"));
        }
        path.push(part);
        regular(&path, i + 1 < parts.len())?;
    }
    Ok(path)
}

/// The closure produces and verifies a complete sibling file. No source is
/// removed first; rename is the commit point for this one file.
pub(crate) fn replace<T>(
    source: &Path,
    expected_hash: &str,
    build: impl FnOnce(&Path) -> Result<T>,
) -> Result<T> {
    regular(source, false)?;
    let permissions = fs::metadata(source)
        .map_err(|e| at(source, e))?
        .permissions();
    if permissions.readonly() {
        return Err(at(source, "source is read-only"));
    }
    if hash(source)? != expected_hash {
        return Err(at(source, "source changed since probing; run probe again"));
    }
    let scratch = tempfile::Builder::new()
        .prefix(".krkr-convert-")
        .tempdir_in(source.parent().ok_or("source parent missing")?)
        .map_err(|e| at(source, e))?;
    // Keep the suffix for encoders and probes which require it (e.g. raw DIB).
    let output = scratch
        .path()
        .join(source.file_name().ok_or("source filename missing")?);
    let result = build(&output)?;
    regular(&output, false)?;
    fs::File::options()
        .write(true)
        .open(&output)
        .and_then(|f| f.sync_all())
        .map_err(|e| at(&output, e))?;
    regular(source, false)?;
    if hash(source)? != expected_hash {
        return Err(at(
            source,
            "source changed during conversion; original was not replaced",
        ));
    }
    fs::set_permissions(&output, permissions).map_err(|e| at(&output, e))?;
    fs::rename(&output, source).map_err(|e| at(source, e))?;
    Ok(result)
}

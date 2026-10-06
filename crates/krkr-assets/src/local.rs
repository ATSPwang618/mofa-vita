//! Host path adaptation. Case-sensitive hosts resolve ASCII-folded game names
//! component by component while retaining the actual filesystem spelling.
use crate::{Error, Result, name};
mod lookup;
pub(crate) use lookup::Lookup;
#[cfg(any(target_os = "vita", test))]
mod vita;
use std::{
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
};

pub fn units(path: &Path) -> Result<Vec<u16>> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
        // canonicalize() returns Win32 verbatim paths. Storage syntax has no
        // device namespace; translate disk/UNC prefixes before normalization.
        if let Some(rest) = raw.strip_prefix(&[92, 92, 63, 92]) {
            if rest.len() >= 3 && rest[1] == 58 {
                return Ok(rest.to_vec());
            }
            if let Some(unc) = rest.strip_prefix(&[85, 78, 67, 92]) {
                return Ok([vec![92, 92], unc.to_vec()].concat());
            }
            return Err(Error::Name("unsupported Windows device path"));
        }
        Ok(raw)
    }
    #[cfg(not(windows))]
    {
        Ok(path
            .to_str()
            .ok_or(Error::Name("non-Unicode host path"))?
            .encode_utf16()
            .collect())
    }
}
#[cfg(target_os = "vita")]
pub(crate) fn vita_storage(text: &[u16]) -> Option<Vec<u16>> {
    String::from_utf16(text)
        .ok()
        .and_then(|text| vita::storage(&text))
        .map(|text| name::units(&text))
}
pub fn path(text: &[u16]) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        Ok(OsString::from_wide(name::c_string(text)).into())
    }
    #[cfg(not(windows))]
    {
        Ok(OsString::from(
            String::from_utf16(name::c_string(text))
                .map_err(|_| Error::Name("invalid Unicode host path"))?,
        )
        .into())
    }
}
/// Resolve a native path before dispatching work; Vita device paths are
/// absolute even though std's Unix Path representation calls them relative.
pub fn absolute(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(target_os = "vita")]
    {
        if path.to_str().and_then(vita::storage).is_some() {
            return Ok(path.to_owned());
        }
        return Ok(std::env::current_dir()?.join(path));
    }
    #[cfg(not(target_os = "vita"))]
    std::path::absolute(path)
}
pub fn directory(path: &Path) -> Result<Vec<u16>> {
    let absolute = absolute(path)?;
    let raw = units(&absolute)?;
    let current = name::units("file://./");
    let mut normalized = name::normalize(&raw, &current)?;
    if normalized.last() != Some(&47) {
        normalized.push(47);
    }
    Ok(normalized)
}
pub fn from_storage(text: &[u16]) -> Result<PathBuf> {
    if name::split_archive(text).1.is_some() {
        return Err(Error::Name("archive entries have no local filename"));
    }
    let body = text
        .strip_prefix(&[102, 105, 108, 101, 58, 47, 47])
        .ok_or(Error::Name("not a file storage"))?;
    let at = body
        .iter()
        .position(|&u| u == 47)
        .ok_or(Error::Name("missing domain separator"))?;
    let (domain, tail) = (&body[..at], &body[at..]);
    #[cfg(target_os = "vita")]
    return path(&vita::native(domain, tail)?);
    #[cfg(not(target_os = "vita"))]
    {
        let raw;
        if domain == [46] {
            #[cfg(windows)]
            {
                if tail.len() < 3 || !(97..=122).contains(&tail[1]) || tail[2] != 47 {
                    return Err(Error::Name("missing Windows drive"));
                }
                let mut drive = Vec::with_capacity(tail.len());
                drive.extend_from_slice(&[tail[1], 58]);
                drive.extend_from_slice(&tail[2..]);
                raw = drive;
            }
            #[cfg(not(windows))]
            {
                raw = tail.to_vec();
            }
        } else {
            #[cfg(windows)]
            {
                let mut unc = Vec::with_capacity(2 + domain.len() + tail.len());
                unc.extend_from_slice(&[47, 47]);
                unc.extend_from_slice(domain);
                unc.extend_from_slice(tail);
                raw = unc;
            }
            #[cfg(not(windows))]
            {
                return Err(Error::Name("remote file domain requires a host mount"));
            }
        }
        #[cfg(windows)]
        let raw: Vec<_> = raw
            .into_iter()
            .map(|u| if u == 47 { 92 } else { u })
            .collect();
        path(&raw)
    }
}
pub fn resolve(path: &Path) -> Result<PathBuf> {
    if path.try_exists()? {
        return Ok(path.to_owned());
    }
    resolve_missing(path)
}
fn resolve_missing(path: &Path) -> Result<PathBuf> {
    #[cfg(target_os = "vita")]
    let timer = krkr_protocol::diagnostics::Timer::start();
    let result = resolve_missing_inner(path);
    #[cfg(target_os = "vita")]
    timer.report(|| format!("stage=local-resolve path={}", path.display()));
    result
}
fn resolve_missing_inner(path: &Path) -> Result<PathBuf> {
    // Start at the nearest existing ancestor. A missing optional directory
    // (temp/, #patch/, ...) must not stat every component from the drive root.
    let mut out = PathBuf::new();
    let mut remaining = path;
    for ancestor in path
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
    {
        if ancestor.try_exists()? {
            out = ancestor.to_owned();
            remaining = path.strip_prefix(ancestor).unwrap();
            break;
        }
    }
    let mut components = remaining.components();
    while let Some(component) = components.next() {
        match component {
            Component::Normal(want) => {
                // A missing leaf must not trigger directory enumeration for
                // every existing ancestor (drive, home, game directory, ...).
                // Use the host's exact path lookup before case-fold fallback.
                let exact = out.join(want);
                if exact.try_exists()? {
                    out = exact;
                    continue;
                }
                let target = name::fold(&units(Path::new(want))?);
                let mut found = None;
                match read_dir(&out) {
                    Ok(entries) => {
                        for entry in entries {
                            let entry = entry?;
                            if name::fold(&units(Path::new(&entry.file_name()))?) == target {
                                if found.is_some() {
                                    return Err(Error::Name("ambiguous case-folded host path"));
                                }
                                found = Some(entry.file_name());
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
                if let Some(found) = found {
                    out.push(found);
                } else {
                    out.push(want);
                    // An absent directory also makes its normal descendants
                    // absent. Preserve parent components for general callers.
                    if components
                        .clone()
                        .all(|part| matches!(part, Component::Normal(_)))
                    {
                        out.extend(components);
                        return Ok(out);
                    }
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Ok(out)
}

/// Resolve spelling and obtain metadata in one host lookup on the common path.
pub fn metadata(path: &Path) -> Result<(PathBuf, fs::Metadata)> {
    match stat(path) {
        Ok(metadata) => Ok((path.to_owned(), metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let path = resolve_missing(path)?;
            let metadata = stat(&path)?;
            Ok((path, metadata))
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn stat(path: &Path) -> std::io::Result<fs::Metadata> {
    #[cfg(target_os = "vita")]
    let timer = krkr_protocol::diagnostics::Timer::start();
    let result = fs::metadata(path);
    #[cfg(target_os = "vita")]
    timer.report(|| format!("stage=local-stat path={}", path.display()));
    result
}

pub(crate) fn read_dir(path: &Path) -> std::io::Result<fs::ReadDir> {
    fs::read_dir(path).map_err(|error| directory_error(path, error))
}

fn directory_error(path: &Path, error: std::io::Error) -> std::io::Error {
    // Vita newlib's O_DIRECTORY check returns ENOTDIR even when stat failed
    // because the directory is absent. Preserve genuine errors; only a fresh
    // NotFound stat permits caching this directory as empty.
    if error.kind() == std::io::ErrorKind::NotADirectory
        && let Err(missing) = stat(path)
        && missing.kind() == std::io::ErrorKind::NotFound
    {
        return missing;
    }
    error
}

#[cfg(test)]
#[path = "../tests/local/internal.rs"]
mod tests;

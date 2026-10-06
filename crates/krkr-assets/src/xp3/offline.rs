//! Offline XP3 workflows. Sources are read-only; publish only complete output
//! to a new path. Filters are supplied by the caller, without a VM dependency.
use super::{Archive, Compression, Entry, FilterFactory, Reader, Version, Writer};
use crate::{Error, Limits, Result, local, name};
#[cfg(feature = "parallel")]
use rayon::prelude::*;
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Default)]
pub struct Summary {
    pub files: usize,
    pub bytes: u64,
}

/// Notifications may arrive concurrently when parallel extraction is enabled.
pub enum Progress<'a> {
    Started { files: usize },
    File { name: &'a [u16], bytes: u64 },
}
impl Summary {
    fn add(&mut self, bytes: u64) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(Error::Limit("total bytes"))?;
        self.files += 1;
        Ok(())
    }
}

use super::portable::portable_name;
fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", path.display()),
        )
        .into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
fn destination(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let parent = absolute
        .parent()
        .ok_or(Error::Name("output has no parent"))?;
    let file = absolute
        .file_name()
        .ok_or(Error::Name("output requires a file or directory name"))?;
    let path = fs::canonicalize(parent)?.join(file);
    absent(&path)?;
    Ok(path)
}
fn context(name: &[u16], error: impl std::fmt::Display) -> Error {
    io::Error::other(format!("{}: {error}", String::from_utf16_lossy(name))).into()
}
fn local_path(name: &[u16]) -> Result<PathBuf> {
    // Names have already passed portable_name, including platform separators.
    let text = String::from_utf16(name).map_err(|_| Error::Name("invalid UTF-16 XP3 name"))?;
    Ok(text.split('/').collect())
}
fn check_hierarchy<'a>(names: impl Iterator<Item = &'a [u16]>) -> Result<()> {
    let names: std::collections::BTreeSet<_> = names.collect();
    for name in &names {
        for at in name
            .iter()
            .enumerate()
            .filter_map(|(i, &u)| (u == 47).then_some(i))
        {
            if names.contains(&name[..at]) {
                return Err(context(name, "entry is both a file and a parent directory"));
            }
        }
    }
    Ok(())
}
fn linked(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0 // Includes junctions/reparse points.
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// Recursively pack regular files; empty directories are not XP3 entries.
/// Collect inputs before creating the temporary archive, even for in-tree output.
pub fn pack_directory(
    source: &Path,
    output: &Path,
    compression: Compression,
    limits: Limits,
) -> Result<Summary> {
    pack_directory_with_progress(source, output, compression, limits, &|_| {})
}

pub fn pack_directory_with_progress(
    source: &Path,
    output: &Path,
    compression: Compression,
    limits: Limits,
    progress: &(impl Fn(Progress<'_>) + Sync),
) -> Result<Summary> {
    let output = destination(output)?;
    let root = fs::canonicalize(source)?;
    if !root.is_dir() {
        return Err(Error::Name("pack source must be a directory"));
    }
    let mut pending = vec![root.clone()];
    let mut files = BTreeMap::new();
    let mut visited = 0usize;
    let mut index_bytes = 0usize;
    while let Some(directory) = pending.pop() {
        for item in fs::read_dir(directory)? {
            let path = item?.path();
            let meta = fs::symlink_metadata(&path)?;
            if linked(&meta) || (!meta.is_dir() && !meta.is_file()) {
                return Err(io::Error::other(format!(
                    "not a regular file/directory: {}",
                    path.display()
                ))
                .into());
            }
            visited += 1;
            if visited > limits.max_entries {
                return Err(Error::Limit("source file/directory count"));
            }
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| Error::Name("source escapes root"))?;
            let text = relative
                .to_str()
                .ok_or(Error::Name("source path is not Unicode"))?;
            let name =
                portable_name(&name::units(text)).map_err(|e| context(&name::units(text), e))?;
            if meta.is_dir() {
                pending.push(path);
            } else {
                index_bytes = index_bytes
                    .checked_add(102 + name.len() * 2)
                    .ok_or(Error::Limit("index"))?;
                if index_bytes > limits.max_index_bytes {
                    return Err(Error::Limit("index"));
                }
                if files
                    .insert(
                        name.clone(),
                        (
                            path,
                            Version {
                                bytes: meta.len(),
                                modified: meta.modified().ok(),
                            },
                        ),
                    )
                    .is_some()
                {
                    return Err(context(&name, "duplicate normalized source name"));
                }
            }
        }
    }
    check_hierarchy(files.keys().map(Vec::as_slice))?;
    progress(Progress::Started { files: files.len() });
    let mut temp =
        tempfile::NamedTempFile::new_in(output.parent().ok_or(Error::Name("output parent"))?)?;
    let mut writer = Writer::new(temp.as_file_mut(), compression, limits)?;
    let mut summary = Summary::default();
    for (name, (path, version)) in files {
        let mut file = File::open(&path)?;
        version.check(&file)?;
        let bytes = writer
            .add(&name, &mut file)
            .map_err(|e| context(&name, e))?;
        version.check(&file)?;
        if bytes != version.bytes {
            return Err(Error::Changed);
        }
        summary.add(bytes)?;
        progress(Progress::File { name: &name, bytes });
    }
    writer.finish()?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output)
        .map_err(|e| Error::Io(e.error))?;
    Ok(summary)
}

fn load(source: &Path, limits: Limits) -> Result<Arc<Archive>> {
    let archive = Arc::new(Archive::load_strict(&std::path::absolute(source)?, limits)?);
    check_hierarchy(archive.entries.keys())?;
    Ok(archive)
}
fn reader(
    archive: &Arc<Archive>,
    entry: &Arc<Entry>,
    factory: Option<&dyn FilterFactory>,
    limits: Limits,
) -> Result<Box<dyn Read>> {
    let outer = name::normalize(&local::units(&archive.path)?, &name::units("file://./"))?;
    let storage = [outer, vec![62], entry.name.clone()].concat();
    let filter = factory.map(|f| f.create(&storage, entry)).transpose()?;
    let full = filter.as_ref().is_some_and(|f| f.fetch_full_data());
    let mut reader = Reader::new(archive.clone(), entry.clone(), filter);
    if full {
        let size = crate::binary::size(entry.size, limits.max_read_bytes, "filtered stream bytes")?;
        let mut bytes = vec![0; size];
        reader.read_exact(&mut bytes)?;
        Ok(Box::new(io::Cursor::new(bytes)))
    } else {
        Ok(Box::new(reader))
    }
}

pub fn unpack_archive(
    source: &Path,
    output: &Path,
    filter: Option<&dyn FilterFactory>,
    limits: Limits,
) -> Result<Summary> {
    unpack_archive_with_progress(source, output, filter, limits, &|_| {})
}

pub fn unpack_archive_with_progress(
    source: &Path,
    output: &Path,
    filter: Option<&dyn FilterFactory>,
    limits: Limits,
    progress: &(impl Fn(Progress<'_>) + Sync),
) -> Result<Summary> {
    let output = destination(output)?;
    let archive = load(source, limits)?;
    let temp = tempfile::Builder::new()
        .prefix(".krkr-unpack-")
        .tempdir_in(output.parent().ok_or(Error::Name("output parent"))?)?;
    let entries: Vec<_> = archive.entries.values().collect();
    progress(Progress::Started {
        files: entries.len(),
    });
    let extract = |entry: &Arc<Entry>| -> Result<u64> {
        let path = temp.path().join(local_path(&entry.name)?);
        fs::create_dir_all(path.parent().ok_or(Error::Name("entry parent"))?)?;
        let mut stream =
            reader(&archive, entry, filter, limits).map_err(|e| context(&entry.name, e))?;
        let mut file = File::options().write(true).create_new(true).open(path)?;
        let (bytes, hash) =
            super::writer::copy(&mut stream, &mut file).map_err(|e| context(&entry.name, e))?;
        if bytes != entry.size {
            return Err(Error::Changed);
        }
        // The protected bit is an extraction policy flag, not an encryption
        // algorithm. Some plaintext archives retain it. Without a decoder only
        // accept marked entries whose stored Adler-32 matches the actual data.
        // With a decoder, adlr may instead be key material and must not be used
        // as a plaintext checksum.
        if filter.is_none() && entry.protected && hash != entry.hash {
            return Err(context(
                &entry.name,
                "protected XP3 entry failed Adler-32 verification; data may be encrypted or corrupt; supply --xp3-filter <script> for encrypted data",
            ));
        }
        progress(Progress::File {
            name: &entry.name,
            bytes,
        });
        Ok(bytes)
    };
    #[cfg(feature = "parallel")]
    let results: Vec<_> = entries.par_iter().map(extract).collect();
    #[cfg(not(feature = "parallel"))]
    let results: Vec<_> = entries.iter().map(extract).collect();
    let mut summary = Summary::default();
    for bytes in results {
        summary.add(bytes?)?;
    }
    archive.version.check(&File::open(&archive.path)?)?;
    absent(&output)?;
    fs::rename(temp.path(), &output)?;
    // The guard still owns only the old temporary path and cannot delete output.
    Ok(summary)
}

/// Filter every stream and rebuild a standard unprotected archive. Input `adlr`
/// values are filter metadata, not universally plaintext checksums; output hashes
/// are freshly computed Adler-32. Unknown index chunks/EXE wrappers are not copied.
pub fn decrypt_archive(
    source: &Path,
    output: &Path,
    filter: &dyn FilterFactory,
    compression: Compression,
    limits: Limits,
) -> Result<Summary> {
    decrypt_archive_with_progress(source, output, filter, compression, limits, &|_| {})
}

pub fn decrypt_archive_with_progress(
    source: &Path,
    output: &Path,
    filter: &dyn FilterFactory,
    compression: Compression,
    limits: Limits,
    progress: &(impl Fn(Progress<'_>) + Sync),
) -> Result<Summary> {
    let output = destination(output)?;
    let archive = load(source, limits)?;
    progress(Progress::Started {
        files: archive.entries.len(),
    });
    let mut temp =
        tempfile::NamedTempFile::new_in(output.parent().ok_or(Error::Name("output parent"))?)?;
    let mut writer = Writer::new(temp.as_file_mut(), compression, limits)?;
    let mut summary = Summary::default();
    for entry in archive.entries.values() {
        let mut stream =
            reader(&archive, &entry, Some(filter), limits).map_err(|e| context(&entry.name, e))?;
        let bytes = writer
            .add(&entry.name, &mut stream)
            .map_err(|e| context(&entry.name, e))?;
        if bytes != entry.size {
            return Err(Error::Changed);
        }
        summary.add(bytes)?;
        progress(Progress::File {
            name: &entry.name,
            bytes,
        });
    }
    archive.version.check(&File::open(&archive.path)?)?;
    writer.finish()?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(output)
        .map_err(|e| Error::Io(e.error))?;
    Ok(summary)
}

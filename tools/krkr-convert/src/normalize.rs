//! Normalize names from detected content, retaining script names as VFS links.
//! This stage only operates on the helper's private transactional copy.
use crate::{
    adjust, files,
    media::{self, Consistency, Media, Result, Tools, at, hash},
};
use krkr_assets::converted;
use serde::Serialize;
use std::{collections::BTreeSet, fs, path::Path};

#[derive(Clone, Debug, Serialize)]
pub struct Link {
    pub source: String,
    pub target: String,
}

pub(crate) struct Outcome {
    pub links: Vec<Link>,
    pub repairs: Vec<adjust::Adjustment>,
}

pub(crate) fn read_links(inventory: &media::Report) -> Result<Vec<Link>> {
    inventory
        .entries
        .iter()
        .filter_map(|entry| {
            entry
                .path
                .strip_suffix(converted::LINK_SUFFIX)
                .map(|source| (entry, source))
        })
        .map(|(entry, source)| {
            if entry.source_bytes > converted::MAX_LINK_BYTES as u64 {
                return Err("resource link exceeds size limit".into());
            }
            let path = files::resource(&inventory.root, &entry.path)?;
            let bytes = fs::read(&path).map_err(|e| at(&path, e))?;
            let leaf = converted::decode_link(&bytes).map_err(|e| at(&path, e))?;
            let target = Path::new(source)
                .with_file_name(leaf)
                .to_string_lossy()
                .replace('\\', "/");
            Ok(Link {
                source: source.to_owned(),
                target,
            })
        })
        .collect()
}

/// Resolve explicitly selected logical names without renaming or hashing media again.
pub(crate) fn matching_aliases(
    inventory: &media::Report,
    patterns: &[glob::Pattern],
) -> Result<BTreeSet<String>> {
    if patterns.is_empty() {
        return Ok(BTreeSet::new());
    }
    let links = read_links(inventory)?;
    let redirects = links
        .iter()
        .map(|l| (l.source.to_ascii_lowercase(), l.target.clone()))
        .collect();
    links
        .iter()
        .filter(|l| patterns.iter().any(|p| p.matches(&l.source)))
        .map(|l| crate::psv::follow(&l.source, &redirects).map(|s| s.to_ascii_lowercase()))
        .collect()
}

fn extension(info: &Media) -> Result<&str> {
    Ok(match info.container.as_str() {
        "jpeg" => "jpg",
        "ktx" => "ktx",
        "at9" => "at9",
        "tiff" => "tif",
        "tlg" => match info.detail.as_deref() {
            Some("tlg6") => "tlg6",
            _ => "tlg",
        },
        "mp4" if info.kind == "audio" => "m4a",
        "ogg" if info.kind == "video" => "ogv",
        "ogg" => {
            if info
                .tracks
                .iter()
                .filter(|t| t.kind == "audio")
                .all(|t| t.codec == "vorbis")
            {
                "ogg"
            } else if info
                .tracks
                .iter()
                .filter(|t| t.kind == "audio")
                .all(|t| t.codec == "opus")
            {
                "opus"
            } else {
                "ogx"
            }
        }
        "matroska" if info.kind == "audio" => "mka",
        "matroska" => "mkv",
        "mpeg" => "mpg",
        "ajpm" => "amv",
        value @ ("png" | "bmp" | "webp" | "gif" | "avif" | "psd" | "ico" | "cur" | "wav"
        | "flac" | "mp3" | "mp2" | "aac" | "aiff" | "tcwf" | "mp4" | "mov" | "avi"
        | "asf" | "webm") => value,
        _ => {
            return Err(format!(
                "no canonical extension for {} / {}",
                info.kind, info.container
            ));
        }
    })
}

pub(crate) fn marker_entry(root: &Path, path: String) -> Result<media::Entry> {
    let full = files::resource(root, &path)?;
    Ok(media::Entry {
        extension: media::extension(&full),
        expected_format: None,
        source_bytes: fs::metadata(&full).map_err(|e| at(&full, e))?.len(),
        source_sha256: hash(&full)?,
        media: None,
        consistency: Consistency::Unknown,
        path,
    })
}

/// Reserve logical names and directory components as well as physical files.
pub(crate) fn occupied(inventory: &media::Report) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for entry in &inventory.entries {
        let mut path = entry.path.as_str();
        names.insert(path.to_ascii_lowercase());
        if let Some(source) = path.strip_suffix(converted::LINK_SUFFIX) {
            names.insert(source.to_ascii_lowercase());
        }
        while let Some((parent, _)) = path.rsplit_once('/') {
            names.insert(parent.to_ascii_lowercase());
            path = parent;
        }
    }
    names
}

pub(crate) fn reserve(source: &str, ext: &str, occupied: &mut BTreeSet<String>) -> Result<String> {
    let mut target = Path::new(source)
        .with_extension(ext)
        .to_string_lossy()
        .replace('\\', "/");
    if occupied.contains(&target.to_ascii_lowercase()) {
        target = format!("{source}.{ext}");
        let mut index = 1;
        while occupied.contains(&target.to_ascii_lowercase()) {
            target = format!("{source}.krkr-{index}.{ext}");
            index += 1;
        }
    }
    converted::encode_link(
        Path::new(&target)
            .file_name()
            .unwrap()
            .to_str()
            .ok_or("non-UTF-8 resource name")?,
    )
    .map_err(|e| e.to_string())?;
    occupied.insert(target.to_ascii_lowercase());
    Ok(target)
}

pub(crate) fn validate_links(inventory: &media::Report) -> Result<Vec<Link>> {
    let links = read_links(inventory)?;
    let mut vfs =
        krkr_assets::Vfs::new(&inventory.root, Default::default()).map_err(|e| e.to_string())?;
    for link in &links {
        vfs.plan(&krkr_assets::name::units(&link.source))
            .map_err(|e| format!("{}: {e}", link.source))?;
    }
    Ok(links)
}

pub(crate) fn apply(
    inventory: &mut media::Report,
    tools: &Tools,
    repair_metadata: bool,
) -> Result<Outcome> {
    let names: BTreeSet<_> = inventory
        .entries
        .iter()
        .map(|e| e.path.to_ascii_lowercase())
        .collect();
    if names.len() != inventory.entries.len() {
        return Err("resource names collide after case folding".into());
    }
    let mut occupied = occupied(inventory);
    let mut links = Vec::new();
    // Reserve every target and marker before moving anything. Existing logical
    // names stay reserved, including names whose physical files will move.
    for entry in &inventory.entries {
        let path = files::resource(&inventory.root, &entry.path)?;
        if hash(&path)? != entry.source_sha256 {
            return Err(at(&path, "source changed since probing"));
        }
        let Some(info) = &entry.media else { continue };
        if entry.consistency == Consistency::Unreadable {
            return Err(at(&path, "unreadable media"));
        }
        if entry.consistency == Consistency::Match {
            continue;
        }
        let ext = extension(info)?;
        let target = reserve(&entry.path, ext, &mut occupied)?;
        let marker = format!("{}{suffix}", entry.path, suffix = converted::LINK_SUFFIX);
        if !occupied.insert(marker.to_ascii_lowercase()) {
            return Err(format!("resource link already exists: {marker}"));
        }
        links.push(Link {
            source: entry.path.clone(),
            target,
        });
    }
    for link in &links {
        let source = files::resource(&inventory.root, &link.source)?;
        let target = inventory.root.join(&link.target);
        if fs::symlink_metadata(&target).is_ok() {
            return Err(at(&target, "normalization target already exists"));
        }
        fs::rename(&source, &target).map_err(|e| at(&source, e))?;
        let marker = format!("{}{}", link.source, converted::LINK_SUFFIX);
        let bytes = converted::encode_link(target.file_name().unwrap().to_str().unwrap())
            .map_err(|e| e.to_string())?;
        fs::write(inventory.root.join(&marker), bytes).map_err(|e| at(&target, e))?;
        let entry = inventory
            .entries
            .iter_mut()
            .find(|e| e.path == link.source)
            .unwrap();
        if hash(&target)? != entry.source_sha256 {
            return Err(at(&target, "source changed during normalization"));
        }
        entry.path = link.target.clone();
        entry.extension = media::extension(&target);
        entry.expected_format = media::expected(&entry.extension).map(|(_, f)| f.to_owned());
        entry.consistency = media::consistency(&entry.extension, entry.media.as_ref());
        if entry.consistency != Consistency::Match {
            return Err(at(&target, "canonical extension does not match media"));
        }
        inventory
            .entries
            .push(marker_entry(&inventory.root, marker)?);
    }
    let repairs = if repair_metadata {
        let selected = media::Report {
            version: inventory.version,
            root: inventory.root.clone(),
            entries: inventory
                .entries
                .iter()
                .filter(|e| adjust::candidate(e))
                .cloned()
                .collect(),
        };
        let result = adjust::apply(&selected, tools, |_| {})?;
        if result.failed() {
            return Err(result
                .entries
                .iter()
                .filter(|e| e.status == "error")
                .map(|e| {
                    format!(
                        "{}: {}",
                        e.path,
                        e.detail.as_deref().unwrap_or("repair failed")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"));
        }
        for changed in result.entries.iter().filter(|e| e.status == "adjusted") {
            let entry = inventory
                .entries
                .iter_mut()
                .find(|e| e.path == changed.path)
                .unwrap();
            let path = files::resource(&inventory.root, &entry.path)?;
            entry.source_sha256 = changed.output_sha256.clone().ok_or("missing repair hash")?;
            entry.source_bytes = fs::metadata(&path).map_err(|e| at(&path, e))?.len();
        }
        result.entries
    } else {
        Vec::new()
    };
    // Includes input links on a second normalization pass. Check the graph,
    // sibling constraints and dangling targets without executing game scripts.
    validate_links(inventory)?;
    Ok(Outcome { links, repairs })
}

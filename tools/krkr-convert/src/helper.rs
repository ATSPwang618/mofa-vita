//! Transactional game preparation used by the interactive helper. Archive names
//! and boundaries survive conversion, preserving explicit script storage paths.
use crate::{archive, files, media, normalize, progress::Progress, psv};
use krkr_assets::{
    Limits,
    xp3::{Compression, offline},
};
use krkr_protocol::graphics::Size;
use media::{Consistency, Result, Tools, at};
use psv::Selection;
use rayon::prelude::*;
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub enum Target {
    Normalize,
    Psv(psv::Options),
}

impl Target {
    pub fn psv(canvas: Size) -> Self {
        Self::Psv(psv::Options::vita(canvas))
    }
}

pub struct HardwareCounts {
    pub at9: usize,
    pub textures: usize,
}

/// Patterns refer to original resource paths inside each XP3 or loose tree.
/// Existing resource links keep the original script names selectable.
fn select_hardware(
    inventories: &[media::Report],
    options: &psv::Options,
) -> Result<Vec<Selection>> {
    let mut selections = crate::texture_plan::candidates(inventories, options)?;
    for (selection, inventory) in selections.iter_mut().zip(inventories) {
        selection.audio = crate::at9::select_matching(inventory, options.at9.as_ref())?;
    }
    if options.at9.is_some() && selections.iter().all(|s| s.audio.is_empty()) {
        return Err(
            "AT9 patterns did not select any convertible audio resources in the game".into(),
        );
    }
    if !options.texture_auto
        && !options.texture_globs.is_empty()
        && selections.iter().all(|s| s.textures.is_empty())
    {
        return Err("texture patterns did not select any image resources in the game".into());
    }
    Ok(selections)
}

pub fn hardware_counts(
    inventories: &[media::Report],
    options: &psv::Options,
) -> Result<HardwareCounts> {
    let selections = select_hardware(inventories, options)?;
    Ok(HardwareCounts {
        at9: selections.iter().map(|s| s.audio.len()).sum(),
        textures: selections.iter().map(|s| s.textures.len()).sum(),
    })
}

/// Reject input that still needs normalization before starting PSV conversion.
/// This uses the existing inventory; it does not decode or rewrite resources.
pub fn require_normalized(inventories: &[media::Report]) -> Result<()> {
    for inventory in inventories {
        if let Some(entry) = inventory.entries.iter().find(|entry| {
            entry.consistency == Consistency::Unreadable
                || (entry.media.is_some() && entry.consistency != Consistency::Match)
        }) {
            return Err(format!(
                "请先归一化资源，再生成 PSV 游戏资源：{} / {}（需要修正格式或修复媒体）",
                inventory.root.display(),
                entry.path
            ));
        }
    }
    Ok(())
}

struct Part {
    path: PathBuf,
    archive: Option<String>,
}

pub struct Prepared {
    source: PathBuf,
    output: PathBuf,
    scratch: tempfile::TempDir,
    parts: Vec<Part>,
    filter_removed: bool,
}

#[derive(Serialize)]
pub struct Repair {
    pub path: String,
    pub detail: String,
}

#[derive(Serialize)]
pub struct PartReport {
    pub archive: Option<String>,
    pub files: usize,
    pub adjusted: usize,
    pub scaled_images: usize,
    pub converted_videos: usize,
    pub converted_audio: usize,
    pub converted_images: usize,
    pub hardware_audio: usize,
    pub compressed_textures: usize,
    pub transparent_textures: usize,
    pub packed_tiles: usize,
    pub native_bc_tiles: usize,
    pub texture_encoded_bytes: usize,
    pub texture_gpu_bytes: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub texture_skips: Vec<Repair>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<normalize::Link>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repairs: Vec<Repair>,
}

#[derive(Serialize)]
pub struct Report {
    pub version: u32,
    pub source: PathBuf,
    pub output: PathBuf,
    pub target: &'static str,
    pub canvas: Option<[u32; 2]>,
    pub filter_removed: bool,
    pub parts: Vec<PartReport>,
}

pub fn output_path(source: &Path, output: &Path) -> Result<PathBuf> {
    files::regular(source, true)?;
    let source = fs::canonicalize(source).map_err(|e| at(source, e))?;
    let output = std::path::absolute(output).map_err(|e| at(output, e))?;
    let parent = fs::canonicalize(output.parent().ok_or("output parent missing")?)
        .map_err(|e| at(&output, e))?;
    let output = parent.join(output.file_name().ok_or("output directory name missing")?);
    if output.starts_with(&source) || fs::symlink_metadata(&output).is_ok() {
        return Err(at(
            &output,
            "output must be a new directory outside the source tree",
        ));
    }
    Ok(output)
}

impl Prepared {
    pub fn new(
        source: &Path,
        output: &Path,
        filter: Option<&archive::FilterOptions>,
    ) -> Result<Self> {
        let output = output_path(source, output)?;
        let (source, entries) = media::collect(source)?;
        let mut names = BTreeSet::new();
        for name in entries.keys() {
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(format!("resource names collide after case folding: {name}"));
            }
        }
        let scratch = tempfile::Builder::new()
            .prefix(".krkr-helper-")
            .tempdir_in(output.parent().unwrap())
            .map_err(|e| at(&output, e))?;
        let loose = scratch.path().join("loose");
        fs::create_dir(&loose).map_err(|e| at(&loose, e))?;
        let mut archives = Vec::new();
        let mut copies = Vec::new();
        let mut filter_removed = false;
        for (name, path) in entries {
            if media::extension(&path) == "xp3" {
                archives.push((name, path));
            } else if filter.is_some() && name.eq_ignore_ascii_case("xp3filter.tjs") {
                // The automatic startup filter must not run on plaintext output.
                filter_removed = true;
            } else {
                copies.push((name, path));
            }
        }
        let bar = Progress::new("copy", Some(copies.len()));
        copies
            .par_iter()
            .map(|(name, source)| {
                let target = loose.join(name);
                fs::create_dir_all(target.parent().unwrap()).map_err(|e| at(&target, e))?;
                fs::copy(source, &target).map_err(|e| at(source, e))?;
                // Work only on our private copy, including read-only original assets.
                let mut permissions = fs::metadata(&target)
                    .map_err(|e| at(&target, e))?
                    .permissions();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    permissions.set_mode(permissions.mode() | 0o200);
                }
                #[cfg(windows)]
                {
                    // Windows has a read-only attribute, not Unix mode bits.
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                }
                fs::set_permissions(&target, permissions).map_err(|e| at(&target, e))?;
                bar.done(name);
                Ok(())
            })
            .collect::<Result<Vec<_>>>()?;
        drop(bar);
        let mut parts = vec![Part {
            path: loose,
            archive: None,
        }];
        // Each archive extractor already uses the shared file worker pool.
        for (index, (name, path)) in archives.into_iter().enumerate() {
            let output = scratch.path().join(format!("archive-{index}"));
            let provider = filter.map(|f| archive::provider(&path, f)).transpose()?;
            let bar = Progress::new(format!("unpack {name}"), None);
            offline::unpack_archive_with_progress(
                &path,
                &output,
                provider.as_deref(),
                Limits::default(),
                &|event| bar.archive(event),
            )
            .map_err(|e| at(&path, e))?;
            parts.push(Part {
                path: output,
                archive: Some(name),
            });
        }
        Ok(Self {
            source,
            output,
            scratch,
            parts,
            filter_removed,
        })
    }

    pub fn inspect(&self, tools: &Tools) -> Result<Vec<media::Report>> {
        self.parts
            .iter()
            .map(|part| media::inspect(&part.path, tools))
            .collect()
    }

    /// Only literal KAG assignments are candidates. Expressions and conflicts
    /// require an explicit choice; game scripts are never executed for detection.
    pub fn canvases(&self) -> Result<Vec<Size>> {
        let mut choices = BTreeSet::new();
        for part in &self.parts {
            let (_, files) = media::collect(&part.path)?;
            for path in files.values().filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.eq_ignore_ascii_case("config.tjs"))
            }) {
                if fs::metadata(path).map_err(|e| at(path, e))?.len() > 4 * 1024 * 1024 {
                    continue;
                }
                let bytes = fs::read(path).map_err(|e| at(path, e))?;
                let decoded = ["utf-8", "shift-jis"].into_iter().find_map(|encoding| {
                    krkr_assets::text::decode(
                        &bytes,
                        &krkr_assets::name::units(encoding),
                        4 * 1024 * 1024,
                    )
                    .ok()
                });
                if let Some(text) = decoded {
                    for size in canvas_literals(&String::from_utf16_lossy(&text)) {
                        choices.insert((size.width, size.height));
                    }
                }
            }
        }
        Ok(choices
            .into_iter()
            .map(|(width, height)| Size { width, height })
            .collect())
    }

    pub fn convert(
        self,
        inventories: Vec<media::Report>,
        target: Target,
        tools: &Tools,
    ) -> Result<Report> {
        if inventories.len() != self.parts.len() {
            return Err("helper inventory count changed".into());
        }
        let selections = match &target {
            Target::Psv(options) => {
                require_normalized(&inventories)?;
                select_hardware(&inventories, options)?
            }
            Target::Normalize => (0..inventories.len())
                .map(|_| Selection::default())
                .collect(),
        };
        let export = self.scratch.path().join("export");
        let mut reports = Vec::new();
        for (index, ((part, mut inventory), selection)) in self
            .parts
            .iter()
            .zip(inventories)
            .zip(selections)
            .enumerate()
        {
            if inventory.root != fs::canonicalize(&part.path).map_err(|e| at(&part.path, e))? {
                return Err("helper inventory root changed".into());
            }
            if let Some(entry) = inventory
                .entries
                .iter()
                .find(|e| e.consistency == Consistency::Unreadable)
            {
                return Err(format!(
                    "unreadable media: {} / {}",
                    part.archive.as_deref().unwrap_or("loose"),
                    entry.path
                ));
            }
            let original_files = inventory.entries.len();
            let normalized = match &target {
                Target::Normalize => normalize::apply(&mut inventory, tools, true)
                    .map_err(|e| format!("{}: {e}", part.archive.as_deref().unwrap_or("loose")))?,
                Target::Psv(_) => normalize::Outcome {
                    links: Vec::new(),
                    repairs: Vec::new(),
                },
            };
            let adjusted = normalized.repairs;
            let mut summary = PartReport {
                archive: part.archive.clone(),
                files: original_files,
                adjusted: normalized.links.len()
                    + adjusted.iter().filter(|e| e.status == "adjusted").count(),
                scaled_images: 0,
                converted_videos: 0,
                converted_audio: 0,
                converted_images: 0,
                hardware_audio: 0,
                compressed_textures: 0,
                transparent_textures: 0,
                packed_tiles: 0,
                native_bc_tiles: 0,
                texture_encoded_bytes: 0,
                texture_gpu_bytes: 0,
                texture_skips: Vec::new(),
                links: normalized.links,
                repairs: adjusted
                    .iter()
                    .filter(|e| e.status == "adjusted")
                    .filter_map(|e| {
                        e.detail.as_ref().map(|detail| Repair {
                            path: e.path.clone(),
                            detail: detail.clone(),
                        })
                    })
                    .collect(),
            };
            let converted = match &target {
                Target::Normalize => part.path.clone(),
                Target::Psv(options) => {
                    let destination = self.scratch.path().join(format!("converted-{index}"));
                    let result = psv::build_selected(
                        inventory,
                        Some(&destination),
                        options,
                        selection,
                        tools,
                        |_| {},
                    )?;
                    summary.scaled_images = result
                        .entries
                        .iter()
                        .filter(|e| {
                            e.action.starts_with("image_") && e.logical_size != e.stored_size
                        })
                        .count();
                    summary.converted_audio = result
                        .entries
                        .iter()
                        .filter(|e| e.action.starts_with("audio_"))
                        .count();
                    summary.converted_images = result
                        .entries
                        .iter()
                        .filter(|e| {
                            matches!(
                                e.action.as_str(),
                                "image_converted" | "image_bc1" | "image_bc3"
                            )
                        })
                        .count();
                    summary.hardware_audio = result
                        .entries
                        .iter()
                        .filter(|e| e.action == "audio_at9")
                        .count();
                    summary.compressed_textures = result
                        .entries
                        .iter()
                        .filter(|e| matches!(e.action.as_str(), "image_bc1" | "image_bc3"))
                        .count();
                    summary.transparent_textures = result
                        .entries
                        .iter()
                        .filter(|e| e.action == "image_bc3")
                        .count();
                    for storage in result
                        .entries
                        .iter()
                        .filter_map(|entry| entry.texture_storage.as_ref())
                    {
                        summary.packed_tiles += storage.packed_tiles;
                        summary.native_bc_tiles += storage.native_bc_tiles;
                        summary.texture_encoded_bytes += storage.encoded_bytes;
                        summary.texture_gpu_bytes += storage.gpu_bytes;
                    }
                    summary.texture_skips = result
                        .entries
                        .iter()
                        .filter_map(|e| {
                            e.texture_skip.as_ref().map(|reason| Repair {
                                path: e.source.clone(),
                                detail: reason.clone(),
                            })
                        })
                        .collect();
                    summary.links = result.links;
                    summary.converted_videos = result
                        .entries
                        .iter()
                        .filter(|e| e.action == "video_hardware_profile")
                        .count();
                    summary
                        .repairs
                        .extend(result.entries.iter().filter_map(|e| {
                            e.repair.as_ref().map(|detail| Repair {
                                path: e.source.clone(),
                                detail: detail.clone(),
                            })
                        }));
                    destination
                }
            };
            if let Some(name) = &part.archive {
                let destination = export.join(name);
                fs::create_dir_all(destination.parent().unwrap())
                    .map_err(|e| at(&destination, e))?;
                let bar = Progress::new(format!("pack {name}"), None);
                // Preserve direct media reads, but losslessly compress native
                // textures/text in bounded independently seekable segments.
                offline::pack_directory_with_progress(
                    &converted,
                    &destination,
                    if matches!(target, Target::Psv(_)) {
                        Compression::Auto
                    } else {
                        Compression::None
                    },
                    Limits::default(),
                    &|event| bar.archive(event),
                )
                .map_err(|e| at(&destination, e))?;
            } else {
                fs::rename(converted, &export).map_err(|e| at(&export, e))?;
            }
            reports.push(summary);
        }
        if fs::symlink_metadata(&self.output).is_ok() {
            return Err(at(&self.output, "output already exists"));
        }
        fs::rename(export, &self.output).map_err(|e| at(&self.output, e))?;
        Ok(Report {
            version: 1,
            source: self.source,
            output: self.output,
            target: if matches!(target, Target::Psv(_)) {
                "psv"
            } else {
                "normalize"
            },
            canvas: match target {
                Target::Psv(options) => Some([options.canvas.width, options.canvas.height]),
                _ => None,
            },
            filter_removed: self.filter_removed,
            parts: reports,
        })
    }
}

fn canvas_literals(text: &str) -> Vec<Size> {
    // Tokenize just enough to skip comments and quoted text and to reject RHS
    // expressions. This is deliberately not a TJS interpreter.
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
        } else if matches!(c, '\'' | '"') {
            while let Some(q) = chars.next() {
                if q == '\\' {
                    chars.next();
                } else if q == c {
                    break;
                }
            }
            tokens.push("<string>".to_owned());
        } else if c.is_ascii_alphanumeric() || c == '_' {
            let mut token = c.to_string();
            while chars
                .peek()
                .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
            {
                token.push(chars.next().unwrap());
            }
            tokens.push(token);
        } else {
            tokens.push(c.to_string());
        }
    }
    let mut widths = BTreeSet::new();
    let mut heights = BTreeSet::new();
    let mut ambiguous = false;
    for t in tokens.windows(4) {
        let values = match t[0].as_str() {
            "scWidth" => &mut widths,
            "scHeight" => &mut heights,
            _ => continue,
        };
        if t[1] == "=" {
            if t[3] == ";"
                && let Ok(n @ 1..=65535) = t[2].parse::<u32>()
            {
                values.insert(n);
            } else {
                ambiguous = true;
            }
        }
    }
    if !ambiguous && widths.len() == 1 && heights.len() == 1 {
        vec![Size {
            width: *widths.first().unwrap(),
            height: *heights.first().unwrap(),
        }]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn canvas_detection_ignores_comments_strings_and_expressions() {
        assert_eq!(
            canvas_literals(
                "// scWidth=999;\n/*scHeight=999;*/ var s='scWidth=3;'; ;scWidth = 1280; ;scHeight = 720;"
            ),
            [Size {
                width: 1280,
                height: 720
            }]
        );
        assert!(canvas_literals("scWidth=1280/2; scHeight=720;").is_empty());
        assert!(canvas_literals("scWidth=1280; scHeight=720; scWidth=1920;").is_empty());
    }
}

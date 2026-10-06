//! PSV assets retain script coordinates through per-resource metadata. Video
//! resources have real MP4 suffixes plus explicit VFS aliases for legacy names.
use crate::{
    adjust,
    media::{self, Result, Tools, at, hash, run},
};
use krkr_image::scale::Metadata;
use krkr_protocol::{budget::Budget, graphics::Size, pixels::Bytes};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone)]
pub struct Options {
    pub at9: Option<crate::at9::Options>,
    pub canvas: Size,
    pub target: Size,
    /// Explicit relative image patterns to encode as lossy BC1/BC3 assets.
    /// Empty preserves the lossless workflow unless texture_auto is enabled.
    pub texture_globs: Vec<String>,
    /// Automatically consider static images; unsafe candidates stay lossless.
    pub texture_auto: bool,
    /// Offline encoding effort; quality acceptance thresholds are unchanged.
    pub texture_quality: crate::bc::Quality,
    pub texture_storage: crate::bc::Storage,
}

impl Options {
    pub fn vita(canvas: Size) -> Self {
        Self {
            canvas,
            target: Size {
                width: 960,
                height: 544,
            },
            at9: None,
            texture_globs: Vec::new(),
            texture_auto: false,
            texture_quality: Default::default(),
            texture_storage: Default::default(),
        }
    }
}

pub(crate) fn select_textures(
    inventory: &media::Report,
    globs: &[String],
) -> Result<BTreeSet<String>> {
    let patterns = globs
        .iter()
        .map(|p| glob::Pattern::new(p).map_err(|e| format!("invalid texture pattern {p}: {e}")))
        .collect::<Result<Vec<_>>>()?;
    let aliases = crate::normalize::matching_aliases(inventory, &patterns)?;
    Ok(inventory
        .entries
        .iter()
        .filter(|e| e.media.as_ref().is_some_and(|m| m.kind == "image"))
        .filter(|e| {
            patterns.iter().any(|p| p.matches(&e.path))
                || aliases.contains(&e.path.to_ascii_lowercase())
        })
        .map(|e| e.path.clone())
        .collect())
}

#[derive(Default)]
pub(crate) struct Selection {
    pub audio: BTreeSet<String>,
    pub textures: BTreeSet<String>,
    pub texture_skips: BTreeMap<String, String>,
}
#[derive(Serialize)]
pub struct TextureStorage {
    pub packed_tiles: usize,
    pub native_bc_tiles: usize,
    pub encoded_bytes: usize,
    pub gpu_bytes: usize,
}
#[derive(Serialize)]
pub struct Entry {
    pub source: String,
    pub output: String,
    pub action: String,
    pub logical_size: Option<[u32; 2]>,
    pub stored_size: Option<[u32; 2]>,
    pub sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture_skip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture_storage: Option<TextureStorage>,
}
#[derive(Serialize)]
pub struct Report {
    pub version: u32,
    pub source: PathBuf,
    pub output: PathBuf,
    pub canvas: [u32; 2],
    pub target: [u32; 2],
    pub video_profile: &'static str,
    pub links: Vec<crate::normalize::Link>,
    pub entries: Vec<Entry>,
    /// Sum of worker durations, not wall time; parallel stages overlap.
    pub texture_timings: crate::bc::Timings,
}

fn audio_supported(info: &media::Media) -> bool {
    // This status comes from the shared engine decoder, without a desktop
    // FFmpeg backend. TCWF has its existing portable plugin implementation.
    info.status == "builtin_header" || matches!(info.container.as_str(), "tcwf" | "at9")
}

fn image_supported(info: &media::Media) -> bool {
    matches!(
        info.container.as_str(),
        "png" | "jpeg" | "bmp" | "webp" | "tlg" | "psd" | "ico" | "cur" | "ktx" | "kbct"
    )
}

fn province(path: &str) -> bool {
    Path::new(path)
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().to_ascii_lowercase().ends_with("_p"))
}

pub(crate) fn follow(source: &str, redirects: &BTreeMap<String, String>) -> Result<String> {
    let mut target = source.to_owned();
    let mut visited = BTreeSet::new();
    for _ in 0..=krkr_assets::converted::MAX_LINK_DEPTH {
        if !visited.insert(target.to_ascii_lowercase()) {
            return Err(format!("cyclic resource link: {source}"));
        }
        match redirects.get(&target.to_ascii_lowercase()) {
            Some(next) => target = next.clone(),
            None => return Ok(target),
        }
    }
    Err(format!("resource link depth exceeded: {source}"))
}

fn verify_audio(
    source: &Path,
    output: &Path,
    before: &media::Media,
    ext: &str,
    tools: &Tools,
) -> Result<()> {
    let after = media::inspect_file(output, tools)?.ok_or("encoded output has no audio")?;
    if after.kind != "audio"
        || after.tracks.len() != 1
        || !audio_supported(&after)
        || media::consistency(ext, Some(&after)) != media::Consistency::Match
        || after.tracks[0].sample_rate != before.tracks[0].sample_rate
        || after.tracks[0].channels != before.tracks[0].channels
    {
        return Err(at(
            source,
            "audio conversion changed stream count, sample rate or channels, or is not supported by the PSV decoder",
        ));
    }
    Ok(())
}
pub fn dimensions(text: &str) -> Result<Size> {
    let (w, h) = text.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT")?;
    let size = Size {
        width: w.parse().map_err(|_| "invalid width")?,
        height: h.parse().map_err(|_| "invalid height")?,
    };
    if size.width == 0 || size.height == 0 || size.width > 65535 || size.height > 65535 {
        return Err("dimensions must be between 1 and 65535".into());
    }
    Ok(size)
}
fn scaled(size: Size, ratio: f64) -> Size {
    Size {
        width: (f64::from(size.width) * ratio).round().max(1.0) as u32,
        height: (f64::from(size.height) * ratio).round().max(1.0) as u32,
    }
}

// Preserve fine detail and integer sprite-cell boundaries in inexpensive
// images. This is a bounded quality policy, not an inference that a filename
// or a narrow image necessarily belongs to the UI. Full-canvas images and
// large atlases continue to use the ordinary display-density conversion.
fn retain_small_image(size: Size, canvas: Size) -> bool {
    let pixels = u64::from(size.width) * u64::from(size.height);
    size.width.min(size.height) < 64
        && size.width.max(size.height) <= 1024
        && pixels <= 16 * 1024
        && pixels.saturating_mul(16) <= u64::from(canvas.width) * u64::from(canvas.height)
}

/// Area filtering in premultiplied alpha avoids dark fringes on cutout sprites.
fn shrink(source: &Bytes, input: Size, output: Size, budget: &Budget) -> Result<Bytes> {
    let mut pixels = Bytes::zeroed(output.rgba_bytes().ok_or("image size overflow")?, budget)
        .map_err(|e| e.to_string())?;
    let rx = f64::from(input.width) / f64::from(output.width);
    let ry = f64::from(input.height) / f64::from(output.height);
    for y in 0..output.height {
        let (top, bottom) = (f64::from(y) * ry, f64::from(y + 1) * ry);
        for x in 0..output.width {
            let (left, right) = (f64::from(x) * rx, f64::from(x + 1) * rx);
            let mut sum = [0.0; 4];
            let mut weight = 0.0;
            for sy in top.floor() as u32..(bottom.ceil() as u32).min(input.height) {
                let wy = bottom.min(f64::from(sy + 1)) - top.max(f64::from(sy));
                for sx in left.floor() as u32..(right.ceil() as u32).min(input.width) {
                    let w = wy * (right.min(f64::from(sx + 1)) - left.max(f64::from(sx)));
                    let at = (sy as usize * input.width as usize + sx as usize) * 4;
                    let p = &source.as_slice()[at..at + 4];
                    for c in 0..3 {
                        sum[c] += f64::from(p[c]) * f64::from(p[3]) * w;
                    }
                    sum[3] += f64::from(p[3]) * w;
                    weight += w;
                }
            }
            let at = (y as usize * output.width as usize + x as usize) * 4;
            let p = &mut pixels.as_mut_slice()[at..at + 4];
            for c in 0..3 {
                p[c] = if sum[3] == 0.0 {
                    0
                } else {
                    (sum[c] / sum[3]).round() as u8
                };
            }
            p[3] = (sum[3] / weight).round() as u8;
        }
    }
    Ok(pixels)
}
fn companion(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap().to_owned();
    name.push(suffix);
    path.with_file_name(name)
}
fn metadata(path: &Path, logical: Size, stored: Size) -> Result<()> {
    let bytes = Metadata { logical, stored }
        .encode()
        .map_err(|e| e.to_string())?;
    fs::write(companion(path, krkr_image::scale::SUFFIX), bytes).map_err(|e| at(path, e))
}
fn video(
    source: &Path,
    output: &Path,
    info: &media::Media,
    size: Size,
    tools: &Tools,
) -> Result<()> {
    if info.tracks.iter().filter(|t| t.kind == "video").count() != 1
        || info.tracks.iter().filter(|t| t.kind == "audio").count() > 1
        || info
            .tracks
            .iter()
            .any(|t| !matches!(t.kind.as_str(), "video" | "audio"))
    {
        return Err(at(
            source,
            "PSV profile requires one video track and at most one audio track",
        ));
    }
    let track = info.tracks.iter().find(|t| t.kind == "video").unwrap();
    if track
        .pixel_format
        .as_deref()
        .is_some_and(|p| p.starts_with("yuva") || matches!(p, "rgba" | "bgra" | "argb" | "abgr"))
    {
        return Err(at(
            source,
            "alpha video needs a separate alpha layout; opaque AVC conversion would lose transparency",
        ));
    }
    let fps = track
        .frame_rate
        .as_deref()
        .and_then(|s| s.split_once('/'))
        .and_then(|(n, d)| Some(n.parse::<f64>().ok()? / d.parse::<f64>().ok()?));
    if fps.is_none_or(|n| !n.is_finite() || n <= 0.0 || n > 60.0) {
        return Err(at(
            source,
            "PSV profile requires a known frame rate of at most 60 fps",
        ));
    }
    let encoder = media::h264(tools)?;
    run(media::ffmpeg(tools, source)?
        .args(encoder)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-map_metadata",
            "0",
            "-vf",
            &format!(
                "scale={}:{}:flags=lanczos:out_color_matrix=bt601:out_range=tv,setsar=1",
                size.width, size.height
            ),
            "-profile:v",
            "main",
            "-level:v",
            "4.0",
            "-pix_fmt",
            "yuv420p",
            "-colorspace",
            "smpte170m",
            "-color_range",
            "tv",
            "-maxrate",
            "8M",
            "-bufsize",
            "8M",
            "-refs",
            "3",
            "-g",
            "60",
            "-fps_mode",
            "passthrough",
            "-c:a",
            "aac",
            "-profile:a",
            "aac_low",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ])
        .arg(media::tool_path(output)?))?;
    let after = media::probe(tools, output)?;
    if after.container != "mp4"
        || after.width != Some(size.width)
        || after.height != Some(size.height)
        || after.tracks.iter().any(|t| match t.kind.as_str() {
            "video" => {
                t.codec != "h264"
                    || t.profile.as_deref() != Some("Main")
                    || t.level.is_none_or(|n| n == 0 || n > 40)
                    || t.pixel_format.as_deref() != Some("yuv420p")
            }
            "audio" => {
                t.codec != "aac"
                    || t.profile.as_deref() != Some("LC")
                    || t.sample_rate != Some(48000)
                    || t.channels != Some(2)
            }
            _ => true,
        })
    {
        return Err(at(output, "encoded video failed PSV profile verification"));
    }
    if after.tracks.len() != info.tracks.len() {
        return Err(at(output, "conversion changed stream or frame counts"));
    }
    // Count output frames during the mandatory full decode verification rather
    // than decoding the output once with ffprobe and again with ffmpeg.
    let verified = run(media::ffmpeg(tools, output)?.args([
        "-map",
        "0",
        "-fps_mode",
        "passthrough",
        "-progress",
        "pipe:1",
        "-stats_period",
        "3600",
        "-f",
        "null",
        "-",
    ]))?;
    let verified = String::from_utf8_lossy(&verified);
    let frames = verified
        .lines()
        .filter_map(|line| line.strip_prefix("frame="))
        .next_back()
        .and_then(|n| n.trim().parse::<u64>().ok());
    if !verified.lines().any(|line| line == "progress=end")
        || frames != Some(frame_count(source, tools)?)
    {
        return Err(at(output, "conversion changed video frame count"));
    }
    Ok(())
}
fn frame_count(path: &Path, tools: &Tools) -> Result<u64> {
    let bytes = run(Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "json",
            "-i",
        ])
        .arg(media::tool_path(path)?))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| at(path, e))?;
    value["streams"][0]["nb_read_frames"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| at(path, "cannot verify video frame count"))
}

pub fn build(
    source: &Path,
    output: Option<&Path>,
    options: &Options,
    tools: &Tools,
    progress: impl FnMut(&str) + Send,
) -> Result<Report> {
    crate::files::regular(source, true)?;
    let inventory = media::inspect(source, tools)?;
    build_inspected(inventory, output, options, tools, progress)
}

/// Reuse a fresh inventory across helper stages; source hashes are still checked
/// before and after each conversion, including inventories updated by adjust.
pub(crate) fn build_inspected(
    inventory: media::Report,
    output: Option<&Path>,
    options: &Options,
    tools: &Tools,
    progress: impl FnMut(&str) + Send,
) -> Result<Report> {
    let mut selection =
        crate::texture_plan::candidates(std::slice::from_ref(&inventory), options)?.remove(0);
    if !options.texture_globs.is_empty() && !options.texture_auto && selection.textures.is_empty() {
        return Err("--texture-glob did not select any image resources".into());
    }
    selection.audio = crate::at9::select(&inventory, options.at9.as_ref())?;
    build_selected(inventory, output, options, selection, tools, progress)
}

/// The helper resolves selection before normalization, then remaps exact names.
/// Passing sets avoids comparing every file with thousands of literal globs.
pub(crate) fn build_selected(
    inventory: media::Report,
    output: Option<&Path>,
    options: &Options,
    selection: Selection,
    tools: &Tools,
    progress: impl FnMut(&str) + Send,
) -> Result<Report> {
    use rayon::prelude::*;
    let progress = std::sync::Mutex::new(progress);
    if options.canvas.width == 0
        || options.canvas.height == 0
        || options.canvas.width > 65535
        || options.canvas.height > 65535
    {
        return Err("source canvas dimensions must be between 1 and 65535".into());
    }
    if options.target.width > 960
        || options.target.height > 544
        || options.target.width < 2
        || options.target.height < 2
    {
        return Err("PSV target must fit within 960x544".into());
    }
    let output = output
        .map(Path::to_owned)
        .unwrap_or_else(|| companion(&inventory.root, "-psv"));
    let output = std::path::absolute(output).map_err(|e| e.to_string())?;
    let parent = fs::canonicalize(output.parent().ok_or("output parent missing")?)
        .map_err(|e| at(&output, e))?;
    let output = parent.join(output.file_name().ok_or("output filename missing")?);
    if output.starts_with(&inventory.root) || output.exists() {
        return Err(at(
            &output,
            "output must be a new directory outside the source tree",
        ));
    }
    let ratio = (f64::from(options.target.width) / f64::from(options.canvas.width))
        .min(f64::from(options.target.height) / f64::from(options.canvas.height))
        .min(1.0);
    if !ratio.is_finite() || ratio <= 0.0 {
        return Err("invalid source canvas".into());
    }
    let names: BTreeSet<_> = inventory
        .entries
        .iter()
        .map(|e| e.path.to_ascii_lowercase())
        .collect();
    let mut links = crate::normalize::validate_links(&inventory)?;
    let redirects: BTreeMap<_, _> = links
        .iter()
        .map(|l| (l.source.to_ascii_lowercase(), l.target.clone()))
        .collect();
    let province_targets: BTreeSet<_> = links
        .iter()
        .filter(|link| province(&link.source))
        .map(|link| follow(&link.target, &redirects).map(|p| p.to_ascii_lowercase()))
        .collect::<Result<_>>()?;
    let is_mask = |path: &str| {
        Path::new(path)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("_m")
    };
    let mask_targets: BTreeSet<_> = links
        .iter()
        .filter(|link| is_mask(&link.source))
        .map(|link| follow(&link.target, &redirects).map(|p| p.to_ascii_lowercase()))
        .collect::<Result<_>>()?;
    let Selection {
        audio,
        mut textures,
        mut texture_skips,
    } = selection;
    let texture_names: BTreeSet<_> = textures.iter().map(|p| p.to_ascii_lowercase()).collect();
    // A main image with a separate mask must retain matching storage geometry.
    // Include logical aliases: its companion may use an old script-side name.
    let image_names: Vec<_> = inventory
        .entries
        .iter()
        .filter(|e| e.media.as_ref().is_some_and(|m| m.kind == "image"))
        .map(|e| e.path.as_str())
        .chain(links.iter().map(|l| l.source.as_str()))
        .collect();
    let companion_bases: BTreeSet<_> = image_names
        .iter()
        .filter_map(|name| {
            let path = Path::new(name);
            let stem = path.file_stem()?.to_str()?.to_ascii_lowercase();
            (stem.ends_with("_m") || stem.ends_with("_p")).then(|| {
                path.with_file_name(&stem[..stem.len() - 2])
                    .to_string_lossy()
                    .to_ascii_lowercase()
            })
        })
        .collect();
    for name in &image_names {
        let path = Path::new(name);
        let base = path
            .with_file_name(path.file_stem().unwrap_or_default())
            .to_string_lossy()
            .to_ascii_lowercase();
        if companion_bases.contains(&base)
            && texture_names.contains(&follow(name, &redirects)?.to_ascii_lowercase())
        {
            return Err(format!(
                "image with a mask or province companion cannot use lossy textures: {name}"
            ));
        }
    }
    for path in &textures {
        if province(path)
            || is_mask(path)
            || province_targets.contains(&path.to_ascii_lowercase())
            || mask_targets.contains(&path.to_ascii_lowercase())
        {
            return Err(format!(
                "masks and province images cannot use lossy texture compression: {path}"
            ));
        }
    }
    // Preflight uses only container headers and probed dimensions. Pixel
    // classification runs inside the conversion job so each image is decoded
    // once, without retaining an entire game's RGBA data between two passes.
    let decisions = inventory
        .entries
        .par_iter()
        .filter(|e| textures.contains(&e.path))
        .map(|entry| {
            let info = entry.media.as_ref().unwrap();
            let source = crate::files::resource(&inventory.root, &entry.path)?;
            let indexed =
                if options.texture_auto && matches!(info.container.as_str(), "png" | "bmp") {
                    use std::io::Read;
                    let mut header = [0u8; 32];
                    let mut file = fs::File::open(&source).map_err(|e| at(&source, e))?;
                    let count = file.read(&mut header).map_err(|e| at(&source, e))?;
                    (count >= 26 && header.starts_with(b"\x89PNG\r\n\x1a\n") && header[25] == 3)
                        || (count >= 30
                            && header.starts_with(b"BM")
                            && u16::from_le_bytes([header[28], header[29]]) <= 8)
                } else {
                    false
                };
            // Exclude container formats before asking the static-image decoder to
            // flatten them. CUR/ICO hotspots and frames, PSD layers and existing
            // compressed textures must survive automatic selection unchanged.
            if options.texture_auto
                && (indexed
                    || matches!(
                        info.container.as_str(),
                        "psd" | "ico" | "cur" | "ktx" | "kbct" | "gif"
                    ))
            {
                return Ok((
                    entry.path.clone(),
                    Err("indexed, layered, icon or already compressed image".to_owned()),
                ));
            }
            let size = Size {
                width: info.width.ok_or("missing image width")?,
                height: info.height.ok_or("missing image height")?,
            };
            let reason = if crate::texture::storage_size(scaled(size, ratio)).is_none() {
                Some("tiled texture exceeds PSV resource size or dimension limits")
            } else if options.texture_auto && (size.width < 64 || size.height < 64) {
                Some("small image may contain text or interface details")
            } else {
                None
            };
            if let Some(reason) = reason {
                if !options.texture_auto {
                    return Err(at(&source, reason));
                }
                Ok((entry.path.clone(), Err(reason.to_owned())))
            } else {
                Ok((entry.path.clone(), Ok(())))
            }
        })
        .collect::<Result<Vec<_>>>()?;
    for (path, decision) in decisions {
        if let Err(reason) = decision {
            textures.remove(&path);
            texture_skips.insert(path, reason);
        }
    }
    // Encoding is in-process; parallel file workers share immutable settings.
    let encoder = crate::bc::Encoder::new(options.texture_quality);
    let mut occupied = crate::normalize::occupied(&inventory);
    let mut converted_names = BTreeMap::new();
    if names.len() != inventory.entries.len() {
        return Err("resource names collide after ASCII case folding".into());
    }
    for entry in &inventory.entries {
        if let Some(info) = &entry.media {
            let ext = if textures.contains(&entry.path) {
                Some(if options.texture_storage != crate::bc::Storage::Bc {
                    "kbct"
                } else {
                    "ktx"
                })
            } else if info.kind == "audio" {
                if info.tracks.len() != 1 || info.tracks[0].kind != "audio" {
                    return Err(format!(
                        "audio resource needs an unambiguous single audio track: {}",
                        entry.path
                    ));
                }
                if audio.contains(&entry.path) {
                    Some("at9")
                } else {
                    (!audio_supported(info)).then_some("ogg")
                }
            } else if info.kind == "image" && !image_supported(info) {
                if province(&entry.path)
                    || province_targets.contains(&entry.path.to_ascii_lowercase())
                {
                    return Err(format!(
                        "province image conversion must preserve palette indices: {}",
                        entry.path
                    ));
                }
                Some("png")
            } else {
                None
            };
            if let Some(ext) = ext {
                let marker = format!("{}{}", entry.path, krkr_assets::converted::LINK_SUFFIX);
                if !occupied.insert(marker.to_ascii_lowercase()) {
                    return Err(format!("resource link already exists: {marker}"));
                }
                let output = crate::normalize::reserve(&entry.path, ext, &mut occupied)?;
                let fallback = if matches!(ext, "ogg" | "ktx" | "kbct") {
                    Some(crate::normalize::reserve(
                        &entry.path,
                        if ext == "ogg" { "flac" } else { "png" },
                        &mut occupied,
                    )?)
                } else {
                    None
                };
                converted_names.insert(entry.path.clone(), (output, fallback));
            }
        }
        if entry.extension == "xp3" {
            return Err(format!(
                "unpack XP3 archives before PSV conversion: {}",
                entry.path
            ));
        }
        if entry.path.ends_with(krkr_image::scale::SUFFIX)
            || entry
                .path
                .ends_with(krkr_assets::converted::VIDEO_MARKER_SUFFIX)
            || entry.path == "krkr-psv.json"
        {
            return Err("input is already a converted package; use original assets".into());
        }
        if entry.consistency == media::Consistency::Unreadable {
            return Err(format!("unreadable media: {}", entry.path));
        }
        if entry.consistency == media::Consistency::Mismatch
            && entry
                .media
                .as_ref()
                .is_none_or(|m| m.kind != "video" || m.container == "ajpm")
        {
            return Err(format!(
                "run normalize or use helper before PSV conversion: {}",
                entry.path
            ));
        }
        if entry.media.as_ref().is_some_and(|m| {
            m.kind == "video"
                && m.container != "ajpm"
                && (m.width.is_none_or(|w| w < 2) || m.height.is_none_or(|h| h < 2))
        }) {
            return Err(format!(
                "video dimensions cannot be converted to PSV 4:2:0: {}",
                entry.path
            ));
        }
        if entry
            .media
            .as_ref()
            .is_some_and(|m| m.kind == "video" && m.container != "ajpm")
            && entry.extension != "mp4"
            && names.contains(&format!("{}.mp4", entry.path).to_ascii_lowercase())
        {
            return Err(format!(
                "converted video path already exists: {}.mp4",
                entry.path
            ));
        }
    }
    let scratch = tempfile::Builder::new()
        .prefix(".krkr-psv-")
        .tempdir_in(parent)
        .map_err(|e| e.to_string())?;
    let mut report = Report {
        version: 1,
        source: inventory.root.clone(),
        output: output.clone(),
        canvas: [options.canvas.width, options.canvas.height],
        target: [options.target.width, options.target.height],
        video_profile: "MP4 / H.264 Main <=L4.0 / yuv420p BT.601 limited / <=960x544 / <=60fps / AAC-LC 48kHz stereo",
        links: Vec::new(),
        entries: Vec::new(),
        texture_timings: Default::default(),
    };
    let bar = crate::progress::Progress::new("psv", Some(inventory.entries.len()));
    report.entries = inventory
        .entries
        .into_par_iter()
        .map(|entry| {
            let source = crate::files::resource(&inventory.root, &entry.path)?;
            if hash(&source)? != entry.source_sha256 {
                return Err(at(&source, "source changed during probing"));
            }
            let repaired = if entry.media.as_ref().is_some_and(|m| m.container == "bmp") {
                crate::bmp::repair(&source)?
            } else {
                None
            };
            let input = repaired
                .as_ref()
                .map_or(source.as_path(), |r| r.file.path());
            let conversion = converted_names.get(&entry.path);
            let mut name = conversion.map_or_else(|| entry.path.clone(), |(name, _)| name.clone());
            let mut target = scratch.path().join(&name);
            fs::create_dir_all(target.parent().unwrap()).map_err(|e| at(&target, e))?;
            let mut dimensions = None;
            let mut texture_skip = texture_skips.get(&entry.path).cloned();
            let mut texture_storage = None;
            let action;
            if let Some(info) = &entry.media {
                if audio.contains(&entry.path) {
                    crate::at9::encode(&source, &target, info, options.at9.as_ref().unwrap(), tools)?;
                    action = "audio_at9";
                } else if info.kind == "audio" && conversion.is_some() {
                    // Check every converted timeline. A .sli may live beside a
                    // logical alias or even in another XP3, not beside this file.
                    let samples = adjust::audio_samples(&source, tools)?;
                    adjust::transcode(&source, &target, info, "ogg", tools, None)?;
                    verify_audio(&source, &target, info, "ogg", tools)?;
                    if adjust::audio_samples(&target, tools)? == samples {
                        action = "audio_vorbis";
                    } else {
                        // Some short Vorbis layouts lose a decoded first block.
                        // FLAC preserves the timeline without exporting raw PCM.
                        fs::remove_file(&target).map_err(|e| at(&target, e))?;
                        name = conversion.unwrap().1.clone().unwrap();
                        target = scratch.path().join(&name);
                        adjust::transcode(&source, &target, info, "flac", tools, None)?;
                        verify_audio(&source, &target, info, "flac", tools)?;
                        if adjust::audio_samples(&target, tools)? != samples {
                            return Err(at(&source, "conversion changed decoded sample counts; output was not published"));
                        }
                        action = "audio_flac";
                    }
                } else if matches!(info.kind.as_str(), "image" | "video")
                    && let (Some(width), Some(height)) = (info.width, info.height)
                {
                    let logical = Size { width, height };
                    let mut stored = if info.kind == "image"
                        && !textures.contains(&entry.path)
                        && retain_small_image(logical, options.canvas)
                    {
                        logical
                    } else {
                        scaled(logical, ratio)
                    };
                    if info.container == "ajpm" {
                        fs::copy(&source, &target).map_err(|e| at(&source, e))?;
                        action = "amv_native";
                    } else if info.kind == "video" {
                        let fit = ratio
                            .min(f64::from(options.target.width) / f64::from(logical.width))
                            .min(f64::from(options.target.height) / f64::from(logical.height));
                        stored = scaled(logical, fit);
                        stored.width = (stored.width / 2 * 2).max(2);
                        stored.height = (stored.height / 2 * 2).max(2);
                        if entry.extension != "mp4" {
                            name.push_str(".mp4");
                            target = scratch.path().join(&name);
                        }
                        video(&source, &target, info, stored, tools)?;
                        metadata(&target, logical, stored)?;
                        if entry.extension != "mp4" {
                            fs::write(
                                companion(
                                    &scratch.path().join(&entry.path),
                                    krkr_assets::converted::VIDEO_MARKER_SUFFIX,
                                ),
                                krkr_assets::converted::VIDEO_MARKER,
                            )
                            .map_err(|e| e.to_string())?;
                        }
                        action = "video_hardware_profile";
                        dimensions = Some((logical, stored));
                    } else if textures.contains(&entry.path) {
                        // PVR forces compressed POT textures to the supported
                        // twiddled layout. NPOT+STRIDE is not a compressed upload
                        // path in that driver. Resample storage, never script
                        // coordinates; the scale sidecar restores display shape.
                        stored = crate::texture::storage_size(stored)
                            .ok_or_else(|| at(&source, "tiled texture exceeds PSV resource limits"))?;
                        let budget = Budget::new(1024 * 1024 * 1024);
                        let mut decoded = adjust::decode_image(input, info, tools, &budget)?;
                        let main = decoded.pixels.main.as_ref().ok_or("image has no main plane")?;
                        let transparent = main.as_slice().as_chunks::<4>().0.iter().any(|p| p[3] != 255);
                        let reason = if decoded.pixels.province.is_some() {
                            Some("texture conversion must preserve province indices")
                        } else if krkr_image::compressed::validate_tags(&decoded.tags).is_err() {
                            Some("image tags exceed compressed metadata capacity")

                        } else if options.texture_auto && main.as_slice().as_chunks::<4>().0.iter()
                            .filter(|p| p[3] != 0)
                            .all(|p| p[0].abs_diff(p[1]) <= 2 && p[1].abs_diff(p[2]) <= 2) {
                            Some("grayscale image may be a transition rule or mask")
                        } else { None };
                        if let Some(reason) = reason && !options.texture_auto {
                            return Err(at(&source, reason));
                        }
                        let format = if transparent { krkr_protocol::texture::Format::Bc3Rgba }
                            else { krkr_protocol::texture::Format::Bc1Rgb };
                        let encoded = if let Some(reason) = reason {
                            Err(crate::texture::EncodeError::Quality(reason.to_owned()))
                        } else {
                            let encode_at = |size| -> std::result::Result<_, crate::texture::EncodeError> {
                                let pixels = if size == logical { None } else { Some(shrink(main, logical, size, &budget)?) };
                                let rgba = pixels.as_ref().unwrap_or(main).as_slice();
                                crate::texture::encode_tiled_checked(size, rgba, &decoded.tags, |size, pixels| {
                                    match options.texture_storage {
                                        crate::bc::Storage::BcCrunch => encoder.encode_packed(size,pixels,format),
                                        crate::bc::Storage::Bc => encoder.encode(size,pixels,format),
                                    }
                                }, |size, pixels, data| encoder.check(size, pixels, data, options.texture_auto))
                            };
                            let mut result = encode_at(stored);
                            if options.texture_quality != crate::bc::Quality::Fast && matches!(&result, Err(crate::texture::EncodeError::Quality(reason))
                                if reason.starts_with("compression color error")) {
                                let lossless = if retain_small_image(logical, options.canvas) { logical } else { scaled(logical, ratio) };
                                for size in crate::texture::quality_sizes(stored, lossless, format) {
                                    match encode_at(size) {
                                        Ok(data) => { stored = size; result = Ok(data); break; }
                                        Err(crate::texture::EncodeError::Quality(_)) => {}
                                        Err(error) => { result = Err(error); break; }
                                    }
                                }
                            }
                            result
                        };
                        let data = match encoded {
                            Ok(data) if data.starts_with(krkr_image::packed_bc::MAGIC) => Some(data),
                            Ok(data) => Some(krkr_image::compressed::vita_bc(&data).map_err(|e| at(&source, e))?),
                            Err(crate::texture::EncodeError::Quality(reason)) => { texture_skip = Some(reason); None },
                            Err(crate::texture::EncodeError::Failed(error)) => return Err(at(&source, error)),
                        };
                        if let Some(data) = data {
                            let (packed_tiles,native_bc_tiles,gpu_bytes) = if data.starts_with(krkr_image::packed_bc::MAGIC) {
                                krkr_image::packed_bc::storage_stats(&data).map_err(|e|at(&source,e))?
                            } else {(0,(stored.width.div_ceil(1024)*stored.height.div_ceil(1024)) as usize,format.byte_len(stored).unwrap())};
                            texture_storage=Some(TextureStorage {packed_tiles,native_bc_tiles,encoded_bytes:data.len(),gpu_bytes});
                            fs::write(&target, data).map_err(|e| at(&target, e))?;
                            action = if format == krkr_protocol::texture::Format::Bc1Rgb { "image_bc1" } else { "image_bc3" };
                        } else {
                            name = conversion.unwrap().1.clone().unwrap();
                            target = scratch.path().join(&name);
                            // A lossless fallback has no POT constraint. Keep
                            // display-density pixels rather than inflating RAM
                            // to the rejected compressed texture's POT extent.
                            stored = if retain_small_image(logical, options.canvas) { logical } else { scaled(logical, ratio) };
                            let replacement = shrink(main, logical, stored, &budget)?;
                            decoded.pixels.main = Some(replacement);
                            decoded.pixels.size = stored;
                            adjust::save_image(&target, decoded, "png", tools, budget)?;
                            action = "image_lossless_quality";
                        }
                        metadata(&target, logical, stored)?;
                        dimensions = Some((logical, stored));
                    } else if (stored != logical || conversion.is_some())
                        && (conversion.is_some() || adjust::image_format(&entry.extension).is_some()
                            || entry.extension == "webp")
                        && !province(&entry.path)
                        && !province_targets.contains(&entry.path.to_ascii_lowercase())
                        && !matches!(info.container.as_str(), "psd" | "cur" | "ico")
                    {
                        let budget = Budget::new(1024 * 1024 * 1024);
                        let mut decoded = adjust::decode_image(input, info, tools, &budget)
                            .map_err(|e| at(&source, e))?;
                        let pixels = shrink(
                            decoded
                                .pixels
                                .main
                                .as_ref()
                                .ok_or("image has no main plane")?,
                            logical,
                            stored,
                            &budget,
                        )?;
                        decoded.pixels.main = Some(pixels);
                        decoded.pixels.size = stored;
                        let ext = media::extension(&target);
                        adjust::save_image(&target, decoded, &ext, tools, budget)?;
                        let check = media::inspect_file(&target, tools)?
                            .ok_or("scaled output is not an image")?;
                        if check.width != Some(stored.width)
                            || check.height != Some(stored.height)
                            || !image_supported(&check)
                            || media::consistency(&ext, Some(&check))
                                != media::Consistency::Match
                        {
                            return Err(at(&target, "scaled image validation failed"));
                        }
                        metadata(&target, logical, stored)?;
                        action = if conversion.is_some() { "image_converted" } else { "image_scaled" };
                        dimensions = Some((logical, stored));
                    } else {
                        fs::copy(input, &target).map_err(|e| at(&source, e))?;
                        action = "copied";
                    }
                } else if info.kind == "image" || info.kind == "video" {
                    return Err(at(&source, "missing media dimensions"));
                } else {
                    fs::copy(input, &target).map_err(|e| at(&source, e))?;
                    action = "copied";
                }
            } else {
                fs::copy(&source, &target).map_err(|e| at(&source, e))?;
                action = "copied";
            }
            if hash(&source)? != entry.source_sha256 {
                return Err(at(&source, "source changed during conversion"));
            }
            let sha256 = hash(&target)?;
            bar.done(&entry.path);
            progress.lock().unwrap()(&format!("psv {}", entry.path));
            Ok(Entry {
                texture_storage,
                texture_skip,
                source: entry.path,
                output: name,
                action: action.into(),
                logical_size: dimensions.map(|(s, _)| [s.width, s.height]),
                stored_size: dimensions.map(|(_, s)| [s.width, s.height]),
                sha256,
                repair: repaired.map(|r| r.detail),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    links.extend(
        report
            .entries
            .iter()
            .filter(|e| e.source != e.output && e.action != "video_hardware_profile")
            .map(|e| crate::normalize::Link {
                source: e.source.clone(),
                target: e.output.clone(),
            }),
    );
    // Generic links retain logical names for masks and .sli. Carry scale
    // metadata to those names too, and flatten any video conversion redirects.
    let mut redirects: BTreeMap<_, _> = links
        .iter()
        .map(|l| (l.source.to_ascii_lowercase(), l.target.clone()))
        .collect();
    redirects.extend(
        report
            .entries
            .iter()
            .filter(|e| e.source != e.output)
            .map(|e| (e.source.to_ascii_lowercase(), e.output.clone())),
    );
    for link in &links {
        let target = follow(&link.source, &redirects)?;
        crate::files::resource(scratch.path(), &target)?;
        let marker = format!("{}{}", link.source, krkr_assets::converted::LINK_SUFFIX);
        let data = krkr_assets::converted::encode_link(
            Path::new(&target).file_name().unwrap().to_str().unwrap(),
        )
        .map_err(|e| e.to_string())?;
        fs::write(scratch.path().join(&marker), data).map_err(|e| e.to_string())?;
        if let Some(entry) = report.entries.iter_mut().find(|e| e.source == marker) {
            entry.sha256 = hash(&scratch.path().join(&marker))?;
        }
        report.links.push(crate::normalize::Link {
            source: link.source.clone(),
            target: target.clone(),
        });
        let physical_scale = companion(&scratch.path().join(&target), krkr_image::scale::SUFFIX);
        if physical_scale.is_file() {
            let logical_scale = companion(
                &scratch.path().join(&link.source),
                krkr_image::scale::SUFFIX,
            );
            if logical_scale.exists() {
                return Err(at(&logical_scale, "logical scale metadata already exists"));
            }
            fs::copy(&physical_scale, &logical_scale).map_err(|e| at(&logical_scale, e))?;
        }
    }
    let mut vfs =
        krkr_assets::Vfs::new(scratch.path(), Default::default()).map_err(|e| e.to_string())?;
    for link in &report.links {
        vfs.plan(&krkr_assets::name::units(&link.source))
            .map_err(|e| format!("{}: {e}", link.source))?;
    }
    // The report is for the conversion host. Runtime consumers only need the
    // small per-resource markers, so keep JSON out of the exported asset tree.
    fs::rename(scratch.path(), &output).map_err(|e| at(&output, e))?;
    report.texture_timings = encoder.timings();
    Ok(report)
}

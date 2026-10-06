//! Shared media inventory. Uses engine image/audio readers for built-in support;
//! ffprobe supplies actual container/codec metadata for offline planning.
use crate::progress::Progress;
use krkr_assets::{Limits, ReadPlan, ReadSource, Stream, local};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
};

pub type Result<T> = std::result::Result<T, String>;
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}
impl Default for Tools {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Track {
    pub kind: String,
    pub codec: String,
    pub profile: Option<String>,
    pub level: Option<u32>,
    pub frames: Option<u64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub samples: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pixel_format: Option<String>,
    pub frame_rate: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Media {
    pub kind: String,
    pub container: String,
    pub tracks: Vec<Track>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub status: String,
    pub detail: Option<String>,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Consistency {
    Match,
    Mismatch,
    Unknown,
    Unreadable,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Entry {
    pub path: String,
    pub extension: String,
    pub expected_format: Option<String>,
    pub source_bytes: u64,
    pub source_sha256: String,
    pub media: Option<Media>,
    pub consistency: Consistency,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Report {
    pub version: u32,
    pub root: PathBuf,
    pub entries: Vec<Entry>,
}
pub(crate) fn at(path: &Path, e: impl std::fmt::Display) -> String {
    format!("{}: {e}", path.display())
}
pub(crate) fn hash(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| at(path, e))?;
    let mut hash = Sha256::new();
    let mut bytes = [0; 65536];
    loop {
        let n = file.read(&mut bytes).map_err(|e| at(path, e))?;
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub(crate) fn run(command: &mut Command) -> Result<Vec<u8>> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", command.get_program().to_string_lossy()))?;
    if !output.status.success() {
        return Err(format!(
            "{}: {}",
            command.get_program().to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(output.stdout)
}
/// FFmpeg interprets the '?' in Win32 verbatim paths as URL syntax and can
/// select image2 instead of the actual container (silently losing animation).
pub(crate) fn tool_path(path: &Path) -> Result<PathBuf> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_wide(&local::units(path).map_err(|e| at(path, e))?).into())
    }
    #[cfg(not(windows))]
    {
        Ok(path.to_owned())
    }
}
pub(crate) fn version(program: &Path) -> Result<String> {
    let bytes = run(Command::new(program).arg("-version"))?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .next()
        .unwrap_or_default()
        .into())
}
pub(crate) fn h264(tools: &Tools) -> Result<Vec<&'static str>> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, Vec<&'static str>>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    if let Some(args) = cache.get(&tools.ffmpeg) {
        return Ok(args.clone());
    }
    let bytes = run(Command::new(&tools.ffmpeg).args(["-hide_banner", "-encoders"]))?;
    let output = String::from_utf8_lossy(&bytes);
    let has = |name: &str| {
        output
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(name))
    };
    let args = if has("libx264") {
        vec!["-c:v", "libx264", "-crf", "20"]
    } else if has("libopenh264") {
        vec!["-c:v", "libopenh264", "-b:v", "4M", "-rc_mode", "bitrate"]
    } else {
        return Err("FFmpeg needs libx264 or libopenh264 for H.264 conversion".into());
    };
    cache.insert(tools.ffmpeg.clone(), args.clone());
    Ok(args)
}

/// File workers share the CPU with each decoder, encoder and filter graph.
pub(crate) fn ffmpeg(tools: &Tools, source: &Path) -> Result<Command> {
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let threads = (cpus / rayon::current_num_threads())
        .clamp(1, 4)
        .to_string();
    let mut command = Command::new(&tools.ffmpeg);
    command
        .args([
            "-nostdin",
            "-v",
            "error",
            "-xerror",
            "-filter_threads",
            &threads,
            "-filter_complex_threads",
            &threads,
            "-threads",
            &threads,
            "-protocol_whitelist",
            "file",
            "-i",
        ])
        .arg(tool_path(source)?)
        .args(["-threads", &threads]);
    Ok(command)
}

fn text_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xfe])
        || bytes.starts_with(&[0xfe, 0xff])
        || std::str::from_utf8(bytes)
            .is_ok_and(|s| !s.is_empty() && s.chars().all(|c| !c.is_control() || c.is_whitespace()))
}
fn number(value: &serde_json::Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}
pub(crate) fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}
struct LocalFile(PathBuf);
impl ReadSource for LocalFile {
    fn open(&self) -> krkr_assets::Result<Box<dyn Stream>> {
        Ok(Box::new(File::open(&self.0)?))
    }
}
pub(crate) fn plan(path: &Path) -> Result<ReadPlan> {
    // These are already resolved host files, including Windows verbatim paths.
    // Do not reinterpret them as game storage URLs through the VFS.
    Ok(ReadPlan::custom(
        local::units(path).map_err(|e| at(path, e))?,
        fs::metadata(path).map_err(|e| at(path, e))?.len(),
        256 * 1024 * 1024,
        Arc::new(LocalFile(path.to_owned())),
    ))
}
pub(crate) fn builtin(path: &Path) -> Result<krkr_audio::Format> {
    krkr_audio::inspect_builtin(plan(path)?)
}
fn is_image(prefix: &[u8], ext: &str) -> bool {
    prefix.starts_with(b"\x89PNG\r\n\x1a\n")
        || prefix.starts_with(krkr_image::compressed::MAGIC)
        || prefix.starts_with(krkr_image::packed_bc::MAGIC)
        || prefix.starts_with(b"BM")
        || prefix.starts_with(b"TLG")
        || prefix.starts_with(b"\xff\xd8\xff")
        || prefix.starts_with(b"GIF8")
        || (prefix.starts_with(b"RIFF") && prefix.get(8..12) == Some(b"WEBP"))
        || matches!(
            ext,
            "png"
                | "bmp"
                | "dib"
                | "tlg"
                | "tlg5"
                | "tlg6"
                | "jpg"
                | "jpeg"
                | "jif"
                | "webp"
                | "ktx"
                | "kbct"
                | "gif"
                | "tif"
                | "tiff"
                | "avif"
                | "psd"
                | "ico"
                | "cur"
        )
}
/// Game resource contracts include codecs where legacy script dispatch depends
/// on them: .ogg/.oga mean Vorbis, while .opus explicitly requests Opus.
pub(crate) fn expected(ext: &str) -> Option<(&'static str, &'static str)> {
    Some(match ext {
        "ktx" => ("image", "ktx"),
        "kbct" => ("image", "kbct"),
        "png" => ("image", "png"),
        "jpg" | "jpeg" | "jif" => ("image", "jpeg"),
        "bmp" | "dib" => ("image", "bmp"),
        "webp" => ("image", "webp"),
        "gif" => ("image", "gif"),
        "tif" | "tiff" => ("image", "tiff"),
        "avif" => ("image", "avif"),
        "tlg" | "tlg5" | "tlg6" => ("image", "tlg"),
        "psd" => ("image", "psd"),
        "ico" => ("image", "ico"),
        "cur" => ("image", "cur"),
        "wav" | "wave" => ("audio", "wav"),
        "at9" => ("audio", "at9"),
        "ogg" | "oga" | "opus" => ("audio", "ogg"),
        "flac" => ("audio", "flac"),
        "mp3" => ("audio", "mp3"),
        "mp2" => ("audio", "mp2"),
        "aac" => ("audio", "aac"),
        "aif" | "aiff" => ("audio", "aiff"),
        "m4a" => ("audio", "mp4"),
        "wma" => ("audio", "asf"),
        "mka" => ("audio", "matroska"),
        "tcwf" => ("audio", "tcwf"),
        "mp4" | "m4v" => ("video", "mp4"),
        "mov" => ("video", "mov"),
        "avi" => ("video", "avi"),
        "mpg" | "mpeg" => ("video", "mpeg"),
        "wmv" => ("video", "asf"),
        "mkv" => ("video", "matroska"),
        "webm" => ("video", "webm"),
        "ogv" => ("video", "ogg"),
        "ogx" => ("media", "ogg"),
        "amv" => ("video", "ajpm"),
        // ASF can contain either audio or video.
        "asf" => ("media", "asf"),
        _ => return None,
    })
}
pub(crate) fn consistency(ext: &str, media: Option<&Media>) -> Consistency {
    let Some(media) = media else {
        return Consistency::Unknown;
    };
    if media.status == "unreadable" {
        return Consistency::Unreadable;
    }
    let Some((kind, format)) = expected(ext) else {
        return Consistency::Unknown;
    };
    if media.container != format
        || (kind != "media" && media.kind != kind)
        || (ext == "opus"
            && media
                .tracks
                .iter()
                .any(|t| t.kind == "audio" && t.codec != "opus"))
        || (matches!(ext, "ogg" | "oga")
            && media
                .tracks
                .iter()
                .any(|t| t.kind == "audio" && t.codec != "vorbis"))
        || (ext == "tlg5" && media.detail.as_deref() != Some("tlg5"))
        || (ext == "tlg6" && media.detail.as_deref() != Some("tlg6"))
    {
        Consistency::Mismatch
    } else {
        Consistency::Match
    }
}
fn canonical_container(container: &str, tracks: &[Track], prefix: &[u8]) -> String {
    match container {
        "apng" => "png".into(),
        "mov,mp4,m4a,3gp,3g2,mj2" => {
            if prefix.get(8..12) == Some(b"qt  ") {
                "mov".into()
            } else if matches!(prefix.get(8..12), Some(b"avif" | b"avis")) {
                "avif".into()
            } else {
                "mp4".into()
            }
        }
        "matroska,webm" => {
            if prefix.windows(4).any(|s| s == b"webm") {
                "webm".into()
            } else {
                "matroska".into()
            }
        }
        "mp3" if tracks.iter().any(|t| t.codec == "mp2") => "mp2".into(),
        value => value.strip_suffix("_pipe").unwrap_or(value).into(),
    }
}
fn prefix(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| at(path, e))?
        .take(4096)
        .read_to_end(&mut bytes)
        .map_err(|e| at(path, e))?;
    Ok(bytes)
}
pub(crate) fn probe(tools: &Tools, path: &Path) -> Result<Media> {
    let bytes = run(Command::new(&tools.ffprobe).args(["-v", "error", "-threads", "1", "-protocol_whitelist", "file", "-show_entries",
        "stream=codec_type,codec_name,profile,level,nb_frames,sample_rate,channels,duration_ts,time_base,width,height,pix_fmt,r_frame_rate:format=format_name",
        "-of", "json", "-i"]).arg(tool_path(path)?)).map_err(|e| at(path, e))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| at(path, e))?;
    let streams = value["streams"]
        .as_array()
        .ok_or_else(|| at(path, "missing streams"))?;
    let mut tracks = Vec::new();
    for s in streams {
        let rate = number(&s["sample_rate"]).and_then(|n| u32::try_from(n).ok());
        let samples = (|| {
            let (numer, denom) = s["time_base"].as_str()?.split_once('/')?;
            let numer: u128 = numer.parse().ok()?;
            let denom: u128 = denom.parse().ok()?;
            let units = (number(&s["duration_ts"])? as u128)
                .checked_mul(numer)?
                .checked_mul(rate? as u128)?;
            if denom == 0 || units % denom != 0 {
                return None;
            }
            u64::try_from(units / denom).ok()
        })();
        tracks.push(Track {
            kind: s["codec_type"].as_str().unwrap_or("unknown").into(),
            codec: s["codec_name"].as_str().unwrap_or("unknown").into(),
            profile: s["profile"].as_str().map(str::to_owned),
            level: number(&s["level"]).and_then(|n| u32::try_from(n).ok()),
            frames: number(&s["nb_frames"]),
            sample_rate: rate,
            channels: number(&s["channels"]).and_then(|n| u32::try_from(n).ok()),
            samples,
            width: number(&s["width"]).and_then(|n| u32::try_from(n).ok()),
            height: number(&s["height"]).and_then(|n| u32::try_from(n).ok()),
            pixel_format: s["pix_fmt"].as_str().map(str::to_owned),
            frame_rate: s["r_frame_rate"].as_str().map(str::to_owned),
        });
    }
    if tracks.is_empty() {
        return Err(at(path, "no media streams"));
    }
    let video = tracks.iter().find(|t| t.kind == "video");
    let raw_container = value["format"]["format_name"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    let container = canonical_container(&raw_container, &tracks, &prefix(path)?);
    let image = matches!(
        container.as_str(),
        "png" | "jpeg" | "bmp" | "webp" | "gif" | "tiff" | "avif" | "ico" | "psd"
    );
    Ok(Media {
        kind: if image {
            "image"
        } else if video.is_some() {
            "video"
        } else {
            "audio"
        }
        .into(),
        width: video.and_then(|t| t.width),
        height: video.and_then(|t| t.height),
        container,
        tracks,
        status: "probed".into(),
        detail: None,
    })
}
pub(crate) fn inspect_file(path: &Path, tools: &Tools) -> Result<Option<Media>> {
    let ext = extension(path);
    // Generated VFS metadata is validated by the corresponding conversion
    // stage, not by spawning ffprobe once per link in a normalized game.
    if matches!(ext.as_str(), "krkr-link" | "krkr-scale" | "krkr-mp4") {
        return Ok(None);
    }
    let mut prefix = Vec::with_capacity(64);
    File::open(path)
        .map_err(|e| at(path, e))?
        .take(64)
        .read_to_end(&mut prefix)
        .map_err(|e| at(path, e))?;
    if prefix.starts_with(b"TCWF0\x1a") {
        let format = krkr_audio::inspect_tcwf(plan(path)?).map_err(|e| at(path, e))?;
        return Ok(Some(Media {
            kind: "audio".into(),
            container: "tcwf".into(),
            tracks: vec![Track {
                kind: "audio".into(),
                codec: "tcwf".into(),
                profile: None,
                level: None,
                frames: None,
                sample_rate: Some(format.rate),
                channels: Some(format.channels),
                samples: None,
                width: None,
                height: None,
                pixel_format: None,
                frame_rate: None,
            }],
            width: None,
            height: None,
            status: "plugin_required".into(),
            detail: Some(
                "TCWF header checked; playback requires wutcwf; full decode not verified".into(),
            ),
        }));
    }
    if prefix.starts_with(b"RIFF") {
        let mut input = File::open(path).map_err(|e| at(path, e))?;
        let bytes = input.metadata().map_err(|e| at(path, e))?.len();
        if let Some(header) =
            krkr_audio::at9::inspect(&mut input, bytes).map_err(|e| at(path, e))?
        {
            return Ok(Some(Media {
                kind: "audio".into(),
                container: "at9".into(),
                width: None,
                height: None,
                status: "hardware_header".into(),
                detail: Some(
                    "ATRAC9 timeline checked; PSV hardware decode requires device verification"
                        .into(),
                ),
                tracks: vec![Track {
                    kind: "audio".into(),
                    codec: "atrac9".into(),
                    profile: None,
                    level: None,
                    frames: None,
                    sample_rate: Some(header.format.rate),
                    channels: Some(header.format.channels),
                    samples: Some(header.format.frames),
                    width: None,
                    height: None,
                    pixel_format: None,
                    frame_rate: None,
                }],
            }));
        }
    }
    if prefix.starts_with(b"8BPS") {
        let document = krkr_image::psd::Document::load(Arc::new(plan(path)?), Limits::default())
            .map_err(|e| at(path, e))?;
        return Ok(Some(Media {
            kind: "image".into(),
            container: "psd".into(),
            tracks: Vec::new(),
            width: Some(document.size.width),
            height: Some(document.size.height),
            status: "plugin_required".into(),
            detail: Some(format!(
                "PSD plugin index checked: {} layers, {} channels, {}-bit depth, color mode {}; full channel decode not verified",
                document.layers.len(),
                document.channels,
                document.depth,
                document.color_mode
            )),
        }));
    }
    if prefix.starts_with(b"AJPM") {
        let movie = krkr_image::amv::Movie::open(Arc::new(plan(path)?), &|| false)
            .map_err(|e| at(path, e))?
            .ok_or_else(|| at(path, "missing AMV header"))?;
        return Ok(Some(Media {
            kind: "video".into(),
            container: "ajpm".into(),
            tracks: vec![Track {
                kind: "video".into(),
                codec: "ajpm".into(),
                profile: None,
                level: None,
                frames: Some(movie.count() as u64),
                sample_rate: None,
                channels: None,
                samples: None,
                width: Some(movie.size.width),
                height: Some(movie.size.height),
                pixel_format: Some("rgba".into()),
                frame_rate: Some(movie.rate.to_string()),
            }],
            width: Some(movie.size.width),
            height: Some(movie.size.height),
            status: "builtin_header".into(),
            detail: Some(format!(
                "AMV index checked: {} frames; full decode and PSV performance not verified",
                movie.count()
            )),
        }));
    }
    if matches!(prefix.get(..4), Some([0, 0, 1 | 2, 0])) {
        let cursor = krkr_image::cursor::read(
            plan(path)?,
            krkr_protocol::budget::Budget::new(16 * 1024 * 1024),
            &AtomicBool::new(false),
        )
        .map_err(|e| at(path, e))?;
        let container = if prefix[2] == 2 { "cur" } else { "ico" };
        return Ok(Some(Media {
            kind: "image".into(),
            container: container.into(),
            tracks: Vec::new(),
            width: Some(cursor.width.into()),
            height: Some(cursor.height.into()),
            status: "builtin_decoded".into(),
            detail: Some(
                "Selected cursor/icon frame decoded; other directory frames not verified".into(),
            ),
        }));
    }
    let image = is_image(&prefix, &ext);
    let candidate = image
        || matches!(
            ext.as_str(),
            "ogg"
                | "opus"
                | "oga"
                | "wav"
                | "flac"
                | "mp3"
                | "mp2"
                | "aac"
                | "m4a"
                | "wma"
                | "aif"
                | "aiff"
                | "tcwf"
                | "mp4"
                | "avi"
                | "mpg"
                | "mpeg"
                | "wmv"
                | "asf"
                | "mkv"
                | "webm"
                | "mov"
                | "m4v"
                | "amv"
        )
        || prefix.starts_with(b"OggS")
        || prefix.starts_with(b"fLaC")
        || prefix.starts_with(b"ID3")
        || prefix.starts_with(b"TCWF0\x1a")
        || prefix.get(4..8) == Some(b"ftyp")
        || prefix.starts_with(b"\x1a\x45\xdf\xa3")
        || (prefix.starts_with(b"RIFF") && matches!(prefix.get(8..12), Some(b"WAVE" | b"AVI ")));
    let mut image_error = None;
    if image {
        match krkr_image::Request::from_plans(
            plan(path)?,
            None,
            None,
            0x1ffffff,
            None,
            false,
            krkr_protocol::budget::Budget::new(256 * 1024 * 1024),
        )
        .probe(&AtomicBool::new(false))
        {
            Ok(prepared) => {
                return Ok(Some(Media {
                    kind: "image".into(),
                    container: prepared.format_name().into(),
                    tracks: Vec::new(),
                    width: Some(prepared.size.width),
                    height: Some(prepared.size.height),
                    status: "builtin_header".into(),
                    detail: if prepared.format_name() == "tlg" {
                        Some(
                            if prefix.get(3) == Some(&b'0') {
                                match prefix.get(18) {
                                    Some(b'6') => "tlg6",
                                    _ => "tlg5",
                                }
                            } else if prefix.get(3) == Some(&b'6') {
                                "tlg6"
                            } else {
                                "tlg5"
                            }
                            .into(),
                        )
                    } else {
                        None
                    },
                }));
            }
            Err(e) => image_error = Some(e.to_string()),
        }
    }
    // Known text resources still pass through media signatures above, so a PNG
    // hidden behind .ks is discovered. Unknown/binary formats retain ffprobe.
    if !candidate
        && matches!(
            ext.as_str(),
            "tjs" | "ks" | "txt" | "ini" | "cfg" | "json" | "xml" | "csv" | "sli"
        )
        && text_prefix(&prefix)
    {
        return Ok(None);
    }
    let mut result = match probe(tools, path) {
        Ok(result) => result,
        Err(_) if !candidate => return Ok(None),
        Err(e) => {
            return Ok(Some(Media {
                kind: if image { "image" } else { "unknown_media" }.into(),
                container: "unknown".into(),
                tracks: Vec::new(),
                width: None,
                height: None,
                status: "unreadable".into(),
                detail: Some(e),
            }));
        }
    };
    if result.kind == "video"
        && (result.container.ends_with("_pipe")
            || matches!(result.container.as_str(), "gif" | "ico" | "image2"))
    {
        result.kind = "image".into();
        result.status = "host_or_plugin_required".into();
        result.detail = image_error;
    } else if result.kind == "audio" {
        match builtin(path) {
            Ok(_) => {
                result.status = "builtin_header".into();
                result.detail = None;
            }
            Err(e) => {
                result.status = "host_or_plugin_required".into();
                result.detail = Some(e);
            }
        }
    }
    Ok(Some(result))
}
fn linked(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
pub(crate) fn collect(source: &Path) -> Result<(PathBuf, BTreeMap<String, PathBuf>)> {
    if linked(&fs::symlink_metadata(source).map_err(|e| at(source, e))?) {
        return Err(at(source, "source must not be a link"));
    }
    let source = fs::canonicalize(source).map_err(|e| at(source, e))?;
    if source.is_file() && extension(&source) == "xp3" {
        return Err("use xp3 unpack before media inspection/conversion".into());
    }
    let root = if source.is_dir() {
        source.clone()
    } else {
        source.parent().ok_or("source parent missing")?.to_owned()
    };
    let mut pending = vec![source.clone()];
    if source.is_file() {
        let sidecar = PathBuf::from(format!("{}.sli", source.display()));
        if sidecar.exists() {
            pending.push(sidecar);
        }
    }
    let mut files = BTreeMap::new();
    let mut visited = 0;
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).map_err(|e| at(&path, e))?;
        if linked(&meta) || (!meta.is_dir() && !meta.is_file()) {
            return Err(at(&path, "expected regular file/directory, not a link"));
        }
        visited += 1;
        if visited > Limits::default().max_entries {
            return Err("source entry count exceeds limit".into());
        }
        if meta.is_dir() {
            for item in fs::read_dir(&path).map_err(|e| at(&path, e))? {
                pending.push(item.map_err(|e| at(&path, e))?.path());
            }
        } else {
            let name = path
                .strip_prefix(&root)
                .map_err(|e| at(&path, e))?
                .to_str()
                .ok_or("non-Unicode resource name")?
                .replace('\\', "/");
            if files.insert(name, path).is_some() {
                return Err("duplicate resource name".into());
            }
        }
    }
    Ok((source, files))
}
pub fn inspect(source: &Path, tools: &Tools) -> Result<Report> {
    let (source, files) = collect(source)?;
    let root = if source.is_dir() {
        source
    } else {
        source.parent().ok_or("source parent")?.to_owned()
    };
    // Check the tool once so a missing executable is not misreported as unknown data.
    version(&tools.ffprobe)?;
    let progress = Progress::new("probe", Some(files.len()));
    let entries = files
        .into_iter()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(name, path)| {
            let media = match inspect_file(&path, tools) {
                Ok(media) => media,
                Err(error) => Some(Media {
                    kind: "unknown_media".into(),
                    container: "unknown".into(),
                    tracks: Vec::new(),
                    width: None,
                    height: None,
                    status: "unreadable".into(),
                    detail: Some(error),
                }),
            };
            let ext = extension(&path);
            let state = consistency(&ext, media.as_ref());
            let entry = Entry {
                path: name,
                expected_format: expected(&ext).map(|(_, f)| f.into()),
                extension: ext,
                source_bytes: fs::metadata(&path).map_err(|e| at(&path, e))?.len(),
                source_sha256: hash(&path)?,
                media,
                consistency: state,
            };
            progress.done(&entry.path);
            Ok(entry)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(Report {
        version: 1,
        root,
        entries,
    })
}

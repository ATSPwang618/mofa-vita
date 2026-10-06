//! Convert content to the format promised by its existing resource name.
use crate::{
    files,
    media::{self, Consistency, Media, Report, Result, Tools, at, hash, run},
};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
    sync::atomic::AtomicBool,
};

#[derive(Serialize)]
pub struct Adjustment {
    pub path: String,
    pub status: String,
    pub detail: Option<String>,
    pub output_sha256: Option<String>,
}
#[derive(Serialize)]
pub struct Outcome {
    pub version: u32,
    pub root: std::path::PathBuf,
    pub entries: Vec<Adjustment>,
}
impl Outcome {
    pub fn failed(&self) -> bool {
        self.entries.iter().any(|e| e.status == "error")
    }
}

/// Windows PowerShell redirects native stdout to BOM-prefixed UTF-16. Accept
/// those reports as well as the UTF-8 JSON emitted directly by `probe -o`.
pub fn read_report(path: &Path) -> Result<Report> {
    let data = fs::read(path).map_err(|e| at(path, e))?;
    let result = if data.starts_with(&[0xff, 0xfe]) || data.starts_with(&[0xfe, 0xff]) {
        let little = data[0] == 0xff;
        let bytes = &data[2..];
        if bytes.len() % 2 != 0 {
            return Err(at(path, "incomplete UTF-16 JSON code unit"));
        }
        let units: Vec<_> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| {
                if little {
                    u16::from_le_bytes([p[0], p[1]])
                } else {
                    u16::from_be_bytes([p[0], p[1]])
                }
            })
            .collect();
        let text = String::from_utf16(&units).map_err(|e| at(path, e))?;
        serde_json::from_str(&text)
    } else {
        serde_json::from_slice(data.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&data))
    };
    result.map_err(|e| at(path, e))
}

pub(crate) fn image_format(ext: &str) -> Option<krkr_image::save::Format> {
    use krkr_image::save::{BitmapDepth, Format};
    Some(match ext {
        "png" => Format::Png { alpha: true },
        "bmp" | "dib" => Format::Bmp(BitmapDepth::Rgba),
        "jpg" | "jpeg" | "jif" => Format::Jpeg { quality: 95 },
        "tlg" | "tlg5" => Format::Tlg {
            six: false,
            alpha: true,
        },
        "tlg6" => Format::Tlg {
            six: true,
            alpha: true,
        },
        _ => return None,
    })
}

pub(crate) fn decode_image(
    source: &Path,
    info: &Media,
    tools: &Tools,
    budget: &Budget,
) -> Result<krkr_image::Decoded> {
    if matches!(info.container.as_str(), "psd" | "ico" | "cur") {
        return Err(at(
            source,
            "layered documents and multi-frame icons need explicit format-specific conversion",
        ));
    }
    // PNG and WebP can animate even when the engine's ordinary image reader
    // exposes only their first frame.
    let checked = matches!(info.container.as_str(), "png" | "webp");
    if checked {
        static_frame(source, tools)?;
    }
    let request = krkr_image::Request::from_plans(
        media::plan(source)?,
        None,
        None,
        0x02ffffff,
        None,
        false,
        budget.clone(),
    );
    if let Ok(prepared) = request.probe(&AtomicBool::new(false)) {
        return prepared
            .decode(&AtomicBool::new(false))
            .map_err(|e| at(source, e));
    }
    // Count actual frames before using a static image encoder. Never silently
    // turn an animated image into its first frame.
    if !checked {
        static_frame(source, tools)?;
    }
    let size = Size {
        width: info.width.ok_or("missing image width")?,
        height: info.height.ok_or("missing image height")?,
    };
    let bytes = size.rgba_bytes().ok_or("image size overflow")?;
    let mut pixels = Bytes::zeroed(bytes, budget).map_err(|e| e.to_string())?;
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let raw = scratch.path().join("pixels.rgba");
    run(media::ffmpeg(tools, source)?
        .args([
            "-map",
            "0:v:0",
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgba",
            "-f",
            "rawvideo",
        ])
        .arg(media::tool_path(&raw)?))?;
    if fs::metadata(&raw).map_err(|e| at(&raw, e))?.len() != bytes as u64 {
        return Err("decoded image length mismatch".into());
    }
    use std::io::Read;
    fs::File::open(raw)
        .and_then(|mut f| f.read_exact(pixels.as_mut_slice()))
        .map_err(|e| e.to_string())?;
    Ok(krkr_image::Decoded {
        pixels: Pixels {
            size,
            main: Some(pixels),
            province: None,
        },
        tags: Vec::new(),
    })
}
fn static_frame(source: &Path, tools: &Tools) -> Result<()> {
    if let Some(single) = static_container(source)? {
        return if single {
            Ok(())
        } else {
            Err(at(
                source,
                "animated images cannot be converted with a static image writer",
            ))
        };
    }
    let probe = run(Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-count_frames",
            "-show_entries",
            "stream=nb_read_frames",
            "-of",
            "json",
            "-i",
        ])
        .arg(media::tool_path(source)?))?;
    let probe: serde_json::Value = serde_json::from_slice(&probe).map_err(|e| e.to_string())?;
    if probe["streams"]
        .as_array()
        .is_none_or(|s| s.len() != 1 || s[0]["nb_read_frames"].as_str() != Some("1"))
    {
        return Err(at(
            source,
            "static image conversion requires exactly one decoded frame",
        ));
    }
    Ok(())
}

/// PNG/WebP declare animation in container chunks. Seek over compressed pixels
/// instead of spawning ffprobe and decoding every static image a second time.
fn static_container(source: &Path) -> Result<Option<bool>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(source).map_err(|e| at(source, e))?;
    let length = file.metadata().map_err(|e| at(source, e))?.len();
    if length < 12 {
        return Ok(None);
    }
    let mut header = [0; 12];
    file.read_exact(&mut header).map_err(|e| at(source, e))?;
    let png = header.starts_with(b"\x89PNG\r\n\x1a\n");
    let webp = header.starts_with(b"RIFF") && &header[8..] == b"WEBP";
    if !png && !webp {
        return Ok(None);
    }
    let end = if png {
        length
    } else {
        u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8
    };
    if end != length {
        return Err(at(source, "invalid image container length"));
    }
    let mut offset = if png { 8 } else { 12 };
    while offset + if png { 12 } else { 8 } <= end {
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| at(source, e))?;
        let mut chunk = [0; 8];
        file.read_exact(&mut chunk).map_err(|e| at(source, e))?;
        let (tag, size) = if png {
            (
                &chunk[4..],
                u64::from(u32::from_be_bytes(chunk[..4].try_into().unwrap())),
            )
        } else {
            (
                &chunk[..4],
                u64::from(u32::from_le_bytes(chunk[4..].try_into().unwrap())),
            )
        };
        offset += 8 + size + if png { 4 } else { size % 2 };
        if offset > end {
            return Err(at(source, "truncated image chunk"));
        }
        if (png && tag == b"acTL") || (webp && matches!(tag, b"ANIM" | b"ANMF")) {
            return Ok(Some(false));
        }
        if webp && tag == b"VP8X" && size > 0 {
            let mut flags = [0];
            file.read_exact(&mut flags).map_err(|e| at(source, e))?;
            if flags[0] & 2 != 0 {
                return Ok(Some(false));
            }
        }
        if png && tag == b"IEND" {
            return if size == 0 && offset == end {
                Ok(Some(true))
            } else {
                Err(at(source, "invalid PNG ending"))
            };
        }
    }
    if webp && offset == end {
        Ok(Some(true))
    } else {
        Err(at(source, "incomplete image container"))
    }
}

pub(crate) fn save_image(
    output: &Path,
    decoded: krkr_image::Decoded,
    ext: &str,
    tools: &Tools,
    budget: Budget,
) -> Result<()> {
    if ext == "png" && !decoded.tags.is_empty() {
        // Layer.save's PNG writer follows stock save semantics and omits tags.
        // Offline conversion must preserve atlas/effect metadata instead.
        let mut options = krkr_image::export::Options::default();
        options.tags = decoded.tags;
        return krkr_image::export::Request {
            pixels: std::sync::Arc::new(decoded.pixels),
            target: Some(
                krkr_assets::WritePlan::local(output, 1024 * 1024 * 1024)
                    .map_err(|e| e.to_string())?,
            ),
            format: krkr_image::export::Format::Png {
                rgba: true,
                unfiltered: false,
            },
            options,
            budget,
        }
        .run(
            &AtomicBool::new(false),
            &std::sync::atomic::AtomicU8::new(0),
        )
        .map(|_| ())
        .map_err(|e| at(output, e));
    }
    if let Some(format) = image_format(ext) {
        if matches!(ext, "jpg" | "jpeg" | "jif")
            && decoded
                .pixels
                .main
                .as_ref()
                .is_some_and(|p| p.as_slice().as_chunks::<4>().0.iter().any(|p| p[3] != 255))
        {
            return Err("JPEG cannot preserve transparency; original retained".into());
        }
        let request = krkr_image::save::Request {
            target: krkr_assets::WritePlan::local(output, 1024 * 1024 * 1024)
                .map_err(|e| e.to_string())?,
            format,
            pixels: decoded.pixels,
            tags: decoded.tags,
            budget,
        };
        return request
            .write(&AtomicBool::new(false))
            .map_err(|e| at(output, e));
    }
    let args: &[&str] = match ext {
        "webp" => &["-c:v", "libwebp", "-lossless", "1", "-f", "webp"],
        "tif" | "tiff" => &["-c:v", "tiff", "-f", "image2"],
        _ => return Err(format!("no loss-preserving image writer for .{ext}")),
    };
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let png = scratch.path().join("pixels.png");
    save_image(&png, decoded, "png", tools, budget)?;
    run(media::ffmpeg(tools, &png)?
        .args(["-map", "0:v:0", "-frames:v", "1"])
        .args(args)
        .arg(media::tool_path(output)?))?;
    Ok(())
}

fn codecs(ext: &str) -> Result<&'static [&'static str]> {
    Ok(match ext {
        "wav" | "wave" => &["-c:a", "pcm_s16le", "-f", "wav"],
        "ogg" | "oga" => &["-c:a", "libvorbis", "-q:a", "5", "-f", "ogg"],
        "opus" => &["-c:a", "libopus", "-b:a", "128k", "-f", "ogg"],
        "flac" => &["-c:a", "flac", "-f", "flac"],
        "mp3" => &["-c:a", "libmp3lame", "-q:a", "2", "-f", "mp3"],
        "mp2" => &["-c:a", "mp2", "-b:a", "192k", "-f", "mp2"],
        "aac" => &["-c:a", "aac", "-b:a", "192k", "-f", "adts"],
        "aif" | "aiff" => &["-c:a", "pcm_s16be", "-f", "aiff"],
        "m4a" => &["-c:a", "aac", "-b:a", "192k", "-f", "mp4"],
        "wma" => &["-c:a", "wmav2", "-b:a", "192k", "-f", "asf"],
        "mka" => &["-c:a", "flac", "-f", "matroska"],
        "mp4" | "m4v" => &[
            "-c:v",
            "libx264",
            "-crf",
            "18",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ],
        "mov" => &[
            "-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "aac", "-f", "mov",
        ],
        "avi" => &[
            "-c:v",
            "mpeg4",
            "-q:v",
            "3",
            "-c:a",
            "libmp3lame",
            "-q:a",
            "2",
            "-f",
            "avi",
        ],
        "mpg" | "mpeg" => &[
            "-c:v",
            "mpeg2video",
            "-q:v",
            "3",
            "-c:a",
            "mp2",
            "-b:a",
            "192k",
            "-f",
            "mpeg",
        ],
        "wmv" | "asf" => &[
            "-c:v", "wmv2", "-q:v", "3", "-c:a", "wmav2", "-b:a", "192k", "-f", "asf",
        ],
        "mkv" => &[
            "-c:v", "libx264", "-crf", "18", "-pix_fmt", "yuv420p", "-c:a", "flac", "-f",
            "matroska",
        ],
        "webm" => &[
            "-c:v",
            "libvpx-vp9",
            "-crf",
            "28",
            "-b:v",
            "0",
            "-c:a",
            "libopus",
            "-f",
            "webm",
        ],
        "ogv" => &[
            "-c:v",
            "libtheora",
            "-q:v",
            "7",
            "-c:a",
            "libvorbis",
            "-q:a",
            "5",
            "-f",
            "ogg",
        ],
        _ => return Err(format!("no encoder for .{ext}; original retained")),
    })
}

pub(crate) fn transcode(
    source: &Path,
    output: &Path,
    info: &Media,
    ext: &str,
    tools: &Tools,
    scale: Option<Size>,
) -> Result<()> {
    if info.kind == "image" {
        if source
            .file_stem()
            .is_some_and(|s| s.to_string_lossy().to_ascii_lowercase().ends_with("_p"))
        {
            return Err(at(
                source,
                "province images require preserving palette indices; automatic re-encoding is unsupported",
            ));
        }
        if scale.is_some() {
            return Err("use the image resampler for image scaling".into());
        }
        let budget = Budget::new(1024 * 1024 * 1024);
        let decoded = decode_image(source, info, tools, &budget)?;
        return save_image(output, decoded, ext, tools, budget);
    }
    let (kind, _) = media::expected(ext).ok_or_else(|| format!("unknown target suffix .{ext}"))?;
    if kind != "media" && kind != info.kind {
        return Err("extension and content declare different media kinds".into());
    }
    if info
        .tracks
        .iter()
        .any(|t| !matches!(t.kind.as_str(), "audio" | "video"))
    {
        return Err("media has auxiliary streams which this conversion would discard".into());
    }
    if info.tracks.iter().any(|t| {
        t.pixel_format.as_deref().is_some_and(|p| {
            p.contains('a')
                && (p.starts_with("yuva") || p.starts_with("rgba") || p.starts_with("bgra"))
        })
    }) {
        return Err("video contains alpha; an alpha-preserving target profile is required".into());
    }
    let args = codecs(ext)?;
    let mut command = media::ffmpeg(tools, source)?;
    command.args(["-map", "0:v?", "-map", "0:a?", "-map_metadata", "0"]);
    if info.kind == "audio" {
        // Standalone audio and .sli use a continuous decoded sample timeline.
        // Rebuild timestamps so Opus packet discontinuities cannot reach muxers.
        // Use one tick per sample and integer sample positions: N/SR/TB can
        // round down by one tick and make the encoder trim a real final sample.
        command.args(["-af", "asettb=1/sr,asetpts=N"]);
    }
    if let Some(size) = scale {
        command.args([
            "-vf",
            &format!("scale={}:{}:flags=lanczos", size.width, size.height),
        ]);
    }
    if args.contains(&"libx264") {
        // Profiles are independent of the available software H.264 library.
        command.args(media::h264(tools)?);
        let mut i = 0;
        while i < args.len() {
            if matches!(args[i], "-c:v" | "-crf") {
                i += 2;
                continue;
            }
            command.arg(args[i]);
            i += 1;
        }
    } else {
        command.args(args);
    }
    command.arg(media::tool_path(output)?);
    run(&mut command).map_err(|e| at(source, e))?;
    // Decode the entire result before replacing a source: a valid header alone
    // does not establish that the encoder completed a playable stream.
    run(media::ffmpeg(tools, output)?.args(["-map", "0:v?", "-map", "0:a?", "-f", "null", "-"]))?;
    Ok(())
}

/// Container durations can include Opus pre-skip or Vorbis packet padding.
/// Loop coordinates refer to decoded samples, so count those only for .sli media.
pub(crate) fn audio_samples(path: &Path, tools: &Tools) -> Result<Vec<u64>> {
    let bytes = run(Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-threads",
            "1",
            "-protocol_whitelist",
            "file",
            "-select_streams",
            "a",
            "-show_frames",
            "-show_entries",
            "frame=stream_index,nb_samples",
            "-of",
            "json",
            "-i",
        ])
        .arg(media::tool_path(path)?))?;
    let frames: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| at(path, e))?;
    let frames = frames["frames"]
        .as_array()
        .ok_or("missing decoded audio frames")?;
    let mut samples = BTreeMap::<u64, u64>::new();
    for frame in frames {
        let stream = frame["stream_index"]
            .as_u64()
            .ok_or("missing audio stream index")?;
        let count = frame["nb_samples"]
            .as_u64()
            .ok_or("missing decoded sample count")?;
        let total = samples.entry(stream).or_default();
        *total = total
            .checked_add(count)
            .ok_or("audio sample count overflow")?;
    }
    if samples.is_empty() || samples.values().any(|&count| count == 0) {
        return Err(at(path, "no decoded audio samples"));
    }
    Ok(samples.into_values().collect())
}

fn verify(source: &Path, output: &Path, before: &Media, ext: &str, tools: &Tools) -> Result<Media> {
    let after = media::inspect_file(output, tools)?.ok_or("converted output has no media")?;
    if media::consistency(ext, Some(&after)) != Consistency::Match {
        return Err("converted output does not match its extension".into());
    }
    if before.kind != after.kind || before.width != after.width || before.height != after.height {
        return Err("conversion changed media kind or dimensions".into());
    }
    for kind in ["audio", "video"] {
        if before.tracks.iter().filter(|t| t.kind == kind).count()
            != after.tracks.iter().filter(|t| t.kind == kind).count()
            && before.kind != "image"
        {
            return Err("conversion changed media stream count".into());
        }
    }
    let sidecar = source.with_file_name(format!(
        "{}.sli",
        source.file_name().unwrap().to_string_lossy()
    ));
    if sidecar.exists() {
        let before: Vec<_> = before.tracks.iter().filter(|t| t.kind == "audio").collect();
        let after: Vec<_> = after.tracks.iter().filter(|t| t.kind == "audio").collect();
        if before
            .iter()
            .zip(&after)
            .any(|(a, b)| a.sample_rate != b.sample_rate || a.channels != b.channels)
        {
            return Err("cannot verify sample-exact .sli loop coordinates after conversion; original retained".into());
        }
        let source_samples = audio_samples(source, tools)?;
        let output_samples = audio_samples(output, tools)?;
        if source_samples.len() != before.len()
            || output_samples.len() != after.len()
            || source_samples != output_samples
        {
            return Err(format!(
                "conversion changed decoded sample counts for .sli loops: {source_samples:?} -> {output_samples:?}; original retained"
            ));
        }
    }
    Ok(after)
}

pub(crate) fn candidate(entry: &media::Entry) -> bool {
    entry.consistency == Consistency::Mismatch
        || (entry.consistency == Consistency::Match
            && entry
                .media
                .as_ref()
                .is_some_and(|m| matches!(m.container.as_str(), "tlg" | "bmp")))
}

pub fn apply(report: &Report, tools: &Tools, progress: impl FnMut(&str) + Send) -> Result<Outcome> {
    use rayon::prelude::*;
    let progress = std::sync::Mutex::new(progress);
    if report.version != 1 {
        return Err("unsupported probe report version".into());
    }
    files::regular(&report.root, true)?;
    let root = fs::canonicalize(&report.root).map_err(|e| at(&report.root, e))?;
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    for entry in &report.entries {
        if !seen.insert(entry.path.to_ascii_lowercase()) {
            return Err(format!("duplicate report entry: {}", entry.path));
        }
        let source = files::resource(&root, &entry.path)?;
        if entry.extension != media::extension(&source) {
            return Err(at(&source, "report extension differs from filename"));
        }
        if candidate(entry) {
            selected.push((entry, source));
        }
    }
    let bar = crate::progress::Progress::new("adjust", Some(selected.len()));
    let entries = selected
        .into_par_iter()
        .map(|(entry, source)| {
            let result = (|| {
                let before = media::inspect_file(&source, tools)?
                    .ok_or("source is no longer recognized media")?;
                let repaired = if before.container == "bmp" {
                    crate::bmp::repair(&source)?
                } else {
                    None
                };
                let input = repaired
                    .as_ref()
                    .map_or(source.as_path(), |r| r.file.path());
                if media::consistency(&entry.extension, Some(&before)) == Consistency::Match {
                    if before.container == "tlg" {
                        let bytes = fs::read(&source).map_err(|e| at(&source, e))?;
                        if let Some(normalized) = krkr_image::normalize_tlg_metadata(&bytes)
                            .map_err(|e| at(&source, e))?
                        {
                            let digest = files::replace(&source, &entry.source_sha256, |output| {
                                fs::write(output, &normalized).map_err(|e| at(output, e))?;
                                verify(&source, output, &before, &entry.extension, tools)?;
                                hash(output)
                            })?;
                            return Ok(("adjusted", digest, None));
                        }
                    }
                    if let Some(repaired) = &repaired {
                        let digest = files::replace(&source, &entry.source_sha256, |output| {
                            fs::copy(repaired.file.path(), output).map_err(|e| at(output, e))?;
                            verify(&source, output, &before, &entry.extension, tools)?;
                            hash(output)
                        })?;
                        return Ok(("adjusted", digest, Some(repaired.detail.clone())));
                    }
                    return Ok(("already_matches", hash(&source)?, None));
                }
                let digest = files::replace(&source, &entry.source_sha256, |output| {
                    transcode(input, output, &before, &entry.extension, tools, None)?;
                    verify(&source, output, &before, &entry.extension, tools)?;
                    hash(output)
                })?;
                Ok(("adjusted", digest, repaired.map(|r| r.detail)))
            })();
            let item = match result {
                Ok((status, digest, detail)) => Adjustment {
                    path: entry.path.clone(),
                    status: status.into(),
                    detail,
                    output_sha256: Some(digest),
                },
                Err(error) => Adjustment {
                    path: entry.path.clone(),
                    status: "error".into(),
                    detail: Some(error),
                    output_sha256: None,
                },
            };
            bar.done(&entry.path);
            progress.lock().unwrap()(&format!("adjust {}: {}", entry.path, item.status));
            item
        })
        .collect();
    Ok(Outcome {
        version: 1,
        root,
        entries,
    })
}

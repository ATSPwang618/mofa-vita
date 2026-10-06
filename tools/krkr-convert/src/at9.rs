//! External ATRAC9 encoding, with sample-timeline verification before publishing.
use crate::{
    adjust,
    media::{self, Media, Result, Tools, at, run},
};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone)]
pub struct Options {
    pub tool: PathBuf,
    pub globs: Vec<String>,
}
pub(crate) fn select(
    inventory: &media::Report,
    options: Option<&Options>,
) -> Result<BTreeSet<String>> {
    let selected = select_matching(inventory, options)?;
    if options.is_some() && selected.is_empty() {
        return Err("--at9-glob did not select any convertible audio resources".into());
    }
    Ok(selected)
}

/// A helper game spans several inventories, including script-only archives.
/// The caller checks for an empty selection across the whole game.
pub(crate) fn select_matching(
    inventory: &media::Report,
    options: Option<&Options>,
) -> Result<BTreeSet<String>> {
    let Some(options) = options else {
        return Ok(BTreeSet::new());
    };
    let patterns = options
        .globs
        .iter()
        .map(|p| glob::Pattern::new(p).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>>>()?;
    let aliases = crate::normalize::matching_aliases(inventory, &patterns)?;
    let mut selected = BTreeSet::new();
    for entry in &inventory.entries {
        if let Some(info) = &entry.media
            && info.kind == "audio"
            && (patterns.iter().any(|p| p.matches(&entry.path))
                || aliases.contains(&entry.path.to_ascii_lowercase()))
        {
            if info.tracks.len() != 1
                || !matches!(info.tracks[0].sample_rate, Some(8000..=96000))
                || !matches!(info.tracks[0].channels, Some(1 | 2))
            {
                return Err(format!(
                    "{}: AT9 conversion requires one mono/stereo track at 8000..96000 Hz",
                    entry.path
                ));
            }
            if info.container == "at9" {
                continue;
            }
            selected.insert(entry.path.clone());
        }
    }
    Ok(selected)
}
pub(crate) fn encode(
    source: &Path,
    output: &Path,
    info: &Media,
    options: &Options,
    tools: &Tools,
) -> Result<()> {
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let wav = scratch.path().join("source.wav");
    let encoded = scratch.path().join("encoded.at9");
    let decoded = scratch.path().join("decoded.wav");
    // Relabel the encoder's input clock without resampling. Decoded sample
    // indices remain identical; krSR restores the original mixer/script rate.
    // Thus 44.1 kHz resources also use hardware decoding without rewriting .sli.
    run(media::ffmpeg(tools, source)?
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-af",
            "asetrate=48000",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ])
        .arg(&wav))?;
    let samples = adjust::audio_samples(source, tools)?;
    if samples.len() != 1 || adjust::audio_samples(&wav, tools)? != samples {
        return Err(at(source, "PCM staging changed source sample count"));
    }
    run(Command::new(&options.tool)
        .args(["-e", "-fs", "48000"])
        .arg(media::tool_path(&wav)?)
        .arg(media::tool_path(&encoded)?))?;
    let mut input = File::open(&encoded).map_err(|e| at(&encoded, e))?;
    let bytes = input.metadata().map_err(|e| e.to_string())?.len();
    let header = krkr_audio::at9::inspect(&mut input, bytes)?.ok_or("encoder output is not AT9")?;
    if header.format.rate != 48000
        || Some(header.format.channels) != info.tracks[0].channels
        || header.format.frames != samples[0]
    {
        return Err(at(source, "AT9 encoder changed source sample timeline"));
    }
    // The reference decoder trims encoder delay and padding. FFprobe alone
    // reports complete compressed blocks, which are not the audible timeline.
    run(Command::new(&options.tool)
        .args(["-d", "-repeat", "1"])
        .arg(media::tool_path(&encoded)?)
        .arg(media::tool_path(&decoded)?))?;
    if adjust::audio_samples(&decoded, tools)? != samples {
        return Err(at(source, "AT9 decode changed sample count"));
    }
    drop(input);
    source_clock(&encoded, info.tracks[0].sample_rate.unwrap())?;
    std::fs::copy(&encoded, output).map_err(|e| at(output, e))?;
    Ok(())
}

fn source_clock(path: &Path, rate: u32) -> Result<()> {
    let mut file = File::open(path).map_err(|e| at(path, e))?;
    let mut riff = [0; 12];
    file.read_exact(&mut riff).map_err(|e| e.to_string())?;
    let bytes = file.metadata().map_err(|e| e.to_string())?.len();
    if &riff[..4] != b"RIFF"
        || &riff[8..] != b"WAVE"
        || u64::from(u32::from_le_bytes(riff[4..8].try_into().unwrap())) + 8 != bytes
    {
        return Err("AT9 encoder produced a noncanonical RIFF file".into());
    }
    let length = u32::try_from(bytes + 16 - 8).map_err(|_| "AT9 exceeds RIFF size limit")?;
    // Metadata precedes compressed data. Putting it at EOF would force an XP3
    // deflate stream to inflate the entire sound just to discover its clock.
    let mut output =
        tempfile::NamedTempFile::new_in(path.parent().unwrap()).map_err(|e| e.to_string())?;
    riff[4..8].copy_from_slice(&length.to_le_bytes());
    output.write_all(&riff).map_err(|e| e.to_string())?;
    for chunk in [
        &b"krSR"[..],
        &8u32.to_le_bytes(),
        &1u32.to_le_bytes(),
        &rate.to_le_bytes(),
    ] {
        output.write_all(chunk).map_err(|e| e.to_string())?;
    }
    std::io::copy(&mut file, &mut output).map_err(|e| e.to_string())?;
    drop(file);
    output.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

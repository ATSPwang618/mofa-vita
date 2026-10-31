//! Minimal BFSAR (NintendoSDK atk sound archive) reader: enough to resolve a wave
//! sound by name to its FWAV and decode it.
//!
//! Chain: STRG name -> item id (type 1 = sound) -> INFO sound info -> file id (FWSD)
//! -> FWSD wave id table -> (wave archive id, wave index) -> wave archive info ->
//! file id (FWAR) -> FWAV -> DSP-ADPCM/PCM16 channels.

use std::{collections::HashMap, fs::File, io::{Read, Seek, SeekFrom}, path::Path, sync::Mutex};

use anyhow::{Context, Result, bail};

use crate::codec::{Pcm, decode_dsp_into};

fn u8_(d: &[u8], o: usize) -> Result<u8> {
    d.get(o).copied().context("bfsar: out of range")
}
fn u16_(d: &[u8], o: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(d.get(o..o + 2).context("bfsar: out of range")?.try_into()?))
}
fn u32_(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).context("bfsar: out of range")?.try_into()?))
}
fn i32_(d: &[u8], o: usize) -> Result<i32> {
    Ok(u32_(d, o)? as i32)
}

/// Section offsets of a Nintendo binary file (`FSAR`, `FWSD`, `FWAR`, `FWAV`).
fn sections(d: &[u8], magic: &[u8; 4]) -> Result<HashMap<u16, (usize, usize)>> {
    if d.get(..4) != Some(magic) {
        bail!("expected {}", String::from_utf8_lossy(magic));
    }
    let n = u16_(d, 0x10)? as usize;
    let mut m = HashMap::new();
    for i in 0..n {
        let o = 0x14 + i * 12;
        m.insert(u16_(d, o)?, (u32_(d, o + 4)? as usize, u32_(d, o + 8)? as usize));
    }
    Ok(m)
}

/// Reference table: u32 count, then {u16 type, u16 pad, i32 offset} relative to the table.
fn ref_table(d: &[u8], at: usize) -> Result<Vec<Option<usize>>> {
    let n = u32_(d, at)? as usize;
    let remaining = d.len().checked_sub(at.checked_add(4).context("bfsar table offset overflow")?)
        .context("bfsar ref table outside metadata")?;
    anyhow::ensure!(n <= remaining / 8, "bfsar ref table exceeds metadata");
    (0..n)
        .map(|i| {
            let o = at + 4 + i * 8;
            let off = i32_(d, o + 4)?;
            Ok((off >= 0 && u16_(d, o)? != 0).then(|| at + off as usize))
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct StreamSound {
    /// Relative to romfs, e.g. `stream/MOON01_BGM01.opus`.
    pub path: String,
    pub loop_range: Option<(usize, usize)>,
    pub volume: f32,
}

pub struct SoundArchive {
    data: Vec<u8>,
    backing: Option<Mutex<File>>,
    archive_len: usize,
    file_end: usize,
    names: HashMap<String, u32>,
    info: usize,
    file_body: usize,
    sounds: Vec<Option<usize>>,
    warcs: Vec<Option<usize>>,
    files: Vec<Option<usize>>,
}

impl SoundArchive {
    /// Keep only STRG/INFO and the FILE header resident. FILE payloads stay on disk.
    /// The 16 MiB metadata cap is much smaller than the 219 MiB Witch payload.
    pub fn open_file(path: &Path) -> Result<Self> {
        const MAX_META: usize = 16 * 1024 * 1024;
        let mut file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let total = usize::try_from(file.metadata()?.len()).context("BFSAR exceeds address space")?;
        anyhow::ensure!(total >= 0x40, "BFSAR header truncated");
        let mut header = [0u8; 0x100];
        let header_len = total.min(header.len());
        file.read_exact(&mut header[..header_len])?;
        let secs = sections(&header, b"FSAR")?;
        let (file_off, file_size) = *secs.get(&0x2002).context("no FILE section")?;
        let end = file_off.checked_add(file_size).context("FILE section overflow")?;
        let length = file_off.checked_add(8).context("BFSAR metadata offset overflow")?;
        anyhow::ensure!(end <= total && file_off >= 0x40 && length <= MAX_META, "BFSAR metadata/FILE outside bounded archive");
        for ty in [0x2000, 0x2001] {
            let (at, length) = *secs.get(&ty).context("STRG/INFO section missing")?;
            anyhow::ensure!(at.checked_add(length).is_some_and(|v| v <= file_off), "STRG/INFO section overlaps FILE");
        }
        let mut metadata = Vec::new();
        metadata.try_reserve_exact(length).context("allocating BFSAR metadata")?;
        metadata.resize(length, 0);
        file.seek(SeekFrom::Start(0))?;
        file.read_exact(&mut metadata).context("reading BFSAR metadata")?;
        Self::parse_with_file(metadata, Some(file))
    }

    pub fn parse(data: Vec<u8>) -> Result<Self> {
        Self::parse_with_file(data, None)
    }

    fn parse_with_file(data: Vec<u8>, backing: Option<File>) -> Result<Self> {
        let secs = sections(&data, b"FSAR")?;
        let (strg, _) = *secs.get(&0x2000).context("no STRG")?;
        let (info, _) = *secs.get(&0x2001).context("no INFO")?;
        let (file, file_size) = *secs.get(&0x2002).context("no FILE")?;
        let file_end = file.checked_add(file_size).context("FILE section overflow")?;

        // STRG: string table + patricia tree
        let sbody = strg + 8;
        let str_tab = sbody + u32_(&data, sbody + 4)? as usize;
        let tree = sbody + u32_(&data, sbody + 12)? as usize;
        let n_str = u32_(&data, str_tab)? as usize;
        let remaining = data.len().checked_sub(str_tab.checked_add(4).context("STRG offset overflow")?)
            .context("STRG string table outside archive")?;
        anyhow::ensure!(n_str <= remaining / 12, "STRG string count exceeds metadata");
        let mut strings = Vec::new();
        strings.try_reserve_exact(n_str).context("allocating STRG string index")?;
        for i in 0..n_str {
            let o = str_tab + 4 + i * 12;
            let off = u32_(&data, o + 4)? as usize;
            let size = u32_(&data, o + 8)? as usize;
            let s = data.get(str_tab + off..str_tab + off + size).context("bad string")?;
            let s = s.split(|&b| b == 0).next().unwrap_or_default();
            strings.push(String::from_utf8_lossy(s).to_ascii_lowercase());
        }
        let n_nodes = u32_(&data, tree + 4)? as usize;
        let remaining = data.len().checked_sub(tree.checked_add(8).context("STRG tree offset overflow")?)
            .context("STRG tree outside metadata")?;
        anyhow::ensure!(n_nodes <= remaining / 20, "STRG node count exceeds metadata");
        let mut names = HashMap::new();
        for i in 0..n_nodes {
            let o = tree + 8 + i * 20;
            let leaf = u16_(&data, o)? & 1 != 0;
            let sid = u32_(&data, o + 12)?;
            let item = u32_(&data, o + 16)?;
            if leaf && let Some(s) = strings.get(sid as usize) {
                names.insert(s.clone(), item);
            }
        }

        let ibody = info + 8;
        let refs = |ty: u16| -> Result<usize> {
            for k in 0..8 {
                let o = ibody + k * 8;
                if u16_(&data, o)? == ty {
                    return Ok(ibody + i32_(&data, o + 4)? as usize);
                }
            }
            bail!("INFO table {ty:#x} missing")
        };
        let sounds = ref_table(&data, refs(0x2100)?)?;
        let warcs = ref_table(&data, refs(0x2103)?)?;
        let files = ref_table(&data, refs(0x2106)?)?;
        let archive_len = if let Some(file) = &backing { usize::try_from(file.metadata()?.len()).context("BFSAR exceeds address space")? } else { data.len() };
        anyhow::ensure!(file_end <= archive_len, "FILE section exceeds archive");
        Ok(Self {
            data,
            backing: backing.map(Mutex::new),
            archive_len,
            file_end,
            names,
            info,
            file_body: file + 8,
            sounds,
            warcs,
            files,
        })
    }

    pub fn contains(&self, name: &str) -> bool {
        self.names.contains_key(&name.to_ascii_lowercase())
    }

    fn file_location(&self, id: u32) -> Result<(usize, usize)> {
        let fi = self.files.get(id as usize).copied().flatten().context("bad file id")?;
        if u16_(&self.data, fi)? != 0x220C { bail!("file {id} is external"); }
        let rel = i32_(&self.data, fi + 4)?;
        anyhow::ensure!(rel >= 0, "negative BFSAR file location");
        let loc = fi.checked_add(rel as usize).context("BFSAR file location overflow")?;
        let off = i32_(&self.data, loc + 4)?;
        let size = u32_(&self.data, loc + 8)? as usize;
        anyhow::ensure!(off >= 0, "file {id} not stored in archive");
        let start = self.file_body.checked_add(off as usize).context("BFSAR file offset overflow")?;
        anyhow::ensure!(start.checked_add(size).is_some_and(|e| e <= self.file_end && e <= self.archive_len), "BFSAR internal file out of range");
        Ok((start, size))
    }
    fn read_range(&self, start: usize, size: usize, max: usize) -> Result<Vec<u8>> {
        anyhow::ensure!(size <= max && start.checked_add(size).is_some_and(|e| e <= self.file_end && e <= self.archive_len), "BFSAR read exceeds budget or FILE bounds");
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(size).context("allocating bounded BFSAR read")?;
        bytes.resize(size, 0);
        if let Some(file) = &self.backing {
            let mut file = file.lock().map_err(|_| anyhow::anyhow!("BFSAR file lock poisoned"))?;
            file.seek(SeekFrom::Start(start as u64))?;
            file.read_exact(&mut bytes).context("reading BFSAR file range")?;
        } else {
            bytes.copy_from_slice(self.data.get(start..start + size).context("BFSAR range unavailable")?);
        }
        Ok(bytes)
    }

    fn file_wave(&self, id: u32, index: usize) -> Result<Vec<u8>> {
        const MAX_INDEX: usize = 1024 * 1024;
        const MAX_WAVE: usize = 8 * 1024 * 1024;
        let (start, file_size) = self.file_location(id)?;
        anyhow::ensure!(file_size >= 0x40, "FWAR header truncated");
        let header = self.read_range(start, 0x40, MAX_INDEX)?;
        let secs = sections(&header, b"FWAR")?;
        let (info, _) = *secs.get(&0x6800).context("FWAR INFO missing")?;
        let (data, _) = *secs.get(&0x6801).context("FWAR FILE missing")?;
        anyhow::ensure!(info.checked_add(12).is_some_and(|e| e <= file_size), "FWAR INFO outside file");
        let info_header = self.read_range(start + info, 12, MAX_INDEX)?;
        let count = u32_(&info_header, 8)? as usize;
        anyhow::ensure!(index < count, "FWAR wave index out of range");
        let entry_off = 12usize.checked_add(index.checked_mul(12).context("FWAR index multiplication overflow")?).context("FWAR index overflow")?;
        anyhow::ensure!(info.checked_add(entry_off + 12).is_some_and(|e| e <= file_size) && entry_off + 12 <= MAX_INDEX, "FWAR index exceeds budget");
        let entry = self.read_range(start + info + entry_off, 12, MAX_INDEX)?;
        let off = u32_(&entry, 4)? as usize;
        let size = u32_(&entry, 8)? as usize;
        let wave = data.checked_add(8).and_then(|x| x.checked_add(off)).context("FWAV offset overflow")?;
        anyhow::ensure!(wave.checked_add(size).is_some_and(|e| e <= file_size), "FWAV outside FWAR");
        self.read_range(start + wave, size, MAX_WAVE)
    }

    /// Bytes of an internal file by file id.
    fn file(&self, id: u32) -> Result<&[u8]> {
        let fi = self.files.get(id as usize).copied().flatten().context("bad file id")?;
        // file info: ref {u16 type, pad, i32 offset} to location
        let ty = u16_(&self.data, fi)?;
        if ty != 0x220C {
            bail!("file {id} is external");
        }
        let loc = fi + i32_(&self.data, fi + 4)? as usize;
        // internal location: ref to data (relative to FILE body), u32 size
        let off = i32_(&self.data, loc + 4)?;
        let size = u32_(&self.data, loc + 8)? as usize;
        if off < 0 {
            bail!("file {id} not stored in archive");
        }
        let start = self.file_body + off as usize;
        self.data.get(start..start + size).context("file out of range")
    }

    /// Decode a wave sound by name (e.g. `WSD_moon_se03001`).
    pub fn load_wave_sound(&self, name: &str) -> Result<Pcm> {
        let item = *self.names.get(&name.to_ascii_lowercase()).with_context(|| format!("sound {name} not found"))?;
        self.load_wave_sound_id(item).with_context(|| format!("sound {name}"))
    }

    /// Decode a wave sound by item id (`0x01xxxxxx`); some sounds (system sounds 1 and 7) have no name.
    pub fn load_wave_sound_id(&self, item: u32) -> Result<Pcm> {
        if item >> 24 != 1 {
            bail!("item {item:#x} is not a sound");
        }
        let si = self.sounds.get((item & 0xFFFFFF) as usize).copied().flatten().context("bad sound index")?;
        let file_id = u32_(&self.data, si)?;
        // detail reference (wave sound info, 0x2202) at +12: its first field is the sound's index in the FWSD
        if u16_(&self.data, si + 12)? != 0x2202 {
            bail!("item {item:#x} is not a wave sound");
        }
        let detail = si + i32_(&self.data, si + 16)? as usize;
        let index = u32_(&self.data, detail)? as usize;
        let (warc_item, wave_index) = if self.backing.is_some() {
            const MAX_FWSD: usize = 1024 * 1024;
            let (start, size) = self.file_location(file_id)?;
            let fwsd = self.read_range(start, size, MAX_FWSD)?;
            fwsd_wave(&fwsd, index)?
        } else {
            fwsd_wave(self.file(file_id)?, index)?
        };
        let wi = self.warcs.get((warc_item & 0xFFFFFF) as usize).copied().flatten().context("bad warc")?;
        let warc_file = u32_(&self.data, wi)?;
        let fwav = if self.backing.is_some() {
            Some(self.file_wave(warc_file, wave_index as usize)?)
        } else { None };
        // the sound's initial volume (si + 8) is 96 (of 127) for every sound, streams too; the Switch output
        // (Astris capture) has a voice at the config volume times the pan law and an SE at sub_CA0 of its volume
        // with no 96 / 127 on either, so knife leaves it out everywhere
        if let Some(fwav) = fwav { decode_fwav_file_bounded(&fwav) }
        else { decode_fwav(fwar_wave(self.file(warc_file)?, wave_index as usize)?) }
    }

    #[allow(dead_code)]
    fn info_offset(&self) -> usize {
        self.info
    }

    pub fn sound_count(&self) -> usize {
        self.sounds.len()
    }

    /// External path, loop range (in frames) and volume (0..1) of a stream sound by index.
    pub fn stream_sound(&self, index: usize) -> Result<StreamSound> {
        let si = self.sounds.get(index).copied().flatten().context("bad sound index")?;
        let file_id = u32_(&self.data, si)?;
        let volume = u8_(&self.data, si + 8)? as f32 / 127.0;
        let detail_ty = u16_(&self.data, si + 0x0C)?;
        if detail_ty != 0x2201 {
            bail!("sound {index} is not a stream sound");
        }
        let detail = si + i32_(&self.data, si + 0x10)? as usize;
        let mut loop_range = None;
        if u16_(&self.data, detail + 24)? != 0 {
            let ext = detail + i32_(&self.data, detail + 28)? as usize;
            let start = u32_(&self.data, ext + 4)?;
            let end = u32_(&self.data, ext + 8)?;
            if end > start {
                loop_range = Some((start as usize, end as usize));
            }
        }
        let fi = self.files.get(file_id as usize).copied().flatten().context("bad file id")?;
        if u16_(&self.data, fi)? != 0x220D {
            bail!("stream file is not external");
        }
        let loc = fi + i32_(&self.data, fi + 4)? as usize;
        let end = self.data[loc..].iter().position(|&b| b == 0).context("bad path")?;
        let path = String::from_utf8_lossy(&self.data[loc..loc + end]).into_owned();
        Ok(StreamSound { path, loop_range, volume })
    }
}

/// The wave (wave archive item, index in it) of wave sound `index` of an FWSD: its first note's wave id.
/// An FWSD holds several sounds, so the index from the sound's info picks the right one.
fn fwsd_wave(d: &[u8], index: usize) -> Result<(u32, u32)> {
    let secs = sections(d, b"FWSD")?;
    let (info, _) = *secs.get(&0x6800).context("FWSD INFO missing")?;
    let body = info + 8;
    // refs: wave id table (0x0100), wave sound data table (0x0101)
    let ids = body + i32_(d, body + 4)? as usize;
    let sounds = ref_table(d, body + i32_(d, body + 12)? as usize)?;
    let wsd = sounds.get(index).copied().flatten().context("FWSD sound index out of range")?;
    // wave sound data refs: info (+0), track table (+8), note table (+16)
    let notes = ref_table(d, wsd + i32_(d, wsd + 20)? as usize)?;
    let note = notes.first().copied().flatten().context("FWSD sound without notes")?;
    let id = u32_(d, note)? as usize;
    if id >= u32_(d, ids)? as usize {
        bail!("FWSD wave id out of range");
    }
    Ok((u32_(d, ids + 4 + id * 8)?, u32_(d, ids + 8 + id * 8)?))
}

fn fwar_wave(d: &[u8], index: usize) -> Result<&[u8]> {
    let secs = sections(d, b"FWAR")?;
    let (info, _) = *secs.get(&0x6800).context("FWAR INFO missing")?;
    let (file, _) = *secs.get(&0x6801).context("FWAR FILE missing")?;
    let n = u32_(d, info + 8)? as usize;
    if index >= n {
        bail!("wave index out of range");
    }
    let e = info + 12 + index * 12;
    let off = u32_(d, e + 4)? as usize;
    let size = u32_(d, e + 8)? as usize;
    let start = file + 8 + off;
    d.get(start..start + size).context("FWAV out of range")
}

/// A file-backed wave is decoded to PCM only while its declared output stays bounded.
/// Longer wave ambience needs a streaming FWAV API and is reported, never OOM-decoded.
fn decode_fwav_file_bounded(data: &[u8]) -> Result<Pcm> {
    const MAX_FRAMES: usize = 2_000_000;
    let secs = sections(data, b"FWAV")?;
    let (info, _) = *secs.get(&0x7000).context("FWAV INFO missing")?;
    let body = info.checked_add(8).context("FWAV INFO overflow")?;
    let frames = u32_(data, body + 12)? as usize;
    let channels = u32_(data, body + 20)? as usize;
    anyhow::ensure!(matches!(channels, 1 | 2) && frames <= MAX_FRAMES,
        "FWAV PCM output exceeds Vita bounded decoder (frames={frames}, channels={channels})");
    decode_fwav(data)
}

pub fn decode_fwav(d: &[u8]) -> Result<Pcm> {
    let secs = sections(d, b"FWAV")?;
    let (info, _) = *secs.get(&0x7000).context("FWAV INFO missing")?;
    let (data, _) = *secs.get(&0x7001).context("FWAV DATA missing")?;
    let b = info + 8;
    let encoding = u8_(d, b)?;
    let looping = u8_(d, b + 1)? != 0;
    let rate = u32_(d, b + 4)?;
    let loop_start = u32_(d, b + 8)? as usize;
    let samples = u32_(d, b + 12)? as usize;
    // A corrupt or truncated entry otherwise allocates whatever the header claims and
    // aborts inside the allocator (observed on hardware as an undefined-instruction
    // exception with no panic message).
    const MAX_SAMPLES: usize = 4_000_000;
    anyhow::ensure!(samples <= MAX_SAMPLES, "FWAV sample count {samples} exceeds the bounded decoder");
    anyhow::ensure!(encoding <= 2, "unsupported FWAV encoding {encoding}");
    anyhow::ensure!(u32_(d, b + 20)? <= 2, "FWAV has too many channels");
    // Resolve both channels before allocating the output. Channel metadata is
    // tiny; channel-length float arrays previously doubled the stereo peak.
    let mut channels = [None, None];
    let mut channel_count = 0;
    for ci in ref_table(d, b + 20)?.into_iter().flatten() {
        let sample_off = data.checked_add(8).and_then(|x| x.checked_add(i32_(d, ci + 4).ok()? as usize))
            .context("bad sample offset")?;
        let sdata = d.get(sample_off..).context("bad sample offset")?;
        let mut coefs = [0i16; 16];
        let mut history = (0, 0);
        if encoding == 2 {
            let adpcm = ci.checked_add(i32_(d, ci + 12)? as usize).context("ADPCM offset overflow")?;
            for (k, c) in coefs.iter_mut().enumerate() { *c = u16_(d, adpcm + k * 2)? as i16; }
            history = (u16_(d, adpcm + 34)? as i16, u16_(d, adpcm + 36)? as i16);
        }
        channels[channel_count] = Some((sdata, coefs, history));
        channel_count += 1;
    }
    anyhow::ensure!(channel_count != 0, "FWAV without channels");
    let mut frames = Vec::new();
    frames.try_reserve_exact(samples).context("FWAV PCM allocation")?;
    frames.resize(samples, [0.0; 2]);
    for (channel, (sdata, coefs, history)) in channels.into_iter().flatten().enumerate() {
        match encoding {
            2 => decode_dsp_into(sdata, samples, &coefs, history,
                |i, sample| frames[i][channel] = sample),
            1 => for (i, frame) in frames.iter_mut().enumerate() {
                frame[channel] = u16_(sdata, i * 2).map(|v| v as i16 as f32 / 32768.0).unwrap_or(0.0);
            },
            _ => for (i, frame) in frames.iter_mut().enumerate() {
                frame[channel] = sdata.get(i).map(|&v| v as i8 as f32 / 128.0).unwrap_or(0.0);
            },
        }
    }
    if channel_count == 1 {
        for frame in &mut frames { *frame = [frame[0] * std::f32::consts::FRAC_1_SQRT_2; 2]; }
    }
    Ok(Pcm {
        sample_rate: rate,
        frames,
        loop_range: looping.then_some((loop_start, samples)),
        mono: channel_count == 1,
    })
}


#[cfg(test)]
mod tests {
    /// Run with KNIFE_WITCH_BFSAR=/absolute/path/witch.bfsar to compare real bytes.
    #[test]
    fn real_witch_file_backed_matches_full_archive_and_rejects_bad_bounds() {
        let Some(path) = std::env::var_os("KNIFE_WITCH_BFSAR") else { return; };
        let path = std::path::PathBuf::from(path);
        let mut bounded = super::SoundArchive::open_file(&path).unwrap();
        assert_eq!(bounded.data.len(), 3_163_944);
        assert_eq!(bounded.archive_len, 219_087_752);
        let full = super::SoundArchive::parse(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(bounded.sound_count(), full.sound_count());
        for item in [0x10024fe, 0x10024ff, 0x1002509] {
            let a = bounded.load_wave_sound_id(item).unwrap();
            let b = full.load_wave_sound_id(item).unwrap();
            assert_eq!(a.frames, b.frames, "wave {item:#x}");
            assert_eq!(a.sample_rate, b.sample_rate);
            assert_eq!(a.loop_range, b.loop_range);
            assert_eq!(a.mono, b.mono);
        }
        for index in [0, 1, 100] {
            let a = bounded.stream_sound(index).unwrap();
            let b = full.stream_sound(index).unwrap();
            assert_eq!(a.path, b.path);
            assert_eq!(a.loop_range, b.loop_range);
            assert_eq!(a.volume, b.volume);
        }
        let long_wave = bounded.file_wave(1722, 0).unwrap();
        assert!(super::decode_fwav_file_bounded(&long_wave).unwrap_err().to_string().contains("bounded decoder"));
        let sbody = 64 + 8;
        let str_tab = sbody + super::u32_(&bounded.data, sbody + 4).unwrap() as usize;
        let mut corrupt = bounded.data.clone();
        corrupt[str_tab..str_tab + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(super::SoundArchive::parse(corrupt).err().unwrap().to_string().contains("string count"));
        let fi = bounded.files[0].unwrap();
        let loc = fi + super::i32_(&bounded.data, fi + 4).unwrap() as usize;
        bounded.data[loc + 8..loc + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(bounded.load_wave_sound_id(0x10024fe).is_err());
    }
    /// Wave sounds that share an FWSD each decode their own wave (system sounds 0x10040d4.. live in one).
    #[test]
    fn wave_sounds_sharing_a_file_differ() {
        let Ok(data) = std::fs::read("../switch_romfs/knife1.bfsar") else { return };
        let sar = super::SoundArchive::parse(data).unwrap();
        let lens: Vec<_> = (0x10040d4..=0x10040dc).map(|id| sar.load_wave_sound_id(id).unwrap().frames.len()).collect();
        let mut distinct = lens.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), lens.len(), "{lens:?}");
    }
}

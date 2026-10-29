//! Decoders: Nintendo Opus (`.opus` in `stream/`) and DSP-ADPCM (sound effects in the BFSAR).

use anyhow::{Context, Result, bail, ensure};
#[cfg(target_os = "vita")]
use crate::vita_opus as opus;

/// Decode either an original Nintendo stream or a compact standard Ogg Opus stream.
pub fn decode_stream(data: &[u8]) -> Result<Pcm> {
    decode_stream_part(data, 0, usize::MAX).map(|(_, pcm)| pcm)
}

pub fn decode_stream_part(data: &[u8], from: usize, to: usize) -> Result<(usize, Pcm)> {
    if data.starts_with(b"OggS") { decode_ogg_part(data, from, to) } else { decode_nopus_part(data, from, to) }
}

/// Ogg granule positions count 48 kHz samples before pre-skip; the EOS granule trims encoder padding.
fn decode_ogg_part(data: &[u8], from: usize, to: usize) -> Result<(usize, Pcm)> {
    use ogg::reading::PacketReader;
    let mut reader = PacketReader::new(std::io::Cursor::new(data));
    let head = reader.read_packet_expected().context("Ogg Opus header")?;
    ensure!(head.first_in_stream() && head.data.len() >= 19 && head.data.starts_with(b"OpusHead"), "missing OpusHead");
    ensure!(head.data[8] == 1 && head.data[18] == 0 && matches!(head.data[9], 1 | 2), "unsupported Opus channel mapping");
    let channels = usize::from(head.data[9]);
    let pre_skip = usize::from(u16::from_le_bytes(head.data[10..12].try_into()?));
    let gain = f32::from(i16::from_le_bytes(head.data[16..18].try_into()?)) / 256.0;
    let gain = 10f32.powf(gain / 20.0);
    let serial = head.stream_serial();
    let tags = reader.read_packet_expected().context("Ogg Opus tags")?;
    ensure!(tags.stream_serial() == serial && tags.data.starts_with(b"OpusTags"), "missing OpusTags");
    let mut packets = Vec::new();
    let mut samples = 0usize;
    let mut final_granule = None;
    while let Some(packet) = reader.read_packet()? {
        ensure!(packet.stream_serial() == serial && final_granule.is_none(), "chained or multiplexed Ogg streams are unsupported");
        let n = opus::packet::get_nb_samples(&packet.data, 48_000)?;
        samples = samples.checked_add(n).context("Opus sample count overflow")?;
        if packet.last_in_stream() {
            let end = usize::try_from(packet.absgp_page()).context("Opus granule exceeds address space")?;
            ensure!(end >= pre_skip && end <= samples && end >= samples.saturating_sub(n), "invalid Opus EOS granule");
            final_granule = Some(end);
        }
        packets.push((packet.data, n));
    }
    let end = final_granule.context("Ogg Opus stream is missing EOS")?;
    let keep = from.saturating_add(pre_skip);
    let stop = to.saturating_add(pre_skip).min(end).max(keep.min(end));
    if keep >= stop {
        return Ok((from, Pcm { sample_rate: 48_000, frames: Vec::new(), loop_range: None, mono: channels == 1 }));
    }
    let roll = keep.saturating_sub(48_000);
    let ch = if channels == 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
    let mut dec = opus::Decoder::new(48_000, ch)?;
    let mut buf = vec![0f32; 5760 * channels];
    let mut frames = Vec::with_capacity(stop.saturating_sub(keep));
    let mut pos = 0usize;
    for (packet, count) in packets {
        if pos >= stop { break; }
        if pos + count <= roll { pos += count; continue; }
        let n = dec.decode_float(&packet, &mut buf, false)?;
        ensure!(n == count, "Opus decoded sample count mismatch");
        let a = keep.clamp(pos, pos + n) - pos;
        let b = stop.clamp(pos, pos + n) - pos;
        if channels == 1 {
            let scale = gain * std::f32::consts::FRAC_1_SQRT_2;
            frames.extend(buf[a..b].iter().map(|&v| [v * scale; 2]));
        } else {
            let scale = gain * STEREO_STREAM_GAIN;
            frames.extend(buf[a * 2..b * 2].as_chunks::<2>().0.iter().map(|v| [v[0] * scale, v[1] * scale]));
        }
        pos += n;
    }
    Ok((from, Pcm { sample_rate: 48_000, frames, loop_range: None, mono: channels == 1 }))
}

/// Decoded interleaved stereo audio.
#[derive(Clone, Debug, Default)]
pub struct Pcm {
    pub sample_rate: u32,
    /// Stereo frames (left, right).
    pub frames: Vec<[f32; 2]>,
    /// Loop region in frames, if the source loops.
    pub loop_range: Option<(usize, usize)>,
    /// a mono source (both sides already at 1 / sqrt 2): nn::atk pans its one channel
    pub mono: bool,
}

fn u32le(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(d.get(o..o + 4).context("truncated")?.try_into()?))
}
fn u32be(d: &[u8], o: usize) -> Result<u32> {
    Ok(u32::from_be_bytes(d.get(o..o + 4).context("truncated")?.try_into()?))
}

/// Gain of a stereo stream's channels at the centre in nn::atk's balance pan mode (every stream sound has pan
/// option 1, wave sounds 0 = dual, which keeps stereo at full level). Measured on the Switch (Astris capture): BGM
/// 26 and 72 at 0.4 and BGM 35 at 0.32 come out at 0.678 / 0.672 / 0.688 of their sub_CA0 volume, without cross-mix.
pub const STEREO_STREAM_GAIN: f32 = 0.68;

/// Nintendo Opus: header chunk `0x80000001`, data chunk `0x80000004` holding
/// `u32be size, u32be final_range, packet` records.
pub fn decode_nopus(data: &[u8]) -> Result<Pcm> {
    decode_nopus_part(data, 0, usize::MAX).map(|(_, pcm)| pcm)
}

/// Frames `from..to` of a Nintendo Opus file (after its pre-skip) and the index of the first one. Decoding starts
/// a second before `from`: after Opus's nominal 80 ms pre-roll the decoder still differs by up to 0.01 from a
/// whole-file decode, and only matches it exactly after about 0.4 s.
pub fn decode_nopus_part(data: &[u8], from: usize, to: usize) -> Result<(usize, Pcm)> {
    if u32le(data, 0)? != 0x8000_0001 {
        bail!("not a Nintendo Opus file");
    }
    let channels = data[9] as usize;
    let sample_rate = u32le(data, 0x0C)?;
    let data_off = u32le(data, 0x10)? as usize;
    let pre_skip = u32le(data, 0x1C)? as usize;
    if u32le(data, data_off)? != 0x8000_0004 {
        bail!("Nintendo Opus data chunk missing");
    }
    let data_len = u32le(data, data_off + 4)? as usize;
    let mut p = data_off + 8;
    let end = (p + data_len).min(data.len());
    // decoded sample indices: the kept range, and where decoding starts (the packet holding the pre-roll point)
    let (keep, stop) = (from.saturating_add(pre_skip), to.saturating_add(pre_skip));
    let roll = keep.saturating_sub(sample_rate as usize);
    let mut pos = 0;
    while roll > 0 && p + 8 <= end {
        let size = u32be(data, p)? as usize;
        let Some(packet) = data.get(p + 8..p + 8 + size) else { break };
        let n = opus::packet::get_nb_samples(packet, sample_rate)?;
        if pos + n > roll {
            break;
        }
        pos += n;
        p += 8 + size;
    }
    let ch = if channels == 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
    let mut dec = opus::Decoder::new(sample_rate, ch)?;
    let mut buf = vec![0f32; 5760 * channels.max(1)];
    let mut frames = Vec::new();
    while p + 8 <= end && pos < stop {
        let size = u32be(data, p)? as usize;
        p += 8;
        let Some(packet) = data.get(p..p + size) else { break };
        p += size;
        let n = dec.decode_float(packet, &mut buf, false)?;
        let (a, b) = (keep.clamp(pos, pos + n) - pos, stop.clamp(pos, pos + n) - pos);
        pos += n;
        if channels == 1 {
            // nn::atk pans a mono stream to the centre with the sqrt curve: 1 / sqrt 2 on each side (voices
            // capture at 0.70 of their volume on the Switch)
            frames.extend(buf[a..b].iter().map(|&s| [s * std::f32::consts::FRAC_1_SQRT_2; 2]));
        } else {
            frames.extend(buf[a * 2..b * 2].as_chunks::<2>().0.iter().map(|c| [c[0] * STEREO_STREAM_GAIN, c[1] * STEREO_STREAM_GAIN]));
        }
    }
    Ok((
        from,
        Pcm {
            sample_rate,
            frames,
            loop_range: None,
            mono: channels == 1,
        },
    ))
}

/// Decode one channel of GameCube/Wii/Switch DSP-ADPCM.
pub fn decode_dsp(data: &[u8], samples: usize, coefs: &[i16; 16], hist: (i16, i16)) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples);
    let (mut h1, mut h2) = (hist.0 as i32, hist.1 as i32);
    let mut i = 0;
    'frames: for frame in data.chunks(8) {
        let header = frame[0];
        let scale = 1i32 << (header & 0xF);
        let ci = ((header >> 4) & 7) as usize;
        let (c1, c2) = (coefs[ci * 2] as i32, coefs[ci * 2 + 1] as i32);
        for &b in &frame[1..] {
            for nib in [b >> 4, b & 0xF] {
                if i >= samples {
                    break 'frames;
                }
                let n = if nib >= 8 { nib as i32 - 16 } else { nib as i32 };
                let s = ((n * scale) << 11) + 1024 + c1 * h1 + c2 * h2;
                let s = (s >> 11).clamp(-32768, 32767);
                h2 = h1;
                h1 = s;
                out.push(s as f32 / 32768.0);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    fn ogg_fixture(channels: usize) -> Vec<u8> {
        use ogg::writing::{PacketWriteEndInfo::*, PacketWriter};
        let ch = if channels == 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
        let mut encoder = opus::Encoder::new(48_000, ch, opus::Application::Audio).unwrap();
        let mut writer = PacketWriter::new(Vec::new());
        let mut head = b"OpusHead".to_vec();
        head.extend_from_slice(&[1, channels as u8]);
        head.extend_from_slice(&312u16.to_le_bytes());
        head.extend_from_slice(&48_000u32.to_le_bytes());
        head.extend_from_slice(&[0, 0, 0]);
        writer.write_packet(head, 7, EndPage, 0).unwrap();
        writer.write_packet(b"OpusTags\0\0\0\0\0\0\0\0".to_vec(), 7, EndPage, 0).unwrap();
        for i in 0..160usize {
            let input: Vec<f32> = (0..960 * channels).map(|j| (((i * 960 + j / channels) as f32) * 0.0576).sin() * 0.1).collect();
            let packet = encoder.encode_vec_float(&input, 4000).unwrap();
            let granule = (i + 1) * 960 - if i == 159 { 123 } else { 0 };
            writer.write_packet(packet, 7, if i == 159 { EndStream } else { EndPage }, granule as u64).unwrap();
        }
        writer.into_inner()
    }

    #[test]
    fn ogg_preserves_preskip_end_trim_and_partial_bgm_decode() {
        for channels in [1, 2] {
            let data = ogg_fixture(channels);
            let whole = super::decode_stream(&data).unwrap();
            assert_eq!(whole.frames.len(), 160 * 960 - 123 - 312);
            assert_eq!(whole.mono, channels == 1);
            assert!(super::decode_stream_part(&data, whole.frames.len() + 100, usize::MAX).unwrap().1.frames.is_empty());
            for from in [0, 1, 96_123, whole.frames.len() - 100] {
                let to = (from + 1000).min(whole.frames.len());
                let (base, part) = super::decode_stream_part(&data, from, to).unwrap();
                assert_eq!(base, from);
                assert_eq!(part.frames, whole.frames[from..to], "partial decode at {from}");
            }
            for cut in [0, 28, 60, data.len() - 1] {
                assert!(super::decode_stream(&data[..cut]).is_err());
            }
            let mut corrupt = data;
            *corrupt.last_mut().unwrap() ^= 1;
            assert!(super::decode_stream(&corrupt).is_err());
        }
    }

    /// A part decoded from inside a stream (after the pre-roll) is the same frames as the whole decode.
    #[test]
    fn part_matches_whole() {
        let Ok(data) = std::fs::read("../switch_romfs/stream/MOON01_BGM01.opus") else { return };
        let whole = super::decode_nopus(&data).unwrap();
        for from in [0, 1_340_383] {
            let (base, part) = super::decode_nopus_part(&data, from, from + 48_000).unwrap();
            assert_eq!((base, part.frames.len()), (from, 48_000));
            let err = part.frames.iter().zip(&whole.frames[from..]).map(|(a, b)| (a[0] - b[0]).abs().max((a[1] - b[1]).abs())).fold(0f32, f32::max);
            assert_eq!(err, 0.0, "from {from}");
        }
    }
}

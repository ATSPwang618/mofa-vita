//! Bounded file-backed Nintendo Opus decoder for Vita stream voices.
//! The original stream is a physical romfs file, not an .mrg archive entry.
use anyhow::{Context, Result, ensure};
#[cfg(target_os = "vita")]
use crate::vita_opus as opus;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

const MAX_PACKET: usize = 8 * 1024;
const MAX_FRAMES: usize = 5_760;
const PRE_ROLL: u64 = 48_000;

pub(crate) struct NintendoStream {
    file: File,
    data_start: u64,
    data_end: u64,
    pre_skip: u64,
    raw_end: u64,
    channels: usize,
    decoder: opus::Decoder,
    packet: Vec<u8>,
    decoded: Vec<f32>,
    next_packet: u64,
    decoded_end: u64,
    filled: usize,
    cursor: usize,
    target_raw: u64,
}
impl NintendoStream {
    pub(crate) fn open(mut file: File) -> Result<Self> {
        file.seek(SeekFrom::Start(0))?;
        let file_len = file.metadata()?.len();
        ensure!(file_len >= 0x24, "truncated Nintendo Opus header");
        let mut header = [0u8; 0x24];
        file.read_exact(&mut header)?;
        ensure!(
            u32::from_le_bytes(header[0..4].try_into()?) == 0x8000_0001,
            "not Nintendo Opus"
        );
        let channels = usize::from(header[9]);
        ensure!(
            matches!(channels, 1 | 2),
            "unsupported Nintendo Opus channel count"
        );
        ensure!(
            u32::from_le_bytes(header[12..16].try_into()?) == 48_000,
            "Nintendo Opus requires 48 kHz"
        );
        let data_off = u64::from(u32::from_le_bytes(header[16..20].try_into()?));
        let pre_skip = u64::from(u32::from_le_bytes(header[28..32].try_into()?));
        ensure!(
            data_off >= 0x20 && data_off.checked_add(8).is_some_and(|v| v <= file_len),
            "Nintendo Opus data offset outside file"
        );
        file.seek(SeekFrom::Start(data_off))?;
        let mut chunk = [0u8; 8];
        file.read_exact(&mut chunk)?;
        ensure!(
            u32::from_le_bytes(chunk[..4].try_into()?) == 0x8000_0004,
            "Nintendo Opus DATA chunk missing"
        );
        let data_start = data_off + 8;
        let data_end = data_start
            .checked_add(u64::from(u32::from_le_bytes(chunk[4..].try_into()?)))
            .context("Nintendo Opus DATA length overflow")?;
        ensure!(data_end <= file_len, "truncated Nintendo Opus DATA chunk");
        let decoder = opus::Decoder::new(
            48_000,
            if channels == 1 {
                opus::Channels::Mono
            } else {
                opus::Channels::Stereo
            },
        )?;
        let mut packet = Vec::new();
        packet
            .try_reserve_exact(MAX_PACKET)
            .context("allocating Opus packet buffer")?;
        packet.resize(MAX_PACKET, 0);
        let mut decoded = Vec::new();
        decoded
            .try_reserve_exact(MAX_FRAMES * 2)
            .context("allocating Opus PCM block")?;
        decoded.resize(MAX_FRAMES * 2, 0.0);
        let mut stream = Self {
            file,
            data_start,
            data_end,
            pre_skip,
            raw_end: 0,
            channels,
            decoder,
            packet,
            decoded,
            next_packet: data_start,
            decoded_end: 0,
            filled: 0,
            cursor: 0,
            target_raw: pre_skip,
        };
        // Validate the complete physical packet range with one reusable 8 KiB packet buffer.
        let mut at = data_start;
        while at < data_end {
            let (next, n) = stream.inspect_packet(at)?;
            stream.raw_end = stream
                .raw_end
                .checked_add(n as u64)
                .context("Opus sample count overflow")?;
            at = next;
        }
        ensure!(
            stream.raw_end >= pre_skip,
            "Nintendo Opus pre-skip exceeds samples"
        );
        stream.seek(0)?;
        Ok(stream)
    }
    fn inspect_packet(&mut self, at: u64) -> Result<(u64, usize)> {
        ensure!(
            at.checked_add(8).is_some_and(|v| v <= self.data_end),
            "truncated Nintendo Opus packet header"
        );
        self.file.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 8];
        self.file.read_exact(&mut header)?;
        let size = u32::from_be_bytes(header[..4].try_into()?) as usize;
        ensure!(
            size > 0 && size <= MAX_PACKET,
            "Nintendo Opus packet exceeds 8 KiB"
        );
        let next = at
            .checked_add(8 + size as u64)
            .context("Nintendo Opus packet offset overflow")?;
        ensure!(next <= self.data_end, "truncated Nintendo Opus packet");
        self.file.read_exact(&mut self.packet[..size])?;
        let n = opus::packet::get_nb_samples(&self.packet[..size], 48_000)?;
        ensure!(
            n > 0 && n <= MAX_FRAMES,
            "invalid Nintendo Opus frame count"
        );
        Ok((next, n))
    }
    pub(crate) fn channels(&self) -> u32 {
        self.channels as u32
    }
    pub(crate) fn total_frames(&self) -> u64 {
        self.raw_end - self.pre_skip
    }
    pub(crate) fn seek(&mut self, frame: u64) -> Result<()> {
        ensure!(
            frame <= self.total_frames(),
            "Nintendo Opus seek exceeds source"
        );
        let raw = frame
            .checked_add(self.pre_skip)
            .context("Nintendo Opus seek overflow")?;
        let roll = raw.saturating_sub(PRE_ROLL);
        let mut pos = 0u64;
        let mut at = self.data_start;
        while at < self.data_end && pos < roll {
            let (next, n) = self.inspect_packet(at)?;
            if pos + n as u64 > roll {
                break;
            }
            pos += n as u64;
            at = next;
        }
        self.decoder = opus::Decoder::new(
            48_000,
            if self.channels == 1 {
                opus::Channels::Mono
            } else {
                opus::Channels::Stereo
            },
        )?;
        self.next_packet = at;
        self.decoded_end = pos;
        self.filled = 0;
        self.cursor = 0;
        self.target_raw = raw;
        Ok(())
    }
    /// Read no more than 960 stereo frames; decoding remains bounded to one packet.
    pub(crate) fn read(&mut self, out: &mut [[f32; 2]]) -> Result<usize> {
        ensure!(
            !out.is_empty() && out.len() <= 960,
            "Nintendo Opus read exceeds block"
        );
        let mut written = 0;
        while written < out.len() {
            if self.cursor == self.filled {
                if self.next_packet == self.data_end {
                    break;
                }
                let (next, expected) = self.inspect_packet(self.next_packet)?;
                let packet_len = (next - self.next_packet - 8) as usize;
                let n = self.decoder.decode_float(
                    &self.packet[..packet_len],
                    &mut self.decoded[..MAX_FRAMES * self.channels],
                    false,
                )?;
                ensure!(n == expected, "Nintendo Opus decoder frame mismatch");
                self.next_packet = next;
                self.decoded_end += n as u64;
                self.filled = n;
                self.cursor = 0;
            }
            let raw = self.decoded_end - self.filled as u64 + self.cursor as u64;
            if raw >= self.target_raw {
                let sample = if self.channels == 1 {
                    [self.decoded[self.cursor] * std::f32::consts::FRAC_1_SQRT_2; 2]
                } else {
                    let i = self.cursor * 2;
                    [
                        self.decoded[i] * crate::codec::STEREO_STREAM_GAIN,
                        self.decoded[i + 1] * crate::codec::STEREO_STREAM_GAIN,
                    ]
                };
                out[written] = sample;
                written += 1;
            }
            self.cursor += 1;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(channels: usize, packets: usize) -> Vec<u8> {
        let mut encoder = opus::Encoder::new(
            48_000,
            if channels == 1 {
                opus::Channels::Mono
            } else {
                opus::Channels::Stereo
            },
            opus::Application::Audio,
        )
        .unwrap();
        let mut out = vec![0u8; 0x28];
        out[0..4].copy_from_slice(&0x8000_0001u32.to_le_bytes());
        out[9] = channels as u8;
        out[12..16].copy_from_slice(&48_000u32.to_le_bytes());
        out[16..20].copy_from_slice(&0x20u32.to_le_bytes());
        out[28..32].copy_from_slice(&312u32.to_le_bytes());
        out[0x20..0x24].copy_from_slice(&0x8000_0004u32.to_le_bytes());
        for p in 0..packets {
            let input = (0..960 * channels)
                .map(|i| (((p * 960 + i / channels) as f32) * 0.035).sin() * 0.1)
                .collect::<Vec<_>>();
            let encoded = encoder.encode_vec_float(&input, 4000).unwrap();
            out.extend_from_slice(&(encoded.len() as u32).to_be_bytes());
            out.extend_from_slice(&0u32.to_be_bytes());
            out.extend_from_slice(&encoded);
        }
        let len = (out.len() - 0x28) as u32;
        out[0x24..0x28].copy_from_slice(&len.to_le_bytes());
        out
    }
    fn temp_file(data: &[u8]) -> (File, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "knife-nopus-{}-{}.opus",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::write(&path, data).unwrap();
        let file = File::open(&path).unwrap();
        (file, path)
    }
    #[test]
    fn bounded_stream_matches_whole_and_seek_with_preroll() {
        for channels in [1, 2] {
            let data = fixture(channels, 110);
            let full = crate::codec::decode_nopus(&data).unwrap();
            let (mut file, path) = temp_file(&data);
            file.seek(SeekFrom::Start(4)).unwrap(); // format sniff consumed the first four bytes
            let mut s = NintendoStream::open(file).unwrap();
            assert_eq!(s.total_frames() as usize, full.frames.len());
            assert_eq!(s.channels(), channels as u32);
            let mut block = [[0.0; 2]; 960];
            for start in [0, 42, 67_213, full.frames.len() - 1, full.frames.len()] {
                s.seek(start as u64).unwrap();
                let n = s.read(&mut block).unwrap();
                assert_eq!(n, (full.frames.len() - start).min(960));
                assert_eq!(&block[..n], &full.frames[start..start + n]);
            }
            drop(s);
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn rejects_truncation_and_range() {
        let data = fixture(1, 5);
        for cut in [0, 0x20, 0x27, data.len() - 1] {
            let (file, path) = temp_file(&data[..cut]);
            assert!(NintendoStream::open(file).is_err());
            std::fs::remove_file(path).unwrap();
        }
        let (file, path) = temp_file(&data);
        let mut s = NintendoStream::open(file).unwrap();
        assert!(s.seek(s.total_frames() + 1).is_err());
        drop(s);
        std::fs::remove_file(path).unwrap();
    }
}

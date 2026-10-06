//! Bounded ATRAC9 WAVE demux and sample-exact streaming. Codec execution belongs
//! to the host; the Vita host decodes one superframe per hardware request.
use crate::{DecodedFrame, Format, Result, StreamDecoder};
use krkr_protocol::budget::{Budget, Permit};
use std::{
    io::{BufReader, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

const GUID: [u8; 16] = [
    0xd2, 0x42, 0xe1, 0x47, 0xba, 0x36, 0x8d, 0x4d, 0x88, 0xfc, 0x61, 0x65, 0x4f, 0x8c, 0x83, 0x6c,
];
fn u16le(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn read(input: &mut (impl Read + ?Sized), data: &mut [u8]) -> Result<()> {
    input.read_exact(data).map_err(|e| format!("AT9 read: {e}"))
}

#[derive(Clone, Copy, Debug)]
pub struct Header {
    pub format: Format,
    pub config: [u8; 4],
    /// Hardware codec rate; the optional krSR chunk retains the script rate.
    pub codec_rate: u32,
    pub data_offset: u64,
    pub data_bytes: u64,
    pub block_bytes: u32,
    pub block_frames: u32,
    pub frame_samples: u32,
    pub delay: u32,
}
impl Header {
    pub fn block_samples(&self) -> u32 {
        self.frame_samples * self.block_frames
    }
    /// Start far enough before the requested sample to rebuild transform overlap.
    /// The first frame after reset is discarded, including seeks at block edges.
    pub fn seek(&self, sample: u64) -> Result<(u64, u64)> {
        if sample > self.format.frames {
            return Err("AT9 seek outside stream".into());
        }
        let encoded = sample + u64::from(self.delay);
        let block =
            encoded.saturating_sub(u64::from(self.frame_samples)) / u64::from(self.block_samples());
        Ok((
            self.data_offset + block * u64::from(self.block_bytes),
            encoded - block * u64::from(self.block_samples()),
        ))
    }
}

// WAVE parsing uses short reads and absolute chunk offsets. Keep nearby headers
// in one small buffer; BufReader::seek(Start) would discard it on every chunk.
struct Headers<'a, T: Read + Seek + ?Sized> {
    input: BufReader<&'a mut T>,
    position: u64,
}
impl<'a, T: Read + Seek + ?Sized> Headers<'a, T> {
    fn new(input: &'a mut T) -> Result<Self> {
        input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        Ok(Self {
            input: BufReader::with_capacity(512, input),
            position: 0,
        })
    }
    fn read(&mut self, data: &mut [u8]) -> Result<()> {
        read(&mut self.input, data)?;
        self.position += data.len() as u64;
        Ok(())
    }
    fn seek(&mut self, position: u64) -> Result<()> {
        // RIFF's declared length is u32; both offsets fit in i64.
        self.input
            .seek_relative(position as i64 - self.position as i64)
            .map_err(|e| e.to_string())?;
        self.position = position;
        Ok(())
    }
}

/// Returns None for other WAVE codecs. Buffers 512 bytes at a time and skips
/// payload chunks. The caller must reset the stream cursor before decoding.
pub fn inspect(input: &mut (impl Read + Seek + ?Sized), bytes: u64) -> Result<Option<Header>> {
    if bytes < 12 {
        return Ok(None);
    }
    let mut input = Headers::new(input)?;
    let mut riff = [0; 12];
    input.read(&mut riff)?;
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
        return Ok(None);
    }
    let end = u64::from(u32le(&riff, 4)) + 8;
    if end < 12 || end > bytes {
        return Err("truncated WAVE container".into());
    }
    let mut offset = 12;
    let mut fmt = None;
    let mut fact = None;
    let mut data = None;
    let mut source_rate = None;
    for _ in 0..256 {
        if offset == end {
            break;
        }
        if offset + 8 > end {
            return Err("truncated WAVE chunk".into());
        }
        input.seek(offset)?;
        let mut chunk = [0; 8];
        input.read(&mut chunk)?;
        let len = u64::from(u32le(&chunk, 4));
        let start = offset + 8;
        offset = start + len + (len & 1);
        if offset > end {
            return Err("WAVE chunk outside container".into());
        }
        match &chunk[..4] {
            b"fmt " => {
                if fmt.is_some() || len < 16 {
                    return Err("invalid WAVE format chunk".into());
                }
                let mut b = [0; 52];
                input.read(&mut b[..len.min(52) as usize])?;
                if u16le(&b, 0) != 0xfffe || len < 40 || b[24..40] != GUID {
                    return Ok(None);
                }
                if len != 52 || u16le(&b, 16) != 34 || u32le(&b, 40) > 2 {
                    return Err("unsupported AT9 format header".into());
                }
                fmt = Some(b);
            }
            b"fact" => {
                if fact.is_some() || len != 12 {
                    return Err("unsupported AT9 fact chunk".into());
                }
                let mut b = [0; 12];
                input.read(&mut b)?;
                fact = Some((u32le(&b, 0), u32le(&b, 8)));
            }
            b"data" => {
                if data.replace((start, len)).is_some() {
                    return Err("multiple WAVE data chunks".into());
                }
            }
            b"krSR" => {
                if source_rate.is_some() || len != 8 {
                    return Err("invalid AT9 source-rate chunk".into());
                }
                let mut b = [0; 8];
                input.read(&mut b)?;
                let rate = u32le(&b, 4);
                if u32le(&b, 0) != 1 || !(8000..=96000).contains(&rate) {
                    return Err("unsupported AT9 source-rate metadata".into());
                }
                source_rate = Some(rate);
            }
            _ => {}
        }
    }
    let Some(fmt) = fmt else {
        return Ok(None);
    };
    if offset != end {
        return Err("too many AT9 chunks".into());
    }
    let (samples, delay) = fact.ok_or("AT9 fact chunk missing")?;
    let (data_offset, data_bytes) = data.ok_or("AT9 data chunk missing")?;
    let config: [u8; 4] = fmt[44..48].try_into().unwrap();
    let bits = u32::from_be_bytes(config);
    let rate = match (bits >> 20) & 15 {
        1 => 12000,
        4 => 24000,
        7 => 48000,
        _ => return Err("unsupported Vita AT9 sample rate".into()),
    };
    let channels = match (bits >> 17) & 7 {
        0 => 1,
        1 | 2 => 2,
        _ => return Err("Vita AT9 requires mono or stereo".into()),
    };
    let log_frames = (bits >> 3) & 3;
    let frame_bytes = ((bits >> 5) & 2047) + 1;
    if config[0] != 0xfe
        || bits & 0x10000 != 0
        || !matches!(log_frames, 0 | 2)
        || frame_bytes > 1024
    {
        return Err("unsupported AT9 configuration".into());
    }
    let block_frames = 1 << log_frames;
    let block_bytes = frame_bytes * block_frames;
    let frame_samples = rate * 256 / 48000;
    let block_samples = frame_samples * block_frames;
    if u32::from(u16le(&fmt, 2)) != channels
        || u32le(&fmt, 4) != rate
        || u32::from(u16le(&fmt, 12)) != block_bytes
        || u32::from(u16le(&fmt, 18)) != block_samples
        || delay < frame_samples
        || delay > block_samples * 4
        || samples == 0
        || !data_bytes.is_multiple_of(u64::from(block_bytes))
        || u64::from(samples) + u64::from(delay)
            > data_bytes / u64::from(block_bytes) * u64::from(block_samples)
    {
        return Err("inconsistent AT9 geometry or sample timeline".into());
    }
    Ok(Some(Header {
        format: Format {
            rate: source_rate.unwrap_or(rate),
            channels,
            bits: 16,
            frames: samples.into(),
        },
        codec_rate: rate,
        config,
        data_offset,
        data_bytes,
        block_bytes,
        block_frames,
        frame_samples,
        delay,
    }))
}

/// Buffers belong to the codec, allowing aligned direct reads and hardware PCM
/// output without an intermediate compressed/PCM copy. Called on decode workers.
pub trait PacketDecoder: Send {
    fn reset(&mut self) -> Result<()>;
    fn decode(&mut self, input: &mut dyn Read) -> Result<()>;
    fn pcm(&self) -> &[i16];
    /// Release scarce native decoder channels while queued PCM finishes playing.
    fn finish(&mut self) {}
}
pub struct Stream<C> {
    codec: C,
    input: BufReader<Box<dyn krkr_assets::Stream>>,
    _buffer: Permit,
    header: Header,
    cancel: Arc<AtomicBool>,
    position: u64,
    next_block: u64,
    skip: u64,
    offset: usize,
    available: usize,
}
impl<C: PacketDecoder> Stream<C> {
    pub fn new(
        codec: C,
        input: Box<dyn krkr_assets::Stream>,
        header: Header,
        cancel: Arc<AtomicBool>,
        budget: &Budget,
    ) -> Result<Self> {
        // The hardware consumes small superframes. Coalesce their file/XP3
        // reads on the decode worker instead of issuing one kernel read per
        // packet per voice. Short effects reserve only their compressed body.
        // Seek discards prefetched bytes; they never alter the source timeline.
        let bytes = header.data_bytes.min(16 * 1024) as usize;
        let permit = budget.reserve(bytes).map_err(|e| e.to_string())?;
        let mut stream = Self {
            codec,
            input: BufReader::with_capacity(bytes, input),
            _buffer: permit,
            header,
            cancel,
            position: 0,
            next_block: 0,
            skip: 0,
            offset: 0,
            available: 0,
        };
        stream.seek(0)?;
        Ok(stream)
    }
    fn ready(&mut self) -> Result<bool> {
        if self.position >= self.header.format.frames {
            self.codec.finish();
            return Ok(false);
        }
        let channels = self.header.format.channels as usize;
        while self.offset == self.available {
            if self.cancel.load(Ordering::Acquire) {
                return Err("AT9 decoding cancelled".into());
            }
            if self.next_block + u64::from(self.header.block_bytes)
                > self.header.data_offset + self.header.data_bytes
            {
                return Err("AT9 data ended before sample timeline".into());
            }
            self.codec.decode(&mut self.input)?;
            self.next_block += u64::from(self.header.block_bytes);
            self.available = self.header.block_samples() as usize;
            if self.codec.pcm().len() != self.available * channels {
                return Err("AT9 decoder returned unexpected PCM length".into());
            }
            self.offset = self.skip.min(self.available as u64) as usize;
            self.skip -= self.offset as u64;
        }
        Ok(true)
    }
}
impl<C: PacketDecoder> StreamDecoder for Stream<C> {
    fn format(&self) -> Format {
        self.header.format
    }
    fn seek(&mut self, sample: u64) -> Result<()> {
        let (offset, skip) = self.header.seek(sample)?;
        self.codec.reset()?;
        self.input
            .seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        self.next_block = offset;
        self.skip = skip;
        self.offset = 0;
        self.available = 0;
        self.position = sample;
        Ok(())
    }
    fn next(&mut self) -> Result<Option<DecodedFrame>> {
        if !self.ready()? {
            return Ok(None);
        }
        let channels = self.header.format.channels as usize;
        let at = self.offset * channels;
        let mut pcm = [0; 8];
        pcm[..channels].copy_from_slice(&self.codec.pcm()[at..at + channels]);
        let frame = DecodedFrame {
            sample: [
                f32::from(pcm[0]) / 32768.,
                f32::from(pcm[channels - 1]) / 32768.,
            ],
            pcm,
            position: self.position,
        };
        self.position += 1;
        self.offset += 1;
        Ok(Some(frame))
    }
    fn read_frames(&mut self, output: &mut [DecodedFrame]) -> Result<usize> {
        let mut written = 0;
        while written < output.len() && self.ready()? {
            let count = (self.available - self.offset)
                .min(output.len() - written)
                .min((self.header.format.frames - self.position).min(usize::MAX as u64) as usize);
            let out = &mut output[written..written + count];
            match self.header.format.channels {
                1 => {
                    let input = &self.codec.pcm()[self.offset..self.offset + count];
                    for (i, (target, &value)) in out.iter_mut().zip(input).enumerate() {
                        let sample = f32::from(value) / 32768.;
                        *target = DecodedFrame {
                            sample: [sample, sample],
                            pcm: [value, 0, 0, 0, 0, 0, 0, 0],
                            position: self.position + i as u64,
                        };
                    }
                }
                2 => {
                    let input = &self.codec.pcm()[self.offset * 2..(self.offset + count) * 2];
                    for (i, (target, &[left, right])) in
                        out.iter_mut().zip(input.as_chunks::<2>().0).enumerate()
                    {
                        *target = DecodedFrame {
                            sample: [f32::from(left) / 32768., f32::from(right) / 32768.],
                            pcm: [left, right, 0, 0, 0, 0, 0, 0],
                            position: self.position + i as u64,
                        };
                    }
                }
                _ => return Err("Vita AT9 requires mono or stereo".into()),
            }
            written += count;
            self.offset += count;
            self.position += count as u64;
        }
        Ok(written)
    }
}

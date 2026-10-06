use crate::{Budget, Format, Permit, Result};
use krkr_assets::{ReadPlan, Stream};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Mutex,
};
use symphonia::core::{
    audio::{Channels, Position as Channel},
    codecs::audio::{AudioDecoder, AudioDecoderOptions},
    formats::{FormatReader, SeekMode, SeekTo, TrackType, probe::Hint},
    io::{MediaSource, MediaSourceStream},
    units::{TimeBase, Timestamp},
};
#[cfg(test)]
#[path = "../tests/internal/decode_batch.rs"]
mod batch_tests;

struct Source {
    stream: Mutex<Box<dyn Stream>>,
    len: u64,
}

pub(super) fn inspect_builtin(plan: ReadPlan) -> Result<Format> {
    Standard::open(plan, false, Budget::new(usize::MAX)).map(|decoder| decoder.format)
}
impl Read for Source {
    fn read(&mut self, dst: &mut [u8]) -> std::io::Result<usize> {
        self.stream.get_mut().unwrap().read(dst)
    }
}
impl Seek for Source {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.stream.get_mut().unwrap().seek(pos)
    }
}
impl MediaSource for Source {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.len)
    }
}

struct Standard {
    reader: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
    time_base: TimeBase,
    pub format: Format,
    pub samples: Vec<f32>,
    pub offset: usize,
    pub position: u64,
    skip_until: u64,
    weights: [[f32; 2]; 8],
    pcm16: bool,
    budget: Budget,
    sample_permits: Vec<Permit>,
}
impl Standard {
    pub fn open(plan: ReadPlan, vorbis: bool, budget: Budget) -> Result<Self> {
        let stream = plan.open().map_err(|e| e.to_string())?;
        Self::from_stream(stream, plan.bytes, vorbis, budget)
    }
    fn from_stream(
        mut stream: Box<dyn Stream>,
        len: u64,
        vorbis: bool,
        budget: Budget,
    ) -> Result<Self> {
        stream.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let source = Source {
            stream: Mutex::new(stream),
            len,
        };
        let stream = MediaSourceStream::new(Box::new(source), Default::default());
        let reader = symphonia::default::get_probe()
            .probe(&Hint::new(), stream, Default::default(), Default::default())
            .map_err(|e| e.to_string())?;
        let track = reader
            .default_track(TrackType::Audio)
            .ok_or("no audio track")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .ok_or("no audio codec")?;
        let pcm16 =
            vorbis && params.codec == symphonia::core::codecs::audio::well_known::CODEC_ID_VORBIS;
        let rate = params.sample_rate.ok_or("missing sample rate")?;
        let channels = params
            .channels
            .as_ref()
            .ok_or("missing audio channels")?
            .count() as u32;
        if rate == 0 || rate > 384_000 || channels == 0 || channels > 8 {
            return Err("unsupported audio channel/rate configuration".into());
        }
        let mut weights = [[0.0; 2]; 8];
        if channels == 1 {
            weights[0] = [1.0; 2];
        } else if channels == 2 {
            weights[0][0] = 1.0;
            weights[1][1] = 1.0;
        } else if let Some(Channels::Positioned(positions)) = params.channels.as_ref() {
            for (i, position) in positions.iter().enumerate() {
                weights[i] = match position {
                    Channel::FRONT_LEFT => [1.0, 0.0],
                    Channel::FRONT_RIGHT => [0.0, 1.0],
                    Channel::FRONT_CENTER => [std::f32::consts::FRAC_1_SQRT_2; 2],
                    Channel::LFE1 => [0.5; 2],
                    Channel::REAR_LEFT | Channel::SIDE_LEFT | Channel::FRONT_LEFT_CENTER => {
                        [std::f32::consts::FRAC_1_SQRT_2, 0.0]
                    }
                    Channel::REAR_RIGHT | Channel::SIDE_RIGHT | Channel::FRONT_RIGHT_CENTER => {
                        [0.0, std::f32::consts::FRAC_1_SQRT_2]
                    }
                    Channel::REAR_CENTER => [0.5; 2],
                    _ => return Err("unsupported audio channel layout".into()),
                };
            }
        } else {
            return Err("unsupported audio channel layout".into());
        }
        let time_base = track
            .time_base
            .unwrap_or_else(|| TimeBase::try_from_recip(rate).unwrap());
        let frames = track
            .num_frames
            .or_else(|| {
                track.duration.map(|d| {
                    (d.get() as u128 * time_base.numer.get() as u128 * rate as u128
                        / time_base.denom.get() as u128) as u64
                })
            })
            .unwrap_or(0);
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(params, &AudioDecoderOptions::default())
            .map_err(|e| e.to_string())?;
        let format = Format {
            rate,
            channels,
            bits: params.bits_per_sample.unwrap_or(16),
            frames,
        };
        let track = track.id;
        let mut result = Self {
            reader,
            decoder,
            track,
            time_base,
            format,
            samples: Vec::new(),
            offset: 0,
            position: 0,
            skip_until: 0,
            weights,
            pcm16,
            budget,
            sample_permits: Vec::new(),
        };
        result.next_packet()?;
        Ok(result)
    }
    pub fn seek(&mut self, frame: u64) -> Result<()> {
        let ts = frame as u128 * self.time_base.denom.get() as u128
            / (self.format.rate as u128 * self.time_base.numer.get() as u128);
        let ts = i64::try_from(ts).map_err(|_| "audio seek overflow")?;
        self.reader
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(ts),
                    track_id: self.track,
                },
            )
            .map_err(|e| e.to_string())?;
        self.decoder.reset();
        self.samples.clear();
        self.offset = 0;
        self.skip_until = frame;
        self.position = frame;
        self.next_packet()?;
        Ok(())
    }
    fn next_packet(&mut self) -> Result<bool> {
        loop {
            let Some(packet) = self.reader.next_packet().map_err(|e| e.to_string())? else {
                self.samples.clear();
                self.offset = 0;
                return Ok(false);
            };
            if packet.track_id != self.track {
                continue;
            }
            let position = (packet.pts.get().max(0) as u128
                * self.time_base.numer.get() as u128
                * self.format.rate as u128
                / self.time_base.denom.get() as u128) as u64;
            let decoded = self.decoder.decode(&packet).map_err(|e| e.to_string())?;
            if decoded.spec().rate() != self.format.rate
                || decoded.spec().channels().count() != self.format.channels as usize
            {
                return Err("audio format changed within stream".into());
            }
            let count = decoded.samples_interleaved();
            if count > 131_072 {
                return Err("audio packet exceeds decode budget".into());
            }
            if count > self.samples.capacity() {
                let capacity = count.div_ceil(1024) * 1024;
                let permit = self
                    .budget
                    .reserve((capacity - self.samples.capacity()) * std::mem::size_of::<f32>())
                    .map_err(|e| e.to_string())?;
                // Small blocks bound growth/permit records (at most 128)
                // without reserving a much larger PCM packet for every voice.
                // Keep capacity and its permits across seeks/replay.
                self.samples.reserve_exact(capacity - self.samples.len());
                self.sample_permits.push(permit);
            }
            self.samples.resize(count, 0.0);
            decoded.copy_to_slice_interleaved(&mut self.samples);
            self.offset = (self.skip_until.saturating_sub(position) as usize)
                .saturating_mul(self.format.channels as usize)
                .min(count);
            self.position = position + (self.offset / self.format.channels as usize) as u64;
            if self.offset != count {
                return Ok(true);
            }
        }
    }
    pub fn next(&mut self) -> Result<Option<crate::Frame>> {
        if self.offset == self.samples.len() && !self.next_packet()? {
            return Ok(None);
        }
        let channels = self.format.channels as usize;
        let samples = &self.samples[self.offset..self.offset + channels];
        let mut stereo = [0.0; 2];
        for (sample, weights) in samples.iter().zip(&self.weights) {
            let sample = if self.pcm16 {
                ((*sample * 32768.).round().clamp(-32768., 32767.) as i16) as f32 / 32768.
            } else {
                *sample
            };
            stereo[0] += sample * weights[0];
            stereo[1] += sample * weights[1];
        }
        let frame = crate::Frame {
            sample: stereo,
            pcm: std::array::from_fn(|i| {
                samples
                    .get(i)
                    .map_or(0, |s| (*s * 32768.).round().clamp(-32768., 32767.) as i16)
            }),
            position: self.position,
            labels: [0, 0],
        };
        self.position += 1;
        self.offset += channels;
        Ok(Some(frame))
    }
}
enum Backend {
    Standard(Box<Standard>),
    Tcwf(crate::tcwf::Decoder),
    External(Box<dyn crate::StreamDecoder>),
}
pub(crate) struct Decoder {
    backend: Backend,
    pub format: Format,
    pub position: u64,
    // Conservative allowance for builtin codec/demux state and worker scratch.
    // The interleaved packet buffer is charged separately at its real capacity.
    _permit: Option<Permit>,
}
impl Decoder {
    pub fn open(
        plan: ReadPlan,
        codecs: [usize; 4],
        external: Option<std::sync::Arc<dyn crate::DecoderBackend>>,
        budget: crate::Budget,
        stop: &std::sync::atomic::AtomicBool,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Self> {
        let mut stream = plan
            .open_interruptible(&|| stop.load(std::sync::atomic::Ordering::Relaxed))
            .map_err(|e| e.to_string())?;
        let mut prefix = Vec::with_capacity(64);
        stream
            .by_ref()
            .take(64)
            .read_to_end(&mut prefix)
            .map_err(|e| e.to_string())?;
        let tcwf = prefix.starts_with(b"TCWF0\x1a");
        let at9 = if prefix.starts_with(b"RIFF") {
            crate::at9::inspect(&mut stream, plan.bytes)?
        } else {
            None
        };
        // Script-requested extended codecs require registration. Converted AT9
        // is a native host capability and needs no game-side plugin changes.
        let opus = prefix.starts_with(b"OggS") && prefix.windows(8).any(|v| v == b"OpusHead");
        let ffmpeg = codecs[crate::Codec::Ffmpeg as usize] != 0;
        if opus && !ffmpeg && codecs[crate::Codec::Opus as usize] == 0 {
            return Err("Opus audio requires wuopus/wuffmpeg and a host decoder backend".into());
        }
        let permit = if tcwf || !(opus || ffmpeg || at9.is_some()) {
            Some(budget.reserve(512 * 1024).map_err(|e| e.to_string())?)
        } else {
            // Host decoders already reserve their own buffers from this pool.
            None
        };
        let backend = if tcwf {
            if codecs[crate::Codec::Tcwf as usize] == 0 {
                return Err("TCWF decoder plugin is not linked".into());
            }
            stream.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            Backend::Tcwf(crate::tcwf::Decoder::open(stream, plan.bytes)?)
        } else if opus || ffmpeg || at9.is_some() {
            let external = external.ok_or(if opus {
                "Opus audio requires a host decoder backend"
            } else {
                "extended audio decoder is unavailable"
            })?;
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            let worker_cancel = cancel.clone();
            std::thread::Builder::new()
                .name("krkr-audio-open".into())
                .spawn(move || {
                    let result = external.open_prepared(
                        &plan,
                        budget,
                        opus,
                        worker_cancel,
                        crate::DecoderSource { stream, at9 },
                    );
                    let _ = tx.send(result);
                })
                .map_err(|e| e.to_string())?;
            let decoder = loop {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    cancel.store(true, std::sync::atomic::Ordering::Release);
                    return Err("audio open cancelled".into());
                }
                match krkr_protocol::channel::recv_timeout(
                    &rx,
                    std::time::Duration::from_millis(10),
                ) {
                    Ok(result) => break result?,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => return Err("audio open worker disconnected".into()),
                }
            };
            Backend::External(decoder)
        } else {
            Backend::Standard(Box::new(Standard::from_stream(
                stream,
                plan.bytes,
                codecs[crate::Codec::Vorbis as usize] != 0,
                budget,
            )?))
        };
        let (format, position) = match &backend {
            Backend::Standard(d) => (d.format, d.position),
            Backend::Tcwf(d) => (d.format, 0),
            Backend::External(d) => (d.format(), 0),
        };
        Ok(Self {
            backend,
            format,
            position,
            _permit: permit,
        })
    }
    pub fn seek(&mut self, frame: u64) -> Result<()> {
        self.position = match &mut self.backend {
            Backend::Standard(d) => {
                d.seek(frame)?;
                d.position
            }
            Backend::Tcwf(d) => {
                d.seek(frame)?;
                frame
            }
            Backend::External(d) => {
                d.seek(frame)?;
                frame
            }
        };
        Ok(())
    }
    pub fn next(&mut self) -> Result<Option<crate::Frame>> {
        let frame = match &mut self.backend {
            Backend::Standard(d) => d.next()?,
            Backend::Tcwf(d) => d.next()?,
            Backend::External(d) => d.next()?.map(|f| crate::Frame {
                sample: f.sample,
                pcm: f.pcm,
                position: f.position,
                labels: [0, 0],
            }),
        };
        if let Some(frame) = &frame {
            self.position = frame.position + 1;
        }
        Ok(frame)
    }
    pub fn read_frames(&mut self, output: &mut [crate::Frame]) -> Result<usize> {
        if let Backend::External(decoder) = &mut self.backend {
            // Bound worker stack use even if a caller requests a larger block.
            // One virtual call can consume a native codec's contiguous PCM.
            let mut decoded = [crate::DecodedFrame::default(); 256];
            let mut count = 0;
            for out in output.chunks_mut(decoded.len()) {
                let length = decoder.read_frames(&mut decoded[..out.len()])?;
                if length > out.len() {
                    return Err("audio decoder returned more frames than requested".into());
                }
                for (target, frame) in out.iter_mut().zip(&decoded[..length]) {
                    *target = crate::Frame {
                        sample: frame.sample,
                        pcm: frame.pcm,
                        position: frame.position,
                        labels: [0, 0],
                    };
                }
                if length != 0 {
                    self.position = decoded[length - 1].position + 1;
                }
                count += length;
                if length < out.len() {
                    break;
                }
            }
            Ok(count)
        } else {
            let mut count = 0;
            for target in output {
                let Some(frame) = self.next()? else {
                    break;
                };
                *target = frame;
                count += 1;
            }
            Ok(count)
        }
    }
}

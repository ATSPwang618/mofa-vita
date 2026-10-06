//! Pure audio FFmpeg backend for wuopus/wuffmpeg. No video stream is required.
//! All IO uses the VFS; planar samples and channel layouts are converted here.
use super::{err, ff};
use krkr_audio::{DecodedFrame, DecoderBackend, Format, Result, StreamDecoder};
use krkr_protocol::budget::{Budget, Permit};
use std::io::Read;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub struct WaveBackend;
impl DecoderBackend for WaveBackend {
    fn open(
        &self,
        plan: &krkr_assets::ReadPlan,
        budget: Budget,
        opus: bool,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn StreamDecoder>> {
        let mut input = plan
            .open_interruptible(&|| cancel.load(Ordering::Acquire))
            .map_err(|e| e.to_string())?;
        let at9 = krkr_audio::at9::inspect(&mut input, plan.bytes)?;
        drop(input);
        let mut wave = Wave::open(plan, budget, opus, cancel, at9)?;
        if let Some(header) = at9 {
            // WAVE demuxers expose padded AT9 blocks. Keep desktop playback on the
            // same audible timeline as the hardware backend and .sli scripts.
            wave.format.frames = header.data_bytes / u64::from(header.block_bytes)
                * u64::from(header.block_samples());
            wave.skip_until = header.delay.into();
            Ok(Box::new(At9Wave {
                wave,
                header,
                position: 0,
            }))
        } else {
            Ok(Box::new(wave))
        }
    }
}
struct At9Wave {
    wave: Wave,
    header: krkr_audio::at9::Header,
    position: u64,
}
impl StreamDecoder for At9Wave {
    fn format(&self) -> Format {
        self.header.format
    }
    fn seek(&mut self, frame: u64) -> Result<()> {
        if frame > self.header.format.frames {
            return Err("AT9 seek outside stream".into());
        }
        self.wave.seek(frame + u64::from(self.header.delay))?;
        self.position = frame;
        Ok(())
    }
    fn next(&mut self) -> Result<Option<DecodedFrame>> {
        if self.position >= self.header.format.frames {
            return Ok(None);
        }
        let mut frame = self
            .wave
            .next()?
            .ok_or("AT9 ended before its declared timeline")?;
        frame.position = self.position;
        // Use the same signed 16-bit domain as sceAudiodec for stereo mixing.
        frame.sample = [
            f32::from(frame.pcm[0]) / 32768.,
            f32::from(frame.pcm[self.header.format.channels as usize - 1]) / 32768.,
        ];
        self.position += 1;
        Ok(Some(frame))
    }
}
struct Wave {
    input: ff::format::context::Input,
    decoder: ff::decoder::Audio,
    resampler: Option<ff::software::resampling::Context>,
    index: usize,
    time_base: f64,
    start: f64,
    format: Format,
    opus: bool,
    eof: bool,
    drained: bool,
    samples: Vec<[f32; 2]>,
    pcm: Vec<[i16; 8]>,
    pcm_memory: Permit,
    offset: usize,
    block_position: i64,
    next_position: i64,
    skip_until: u64,
    budget: Budget,
    _memory: Permit,
    at9_samples: Option<usize>,
}
impl Wave {
    fn open(
        plan: &krkr_assets::ReadPlan,
        budget: Budget,
        opus: bool,
        cancel: Arc<AtomicBool>,
        at9: Option<krkr_audio::at9::Header>,
    ) -> Result<Self> {
        ff::init().map_err(err)?;
        // Includes AVIO, probe/codec scratch and owned PCM packet storage.
        // AT9 has a known packet/sample bound. Avoid generic probing, large
        // AVIO buffers and resampling storage for every concurrent effect.
        let memory = budget
            .reserve(if at9.is_some() {
                512 * 1024
            } else {
                2 * 1024 * 1024
            })
            .map_err(|e| e.to_string())?;
        let pcm_memory = budget.reserve(0).map_err(|e| e.to_string())?;
        let name = String::from_utf16_lossy(&plan.name);
        let stream = ff::format::context::StreamIo::from_read_seek_with_capacity(
            plan.open_interruptible(&|| cancel.load(Ordering::Acquire))
                .map_err(|e| e.to_string())?,
            if at9.is_some() { 4096 } else { 64 * 1024 },
        )
        .map_err(err)?;
        let mut options = ff::Dictionary::new();
        options.set("probesize", "1048576");
        options.set("analyzeduration", "5000000");
        options.set("protocol_whitelist", "");
        if let Some(header) = at9 {
            options.set("probesize", "4096");
            options.set("analyzeduration", "0");
            options.set("max_size", &header.block_bytes.max(1024).to_string());
        }
        let input = ff::format::input_from_stream_with_interrupt(
            stream,
            Some(&name),
            Some(options),
            move || cancel.load(Ordering::Acquire),
        )
        .map_err(err)?;
        let source = input
            .streams()
            .best(ff::media::Type::Audio)
            .ok_or("file has no audio stream")?;
        let index = source.index();
        let mut context = ff::codec::context::Context::from_parameters(source.parameters())
            .map_err(err)?
            .decoder();
        context.set_packet_time_base(source.time_base());
        let decoder = context.audio().map_err(err)?;
        if opus && decoder.id() != ff::codec::Id::OPUS {
            return Err("wuopus requires Opus audio".into());
        }
        let rate = if opus { 48000 } else { decoder.rate() };
        let channels = u32::from(decoder.channels());
        if rate == 0 || rate > 384000 || channels == 0 || channels > 8 {
            return Err("unsupported audio rate/channel configuration".into());
        }
        let bits = if opus {
            16
        } else {
            (decoder.format().bytes() * 8) as u32
        };
        if bits == 0 {
            return Err("unknown audio sample format".into());
        }
        let time_base = f64::from(source.time_base());
        let start = if source.start_time() == ff::ffi::AV_NOPTS_VALUE {
            0.
        } else {
            source.start_time() as f64 * time_base
        };
        let duration = if source.duration() > 0 {
            source.duration() as f64 * time_base
        } else {
            input.duration().max(0) as f64 / 1_000_000.
        };
        let mut frames = (duration * rate as f64).round().max(0.) as u64;
        if opus {
            // Ogg stream duration includes pre-skip; opusfile's PCM total does not.
            let mut head = Vec::new();
            plan.open()
                .map_err(|e| e.to_string())?
                .take(64)
                .read_to_end(&mut head)
                .map_err(|e| e.to_string())?;
            if let Some(i) = head.windows(8).position(|v| v == b"OpusHead")
                && head.len() >= i + 12
            {
                frames = frames
                    .saturating_sub(u64::from(u16::from_le_bytes([head[i + 10], head[i + 11]])));
            }
        }
        Ok(Self {
            input,
            decoder,
            resampler: None,
            index,
            time_base,
            start,
            format: Format {
                rate,
                channels,
                bits,
                frames,
            },
            opus,
            eof: false,
            drained: false,
            samples: Vec::with_capacity(at9.map_or(65536, |h| h.block_samples() as usize)),
            pcm: Vec::new(),
            pcm_memory,
            offset: 0,
            block_position: 0,
            next_position: 0,
            skip_until: 0,
            budget,
            _memory: memory,
            at9_samples: at9.map(|h| h.block_samples() as usize),
        })
    }
    fn output(&mut self, frame: &ff::frame::Audio, position: i64) {
        self.samples.clear();
        if frame.channels() == 1 {
            self.samples
                .extend(frame.plane::<f32>(0).iter().map(|&v| [v, v]));
        } else {
            self.samples
                .extend(frame.plane::<(f32, f32)>(0).iter().map(|&(l, r)| [l, r]));
        }
        if self.opus {
            for sample in &mut self.samples {
                for v in sample {
                    *v = ((*v * 32768.).round().clamp(-32768., 32767.) as i16) as f32 / 32768.;
                }
            }
        }
        self.block_position = position;
        self.next_position = position + frame.samples() as i64;
        self.offset = (self.skip_until as i128 - position as i128)
            .max(0)
            .min(frame.samples() as i128) as usize;
    }
    fn receive(&mut self) -> Result<bool> {
        loop {
            let output_layout = if self.format.channels == 1 {
                ff::ChannelLayout::MONO
            } else {
                ff::ChannelLayout::STEREO
            };
            let mut frame = ff::frame::Audio::empty();
            match self.decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    if self
                        .at9_samples
                        .is_some_and(|limit| frame.samples() > limit)
                    {
                        return Err("AT9 decoder exceeded its superframe sample bound".into());
                    }
                    if frame.samples() > 65536
                        || frame.channels() == 0
                        || frame.channels() > 8
                        || frame.rate() == 0
                    {
                        return Err("audio frame exceeds decoder limits".into());
                    }
                    if frame.rate() != self.decoder.rate()
                        || u32::from(frame.channels()) != self.format.channels
                    {
                        return Err("audio format changed within stream".into());
                    }
                    self.pcm.clear();
                    if self.pcm.capacity() < frame.samples() {
                        let memory = self
                            .budget
                            .reserve(frame.samples() * std::mem::size_of::<[i16; 8]>())
                            .map_err(|e| e.to_string())?;
                        self.pcm = Vec::with_capacity(frame.samples());
                        self.pcm_memory = memory;
                    }
                    for i in 0..frame.samples() {
                        let mut pcm = [0; 8];
                        for (channel, sample) in
                            pcm.iter_mut().enumerate().take(frame.channels() as usize)
                        {
                            *sample = source_sample(&frame, i, channel)?;
                        }
                        self.pcm.push(pcm);
                    }
                    if self.at9_samples.is_some() {
                        self.samples.clear();
                        self.samples.extend(self.pcm.iter().map(|p| {
                            [
                                f32::from(p[0]) / 32768.,
                                f32::from(p[self.format.channels as usize - 1]) / 32768.,
                            ]
                        }));
                        let position = frame
                            .timestamp()
                            .map(|t| {
                                ((t as f64 * self.time_base - self.start) * self.format.rate as f64)
                                    .round() as i64
                            })
                            .unwrap_or(self.next_position);
                        self.block_position = position;
                        self.next_position = position + frame.samples() as i64;
                        self.offset = (self.skip_until as i128 - position as i128)
                            .max(0)
                            .min(frame.samples() as i128)
                            as usize;
                        if self.offset < self.samples.len() {
                            return Ok(true);
                        }
                        continue;
                    }
                    let mut layout = frame.channel_layout();
                    if layout.is_empty() {
                        layout = ff::ChannelLayout::default(i32::from(frame.channels()));
                        frame.set_channel_layout(layout);
                    }
                    if self.resampler.as_ref().is_none_or(|r| {
                        r.input().format != frame.format()
                            || r.input().channel_layout != layout
                            || r.input().rate != frame.rate()
                    }) {
                        self.resampler = Some(
                            ff::software::resampling::Context::get(
                                frame.format(),
                                layout,
                                frame.rate(),
                                ff::format::Sample::F32(ff::format::sample::Type::Packed),
                                output_layout,
                                self.format.rate,
                            )
                            .map_err(err)?,
                        );
                    }
                    let count = (frame.samples() * self.format.rate as usize)
                        .div_ceil(frame.rate() as usize)
                        + 256;
                    if count > 65536 {
                        return Err("audio conversion exceeds buffer limit".into());
                    }
                    let position = frame
                        .timestamp()
                        .map(|t| {
                            ((t as f64 * self.time_base - self.start) * self.format.rate as f64)
                                .round() as i64
                        })
                        .unwrap_or(self.next_position);
                    let mut output = ff::frame::Audio::new(
                        ff::format::Sample::F32(ff::format::sample::Type::Packed),
                        count,
                        output_layout,
                    );
                    self.resampler
                        .as_mut()
                        .unwrap()
                        .run(&frame, &mut output)
                        .map_err(err)?;
                    self.output(&output, position);
                    if self.offset < self.samples.len() {
                        return Ok(true);
                    }
                }
                Err(ff::Error::Eof) => {
                    self.drained = true;
                    if let Some(resampler) = &mut self.resampler {
                        let mut output = ff::frame::Audio::new(
                            ff::format::Sample::F32(ff::format::sample::Type::Packed),
                            4096,
                            output_layout,
                        );
                        if resampler.flush(&mut output).map_err(err)?.is_some() {
                            self.drained = false;
                        }
                        self.output(&output, self.next_position);
                        return Ok(self.offset < self.samples.len());
                    }
                    return Ok(false);
                }
                Err(ff::Error::Other {
                    errno: ff::error::EAGAIN,
                }) => {
                    if self.eof {
                        return Err("decoder stalled after end of stream".into());
                    }
                    loop {
                        let mut packet = ff::Packet::empty();
                        match packet.read(&mut self.input) {
                            Ok(()) => {
                                if packet.size() > 1024 * 1024 {
                                    return Err("audio packet exceeds 1 MiB".into());
                                }
                                if packet.stream() != self.index {
                                    continue;
                                }
                                let _permit = self
                                    .budget
                                    .reserve(packet.size())
                                    .map_err(|e| e.to_string())?;
                                self.decoder.send_packet(&packet).map_err(err)?;
                                break;
                            }
                            Err(ff::Error::Eof) => {
                                self.eof = true;
                                self.decoder.send_eof().map_err(err)?;
                                break;
                            }
                            Err(e) => return Err(err(e)),
                        }
                    }
                }
                Err(e) => return Err(err(e)),
            }
        }
    }
}
impl StreamDecoder for Wave {
    fn format(&self) -> Format {
        self.format
    }
    fn seek(&mut self, frame: u64) -> Result<()> {
        if self.format.frames != 0 && frame > self.format.frames {
            return Err("audio seek outside stream".into());
        }
        // Decode pre-roll after a backward seek, then discard to the exact sample.
        let seconds = frame as f64 / self.format.rate as f64;
        let timestamp = ((self.start + (seconds - 0.08).max(0.)) * 1_000_000.) as i64;
        self.input.seek(timestamp, ..timestamp).map_err(err)?;
        self.decoder.flush();
        self.resampler = None;
        self.samples.clear();
        self.offset = 0;
        self.eof = false;
        self.drained = false;
        self.skip_until = frame;
        self.next_position = frame as i64;
        Ok(())
    }
    fn next(&mut self) -> Result<Option<DecodedFrame>> {
        if self.offset == self.samples.len() && (self.drained || !self.receive()?) {
            return Ok(None);
        }
        let position = self.block_position + self.offset as i64;
        let frame = DecodedFrame {
            sample: self.samples[self.offset],
            pcm: self.pcm.get(self.offset).copied().unwrap_or([0; 8]),
            position: position.max(0) as u64,
        };
        self.offset += 1;
        Ok(Some(frame))
    }
}

fn source_sample(frame: &ff::frame::Audio, index: usize, channel: usize) -> Result<i16> {
    use ff::format::Sample;
    let format = frame.format();
    let (plane, offset) = if format.is_planar() {
        (channel, index)
    } else {
        (0, index * frame.channels() as usize + channel)
    };
    let width = format.bytes();
    let data = frame
        .data(plane)
        .get(offset * width..(offset + 1) * width)
        .ok_or("truncated source PCM")?;
    Ok(match format {
        Sample::U8(_) => (i16::from(data[0]) - 128) << 8,
        Sample::I16(_) => i16::from_ne_bytes(data.try_into().unwrap()),
        Sample::I32(_) => (i32::from_ne_bytes(data.try_into().unwrap()) >> 16) as i16,
        Sample::I64(_) => (i64::from_ne_bytes(data.try_into().unwrap()) >> 48) as i16,
        Sample::F32(_) => (f32::from_ne_bytes(data.try_into().unwrap()) * 32768.)
            .round()
            .clamp(-32768., 32767.) as i16,
        Sample::F64(_) => (f64::from_ne_bytes(data.try_into().unwrap()) * 32768.)
            .round()
            .clamp(-32768., 32767.) as i16,
        Sample::None => return Err("unknown source PCM format".into()),
    })
}

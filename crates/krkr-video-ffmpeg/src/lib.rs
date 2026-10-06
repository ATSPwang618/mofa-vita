//! PC software decoder. ffmpeg-next owns AVIO/codec lifetimes; all access is
//! confined to the movie worker. Files and XP3 entries use identical VFS IO.
use ffmpeg_next as ff;
mod wave;
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use krkr_video::{Backend, Decoded, Decoder, Frame, Info, Result};
use std::collections::VecDeque;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
pub use wave::WaveBackend;

pub struct Ffmpeg;
impl Backend for Ffmpeg {
    fn open(
        &self,
        plan: krkr_assets::ReadPlan,
        budget: Budget,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn Decoder>> {
        Movie::open(plan, budget, cancel, true).map(|movie| Box::new(movie) as Box<dyn Decoder>)
    }
    fn open_silent(
        &self,
        plan: krkr_assets::ReadPlan,
        budget: Budget,
        cancel: Arc<AtomicBool>,
    ) -> Result<Box<dyn Decoder>> {
        Movie::open(plan, budget, cancel, false).map(|movie| Box::new(movie) as Box<dyn Decoder>)
    }
}
fn err(error: ff::Error) -> String {
    format!("FFmpeg: {error}")
}
fn waiting(error: ff::Error) -> Result<()> {
    match error {
        ff::Error::Eof
        | ff::Error::Other {
            errno: ff::error::EAGAIN,
        } => Ok(()),
        error => Err(err(error)),
    }
}
const AUDIO_RATE: u32 = 48000;
struct Audio {
    index: usize,
    time_base: f64,
    decoder: ff::decoder::Audio,
    resampler: Option<ff::software::resampling::Context>,
    next: Option<i64>,
    flushed: bool,
}
struct Movie {
    input: ff::format::context::Input,
    video: ff::decoder::Video,
    video_index: usize,
    video_time_base: f64,
    video_indices: Vec<usize>,
    audio_indices: Vec<usize>,
    audio: Option<Audio>,
    scaler: Option<ff::software::scaling::Context>,
    info: Info,
    budget: Budget,
    _codec_memory: Permit,
    start: f64,
    next_video: f64,
    eof: bool,
    video_eof: bool,
    video_finished: bool,
    audio_eof: bool,
    audio_end_reported: bool,
    video_packets: VecDeque<(ff::Packet, Permit)>,
    audio_packets: VecDeque<(ff::Packet, Permit)>,
    queued_bytes: usize,
}
impl Movie {
    fn queue_packet(&mut self, packet: ff::Packet, video: bool) -> Result<()> {
        let bytes = packet.size() + 256;
        if self.queued_bytes + bytes > 16 * 1024 * 1024
            || self.video_packets.len() + self.audio_packets.len() >= 2048
        {
            return Err("movie interleaving exceeds compressed packet buffer budget".into());
        }
        let permit = self.budget.reserve(bytes).map_err(|e| e.to_string())?;
        self.queued_bytes += bytes;
        if video {
            self.video_packets.push_back((packet, permit));
        } else {
            self.audio_packets.push_back((packet, permit));
        }
        Ok(())
    }
    fn open(
        plan: krkr_assets::ReadPlan,
        budget: Budget,
        cancel: Arc<AtomicBool>,
        with_audio: bool,
    ) -> Result<Self> {
        ff::init().map_err(err)?;
        let name = String::from_utf16_lossy(&plan.name);
        let stream = ff::format::context::StreamIo::from_read_seek_with_capacity(
            plan.open().map_err(|e| e.to_string())?,
            64 * 1024,
        )
        .map_err(err)?;
        let mut options = ff::Dictionary::new();
        options.set("probesize", "1048576");
        options.set("analyzeduration", "5000000");
        // Secondary protocols/playlists must not bypass the game VFS.
        options.set("protocol_whitelist", "");
        let input = ff::format::input_from_stream_with_interrupt(
            stream,
            Some(&name),
            Some(options),
            move || cancel.load(Ordering::Acquire),
        )
        .map_err(err)?;
        let source = input
            .streams()
            .best(ff::media::Type::Video)
            .ok_or("movie has no video stream")?;
        let video_index = source.index();
        let video_time_base = f64::from(source.time_base());
        let fps = f64::from(source.avg_frame_rate());
        let fps = if fps.is_finite() && fps > 0.0 {
            fps
        } else {
            f64::from(source.rate())
        };
        if !fps.is_finite() || fps <= 0.0 {
            return Err("movie has no usable frame rate".into());
        }
        let mut context =
            ff::codec::context::Context::from_parameters(source.parameters()).map_err(err)?;
        context.set_threading(ff::codec::threading::Config {
            count: 2,
            ..Default::default()
        });
        let mut options = ff::Dictionary::new();
        options.set("max_pixels", "16777216");
        let codec = ff::decoder::find(context.id()).ok_or("video decoder is unavailable")?;
        let video = context
            .decoder()
            .open_as_with(codec, options)
            .and_then(|d| d.video())
            .map_err(err)?;
        let size = Size {
            width: video.width(),
            height: video.height(),
        };
        let bytes = valid_size(size)?;
        // Admission allowance for codec reference frames/conversion scratch.
        // Native allocator/driver RSS is not claimed to equal these tickets.
        let codec_memory = budget
            .reserve(bytes.checked_mul(12).ok_or("video allocation overflow")?)
            .map_err(|e| e.to_string())?;
        let duration = (input.duration() as f64 / 1_000_000.0).max(0.0);
        let frames = if source.frames() > 0 {
            source.frames() as u64
        } else {
            (duration * fps).round() as u64
        };
        let start = if source.start_time() == ff::ffi::AV_NOPTS_VALUE {
            0.0
        } else {
            source.start_time() as f64 * video_time_base
        };
        let audio_indices: Vec<_> = input
            .streams()
            .filter(|s| s.parameters().medium() == ff::media::Type::Audio)
            .map(|s| s.index())
            .collect();
        let video_indices: Vec<_> = input
            .streams()
            .filter(|s| s.parameters().medium() == ff::media::Type::Video)
            .map(|s| s.index())
            .collect();
        let info = Info {
            size,
            fps,
            frames,
            duration,
            audio_streams: audio_indices.len(),
            video_streams: video_indices.len(),
            video_stream: video_indices
                .iter()
                .position(|i| *i == video_index)
                .unwrap_or(0),
            audio_rate: AUDIO_RATE,
        };
        let mut movie = Self {
            input,
            video,
            video_index,
            video_time_base,
            video_indices,
            audio_indices,
            audio: None,
            scaler: None,
            info,
            budget,
            _codec_memory: codec_memory,
            start,
            next_video: 0.0,
            eof: false,
            video_eof: false,
            video_finished: false,
            audio_eof: false,
            audio_end_reported: false,
            video_packets: VecDeque::new(),
            audio_packets: VecDeque::new(),
            queued_bytes: 0,
        };
        if with_audio && !movie.audio_indices.is_empty() {
            movie.audio_stream(Some(0))?;
        }
        Ok(movie)
    }
    fn receive_video(&mut self) -> Result<Option<Decoded>> {
        let mut decoded = ff::frame::Video::empty();
        if let Err(error) = self.video.receive_frame(&mut decoded) {
            if error == ff::Error::Eof {
                self.video_finished = true;
            }
            waiting(error)?;
            return Ok(None);
        }
        let size = Size {
            width: decoded.width(),
            height: decoded.height(),
        };
        let bytes = valid_size(size)?;
        if size != self.info.size {
            return Err("movie changes dimensions during playback".into());
        }
        let mut rgba = Bytes::zeroed(bytes, &self.budget).map_err(|e| e.to_string())?;
        if self
            .scaler
            .as_ref()
            .is_none_or(|s| s.input().format != decoded.format())
        {
            self.scaler = Some(
                ff::software::scaling::Context::get(
                    decoded.format(),
                    size.width,
                    size.height,
                    ff::format::Pixel::RGBA,
                    size.width,
                    size.height,
                    ff::software::scaling::Flags::BILINEAR,
                )
                .map_err(err)?,
            );
        }
        let mut converted = ff::frame::Video::empty();
        self.scaler
            .as_mut()
            .unwrap()
            .run(&decoded, &mut converted)
            .map_err(err)?;
        for (y, row) in rgba
            .as_mut_slice()
            .chunks_exact_mut(size.width as usize * 4)
            .enumerate()
        {
            let start = y * converted.stride(0);
            row.copy_from_slice(&converted.data(0)[start..start + row.len()]);
        }
        let time = decoded.timestamp().map_or(self.next_video, |pts| {
            pts as f64 * self.video_time_base - self.start
        });
        self.next_video = time + 1.0 / self.info.fps;
        Ok(Some(Decoded::Video(Frame {
            time,
            pixels: Arc::new(Pixels {
                size,
                main: Some(rgba),
                province: None,
            })
            .into(),
        })))
    }
    fn receive_audio(&mut self) -> Result<Option<Decoded>> {
        let Some(audio) = &mut self.audio else {
            return Ok(None);
        };
        let mut decoded = ff::frame::Audio::empty();
        match audio.decoder.receive_frame(&mut decoded) {
            Err(ff::Error::Eof) => {
                if audio.flushed {
                    return Ok(None);
                }
                audio.flushed = true;
                if let Some(resampler) = &mut audio.resampler {
                    let mut output = ff::frame::Audio::new(
                        ff::format::Sample::F32(ff::format::sample::Type::Packed),
                        4096,
                        ff::ChannelLayout::STEREO,
                    );
                    resampler.flush(&mut output).map_err(err)?;
                    return Ok(audio_output(audio, &output));
                }
                return Ok(None);
            }
            Err(error) => {
                waiting(error)?;
                return Ok(None);
            }
            Ok(()) => {}
        }
        if decoded.samples() > 65536
            || decoded.channels() > 8
            || !(8000..=192000).contains(&decoded.rate())
        {
            return Err("movie audio format exceeds decoder limits".into());
        }
        let layout = decoded.channel_layout();
        let recreate = audio.resampler.as_ref().is_none_or(|s| {
            let def = s.input();
            def.format != decoded.format()
                || def.rate != decoded.rate()
                || def.channel_layout != layout
        });
        if recreate {
            audio.resampler = Some(
                ff::software::resampling::Context::get(
                    decoded.format(),
                    layout,
                    decoded.rate(),
                    ff::format::Sample::F32(ff::format::sample::Type::Packed),
                    ff::ChannelLayout::STEREO,
                    AUDIO_RATE,
                )
                .map_err(err)?,
            );
        }
        if audio.next.is_none() {
            audio.next = Some(
                ((decoded.timestamp().unwrap_or(0) as f64 * audio.time_base - self.start)
                    * AUDIO_RATE as f64)
                    .round() as i64,
            );
        }
        let capacity =
            (decoded.samples() * AUDIO_RATE as usize).div_ceil(decoded.rate() as usize) + 256;
        if capacity > 65536 {
            return Err("movie audio conversion exceeds buffer limit".into());
        }
        let mut output = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Packed),
            capacity,
            ff::ChannelLayout::STEREO,
        );
        audio
            .resampler
            .as_mut()
            .unwrap()
            .run(&decoded, &mut output)
            .map_err(err)?;
        Ok(audio_output(audio, &output))
    }
}
fn audio_output(audio: &mut Audio, output: &ff::frame::Audio) -> Option<Decoded> {
    if output.samples() == 0 {
        return None;
    }
    let start = audio.next.unwrap_or(0);
    audio.next = Some(start + output.samples() as i64);
    let skip = (-start).max(0).min(output.samples() as i64) as usize;
    let samples = output.plane::<(f32, f32)>(0)[skip..]
        .iter()
        .map(|&(l, r)| [l, r])
        .collect();
    Some(Decoded::Audio {
        position: start.max(0) as u64,
        samples,
    })
}
fn valid_size(size: Size) -> Result<usize> {
    if size.width == 0 || size.height == 0 || size.width > 4096 || size.height > 4096 {
        return Err("movie dimensions exceed 4096 x 4096 limit".into());
    }
    size.rgba_bytes().ok_or("video dimensions overflow".into())
}
impl Decoder for Movie {
    fn video_stream(&mut self, selection: usize) -> Result<()> {
        let index = *self
            .video_indices
            .get(selection)
            .ok_or("invalid movie video stream")?;
        let source = self
            .input
            .stream(index)
            .ok_or("missing movie video stream")?;
        let mut context =
            ff::codec::context::Context::from_parameters(source.parameters()).map_err(err)?;
        context.set_threading(ff::codec::threading::Config {
            count: 2,
            ..Default::default()
        });
        let codec = ff::decoder::find(context.id()).ok_or("video decoder is unavailable")?;
        let mut options = ff::Dictionary::new();
        options.set("max_pixels", "16777216");
        let video = context
            .decoder()
            .open_as_with(codec, options)
            .and_then(|d| d.video())
            .map_err(err)?;
        let size = Size {
            width: video.width(),
            height: video.height(),
        };
        let bytes = valid_size(size)?;
        let permit = self
            .budget
            .reserve(bytes.checked_mul(12).ok_or("video allocation overflow")?)
            .map_err(|e| e.to_string())?;
        let fps = f64::from(source.avg_frame_rate());
        let fps = if fps.is_finite() && fps > 0.0 {
            fps
        } else {
            f64::from(source.rate())
        };
        if !fps.is_finite() || fps <= 0.0 {
            return Err("movie has no usable frame rate".into());
        }
        self.info.size = size;
        self.info.fps = fps;
        self.info.video_stream = selection;
        self.info.frames = if source.frames() > 0 {
            source.frames() as u64
        } else {
            (self.info.duration * fps).round() as u64
        };
        self.video = video;
        self.video_index = index;
        self.video_time_base = f64::from(source.time_base());
        self._codec_memory = permit;
        self.scaler = None;
        Ok(())
    }
    fn info(&self) -> &Info {
        &self.info
    }
    fn next(&mut self, want_video: bool, want_audio: bool) -> Result<Option<Decoded>> {
        loop {
            if want_video && let Some(frame) = self.receive_video()? {
                return Ok(Some(frame));
            }
            if want_audio && let Some(frame) = self.receive_audio()? {
                return Ok(Some(frame));
            }
            if want_audio
                && !self.audio_end_reported
                && self.audio.as_ref().is_some_and(|a| a.flushed)
            {
                self.audio_end_reported = true;
                return Ok(Some(Decoded::AudioEnd));
            }
            if want_video && let Some((packet, _permit)) = self.video_packets.pop_front() {
                self.queued_bytes -= packet.size() + 256;
                self.video.send_packet(&packet).map_err(err)?;
                continue;
            }
            if want_audio && let Some((packet, _permit)) = self.audio_packets.pop_front() {
                self.queued_bytes -= packet.size() + 256;
                if let Some(audio) = &mut self.audio {
                    audio.decoder.send_packet(&packet).map_err(err)?;
                }
                continue;
            }
            if self.eof {
                if want_video && !self.video_eof && self.video_packets.is_empty() {
                    self.video.send_eof().map_err(err)?;
                    self.video_eof = true;
                    continue;
                }
                if want_audio && !self.audio_eof && self.audio_packets.is_empty() {
                    if let Some(audio) = &mut self.audio {
                        audio.decoder.send_eof().map_err(err)?;
                    }
                    self.audio_eof = true;
                    continue;
                }
                return Ok(
                    if self.video_finished && self.audio.as_ref().is_none_or(|a| a.flushed) {
                        None
                    } else {
                        Some(Decoded::Pending)
                    },
                );
            }
            let mut packet = ff::Packet::empty();
            match packet.read(&mut self.input) {
                Ok(()) => {
                    if packet.size() > 16 * 1024 * 1024 {
                        return Err("movie packet exceeds 16 MiB".into());
                    }
                    if packet.stream() == self.video_index {
                        if want_video {
                            self.video.send_packet(&packet).map_err(err)?;
                        } else {
                            self.queue_packet(packet, true)?;
                        }
                    } else if let Some(audio) = &mut self.audio
                        && packet.stream() == audio.index
                    {
                        if want_audio {
                            audio.decoder.send_packet(&packet).map_err(err)?;
                        } else {
                            self.queue_packet(packet, false)?;
                        }
                    }
                }
                Err(ff::Error::Eof) => {
                    self.eof = true;
                }
                Err(error) => return Err(err(error)),
            }
        }
    }
    fn seek(&mut self, seconds: f64) -> Result<()> {
        let timestamp = ((seconds + self.start) * 1_000_000.0) as i64;
        self.input.seek(timestamp, ..timestamp).map_err(err)?;
        self.video.flush();
        self.next_video = seconds;
        self.eof = false;
        self.video_eof = false;
        self.video_finished = false;
        self.audio_eof = false;
        self.audio_end_reported = false;
        self.video_packets.clear();
        self.audio_packets.clear();
        self.queued_bytes = 0;
        if let Some(audio) = &mut self.audio {
            audio.decoder.flush();
            audio.resampler = None;
            audio.next = None;
            audio.flushed = false;
        }
        Ok(())
    }
    fn audio_stream(&mut self, index: Option<usize>) -> Result<()> {
        let Some(index) = index else {
            self.audio = None;
            return Ok(());
        };
        let index = *self
            .audio_indices
            .get(index)
            .ok_or("invalid movie audio stream")?;
        let source = self
            .input
            .stream(index)
            .ok_or("missing movie audio stream")?;
        let decoder = ff::codec::context::Context::from_parameters(source.parameters())
            .and_then(|c| c.decoder().audio())
            .map_err(err)?;
        self.audio = Some(Audio {
            index,
            time_base: f64::from(source.time_base()),
            decoder,
            resampler: None,
            next: None,
            flushed: false,
        });
        Ok(())
    }
}

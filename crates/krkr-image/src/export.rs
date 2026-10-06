//! Owned PNG/TLG5 export jobs. Compression finishes before publishing a file;
//! progress and cancellation carry no VM, Layer or operating-system handles.
use crate::{Error, Result, Tags};
use krkr_assets::WritePlan;
use krkr_protocol::{
    budget::Budget,
    pixels::{Bytes, Pixels},
};
use std::{
    io::{self, Cursor, Seek, SeekFrom, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
        mpsc::{self, SyncSender},
    },
};

#[derive(Clone, Copy)]
pub enum Format {
    Png { rgba: bool, unfiltered: bool },
    Tlg5,
}
#[derive(Default)]
pub struct Options {
    pub chunks: Vec<([u8; 4], Vec<u8>)>,
    pub compression: Option<i32>,
    pub tags: Tags,
    reservations: Vec<krkr_protocol::budget::Permit>,
}
impl Options {
    pub fn reserve_metadata(&mut self, budget: &Budget, bytes: usize) -> Result<()> {
        self.reservations.push(budget.reserve(bytes)?);
        Ok(())
    }
}
pub struct Request {
    pub pixels: Arc<Pixels>,
    pub target: Option<WritePlan>,
    pub format: Format,
    pub options: Options,
    pub budget: Budget,
}
fn error(e: impl std::fmt::Display) -> Error {
    Error::Codec(e.to_string())
}
fn check(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Acquire) {
        Err(io::Error::other("image export cancelled"))
    } else {
        Ok(())
    }
}

struct Output<'a> {
    data: Cursor<Vec<u8>>,
    limit: usize,
    cancel: &'a AtomicBool,
    progress: &'a AtomicU8,
    tlg: Option<TlgProgress>,
}
// Parse only the fixed channel-packet envelopes of the existing TLG5 encoder.
// Counting completed four-channel stripes gives real row progress even when
// the image compresses to a tiny fraction of its raw size.
struct TlgProgress {
    start: u64,
    height: u32,
    channels: u32,
    header: [u8; 5],
    used: usize,
    left: usize,
}
impl TlgProgress {
    fn write(&mut self, offset: u64, mut bytes: &[u8], progress: &AtomicU8) {
        if offset < self.start {
            let skip = (self.start - offset).min(bytes.len() as u64) as usize;
            bytes = &bytes[skip..];
        }
        while !bytes.is_empty() && self.channels < self.height.div_ceil(4) * 4 {
            if self.used < 5 {
                let n = (5 - self.used).min(bytes.len());
                self.header[self.used..self.used + n].copy_from_slice(&bytes[..n]);
                self.used += n;
                bytes = &bytes[n..];
                if self.used < 5 {
                    break;
                }
                self.left = u32::from_le_bytes(self.header[1..].try_into().unwrap()) as usize;
            }
            let n = self.left.min(bytes.len());
            self.left -= n;
            bytes = &bytes[n..];
            if self.left == 0 {
                self.channels += 1;
                self.used = 0;
                if self.channels.is_multiple_of(4) {
                    progress.store(
                        ((self.channels / 4 * 4).min(self.height) * 100 / self.height) as u8,
                        Ordering::Release,
                    );
                }
            }
        }
    }
}
impl Write for Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        check(self.cancel)?;
        let end = (self.data.position() as usize)
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("encoded image overflow"))?;
        if end > self.limit {
            return Err(io::Error::other("encoded image exceeds reserved bound"));
        }
        let offset = self.data.position();
        let n = self.data.write(bytes)?;
        if let Some(tlg) = &mut self.tlg {
            tlg.write(offset, &bytes[..n], self.progress);
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        check(self.cancel)
    }
}
impl Seek for Output<'_> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        check(self.cancel)?;
        self.data.seek(from)
    }
}
impl Request {
    pub fn run(self, cancel: &AtomicBool, progress: &AtomicU8) -> Result<Option<Bytes>> {
        check(cancel)?;
        let size = crate::image_size(self.pixels.size.width, self.pixels.size.height)?;
        let raw = size
            .rgba_bytes()
            .ok_or(Error::Message("image export size overflow"))?;
        let rgba = self
            .pixels
            .main
            .as_ref()
            .ok_or(Error::Message("image has no main plane"))?
            .as_slice();
        if rgba.len() != raw {
            return Err(Error::Message("image export pixel size mismatch"));
        }
        let tag_bytes = self
            .options
            .tags
            .iter()
            .map(|(k, v)| k.len() + v.len() + 32)
            .sum::<usize>();
        let limit = raw
            .checked_add(raw / 8)
            .and_then(|v| v.checked_add(1024 * 1024 + tag_bytes))
            .ok_or(Error::Message("image export size overflow"))?;
        let output_permit = self.budget.reserve(limit)?;
        let _scratch = self.budget.reserve(
            size.width as usize * 32 + size.height.div_ceil(4) as usize * 4 + 1024 * 1024,
        )?;
        let mut out = Output {
            data: Cursor::new(Vec::with_capacity(limit)),
            limit,
            cancel,
            progress,
            tlg: None,
        };
        match self.format {
            Format::Png {
                rgba: force_alpha,
                unfiltered,
            } => {
                let alpha = force_alpha || rgba.as_chunks::<4>().0.iter().any(|v| v[3] != 255);
                let mut encoder = png::Encoder::new(&mut out, size.width, size.height);
                encoder.set_depth(png::BitDepth::Eight);
                encoder.set_color(if alpha {
                    png::ColorType::Rgba
                } else {
                    png::ColorType::Rgb
                });
                if let Some(level) = self.options.compression {
                    if !(-1..=9).contains(&level) {
                        return Err(Error::Message("PNG compression must be -1..9"));
                    }
                    encoder.set_deflate_compression(png::DeflateCompression::Level(if level < 0 {
                        6
                    } else {
                        level as u8
                    }));
                }
                if unfiltered || self.options.compression == Some(0) {
                    encoder.set_filter(png::Filter::NoFilter);
                }
                let mut writer = encoder.write_header().map_err(error)?;
                for (name, data) in self.options.chunks {
                    writer
                        .write_chunk(png::chunk::ChunkType(name), &data)
                        .map_err(error)?;
                }
                crate::png_image::write_tags(&mut writer, &self.options.tags)?;
                {
                    let mut stream = writer.stream_writer().map_err(error)?;
                    let mut rgb = vec![0; size.width as usize * 3];
                    for (y, row) in rgba.chunks_exact(size.width as usize * 4).enumerate() {
                        check(cancel)?;
                        if alpha {
                            stream.write_all(row)?;
                        } else {
                            for (p, d) in row
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .zip(rgb.as_chunks_mut::<3>().0.iter_mut())
                            {
                                d.copy_from_slice(&p[..3]);
                            }
                            stream.write_all(&rgb)?;
                        }
                        progress.store(
                            ((y + 1) * 99 / size.height as usize) as u8,
                            Ordering::Release,
                        );
                    }
                    stream.finish().map_err(error)?;
                }
                writer.finish().map_err(error)?;
            }
            Format::Tlg5 => {
                use tlg::tlg_type::{PixelLayout, TlgEncoderTrait};
                // Consume the original readback allocation when uniquely held.
                let main = match Arc::try_unwrap(self.pixels) {
                    Ok(mut pixels) => pixels.main.take().unwrap(),
                    Err(pixels) => {
                        let mut main = Bytes::zeroed(raw, &self.budget)?;
                        main.as_mut_slice()
                            .copy_from_slice(pixels.main.as_ref().unwrap().as_slice());
                        main
                    }
                };
                let (data, _input_permit) = main.into_parts();
                let mut tags = String::new();
                for (k, v) in self.options.tags {
                    use std::fmt::Write;
                    write!(&mut tags, "{}:{}={}:{},", k.len(), k, v.len(), v).unwrap();
                }
                if !tags.is_empty() {
                    out.write_all(b"TLG0.0\0sds\x1a")?;
                    out.write_all(&[0; 4])?;
                }
                let raw_start = out.stream_position()?;
                out.tlg = Some(TlgProgress {
                    start: raw_start + 24 + u64::from(size.height.div_ceil(4)) * 4,
                    height: size.height,
                    channels: 0,
                    header: [0; 5],
                    used: 0,
                    left: 0,
                });
                tlg::tlg5::Tlg5Encoder::from_raw(data, PixelLayout::Rgba, size.width, size.height)
                    .encode_to(&mut out)
                    .map_err(error)?;
                if !tags.is_empty() {
                    let end = out.stream_position()?;
                    out.seek(SeekFrom::Start(11))?;
                    out.write_all(&((end - raw_start) as u32).to_le_bytes())?;
                    out.seek(SeekFrom::Start(end))?;
                    out.write_all(b"tags")?;
                    out.write_all(&(tags.len() as u32).to_le_bytes())?;
                    out.write_all(tags.as_bytes())?;
                }
            }
        }
        check(cancel)?;
        progress.store(100, Ordering::Release);
        let bytes = Bytes::with_permit(out.data.into_inner(), output_permit);
        if let Some(target) = self.target {
            check(cancel)?;
            let mut file = target.create()?;
            for chunk in bytes.as_slice().chunks(64 * 1024) {
                check(cancel)?;
                file.write_all(chunk)?;
            }
            check(cancel)?;
            file.finish()?;
            Ok(None)
        } else {
            Ok(Some(bytes))
        }
    }
}

#[derive(Default)]
struct State {
    cancel: AtomicBool,
    progress: AtomicU8,
    result: Mutex<Option<std::result::Result<(), String>>>,
}
pub struct Job(Arc<State>);
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl Job {
    pub fn cancel(&self) {
        self.0.cancel.store(true, Ordering::Release);
    }
    pub fn progress(&self) -> u8 {
        self.0.progress.load(Ordering::Acquire)
    }
    pub fn take_result(&self) -> Option<std::result::Result<(), String>> {
        self.0.result.lock().unwrap().take()
    }
}
type Task = (Request, Arc<State>);
#[derive(Default, Clone)]
pub struct Service(Arc<Mutex<Option<SyncSender<Task>>>>);
impl Service {
    pub fn submit(&self, request: Request) -> std::result::Result<Job, String> {
        if request.target.is_none() {
            return Err("background export needs a file target".into());
        }
        let mut sender = self.0.lock().unwrap();
        if sender.is_none() {
            let (tx, rx) = mpsc::sync_channel::<Task>(64);
            std::thread::Builder::new()
                .name("krkr-image-export".into())
                .spawn(move || {
                    while let Ok((request, state)) = rx.recv() {
                        let result = request
                            .run(&state.cancel, &state.progress)
                            .map(|_| ())
                            .map_err(|e| e.to_string());
                        *state.result.lock().unwrap() = Some(result);
                    }
                })
                .map_err(|e| e.to_string())?;
            *sender = Some(tx);
        }
        let state = Arc::new(State::default());
        sender
            .as_ref()
            .unwrap()
            .try_send((request, state.clone()))
            .map_err(|e| {
                match e {
                    mpsc::TrySendError::Full(_) => "image export queue is full",
                    mpsc::TrySendError::Disconnected(_) => "image export worker stopped",
                }
                .to_string()
            })?;
        Ok(Job(state))
    }
}

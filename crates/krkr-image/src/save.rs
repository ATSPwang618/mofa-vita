//! Image export owns its pixels and write plan. The worker never borrows a VM,
//! VFS or GPU object, and only publishes a completed file.
mod bitmap;
mod encoding;
use crate::{Error, Result, Tags, codec::error};
use krkr_assets::WritePlan;
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::{
    io::{self, BufWriter, Seek, SeekFrom, Write},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitmapDepth {
    Indexed,
    Rgb,
    Rgba,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Bmp(BitmapDepth),
    Png { alpha: bool },
    Jpeg { quality: u8 },
    Tlg { six: bool, alpha: bool },
}
impl Format {
    /// Stock AcceptSave uses case-sensitive prefixes, independently of the
    /// destination extension. Unknown suffixes retain the format's defaults.
    pub fn parse(mode: &str) -> Result<Self> {
        if mode.starts_with("bmp") || matches!(mode, ".bmp" | ".dib") {
            Ok(Self::Bmp(match mode {
                "bmp8" => BitmapDepth::Indexed,
                "bmp24" => BitmapDepth::Rgb,
                _ => BitmapDepth::Rgba,
            }))
        } else if mode.starts_with("png") || mode == ".png" {
            Ok(Self::Png {
                alpha: mode != "png24",
            })
        } else if mode.starts_with("jpg") || mode == ".jpg" {
            let quality = if mode.starts_with("jpg") && mode.len() > 3 {
                let n = mode[3..]
                    .bytes()
                    .filter(u8::is_ascii_digit)
                    .fold(0u32, |n, d| {
                        n.wrapping_mul(10).wrapping_add(u32::from(d - b'0'))
                    });
                if n == 0 { 10 } else { n.min(100) as u8 }
            } else {
                90
            };
            Ok(Self::Jpeg { quality })
        } else if mode.starts_with("tlg") || matches!(mode, ".tlg" | ".tlg5" | ".tlg6") {
            Ok(Self::Tlg {
                six: mode.starts_with("tlg6"),
                alpha: !matches!(mode, "tlg524" | "tlg624"),
            })
        } else {
            Err(Error::Codec(format!(
                "unsupported image save format: {mode}"
            )))
        }
    }
}
pub struct Request {
    pub target: WritePlan,
    pub format: Format,
    pub pixels: Pixels,
    pub tags: Tags,
    pub budget: Budget,
}
impl Request {
    pub fn write(self, cancelled: &AtomicBool) -> Result<()> {
        check(cancelled)?;
        let size = crate::image_size(self.pixels.size.width, self.pixels.size.height)?;
        let main = self
            .pixels
            .main
            .ok_or(Error::Message("image has no main plane"))?;
        drop(self.pixels.province);
        if Some(main.as_slice().len()) != size.rgba_bytes() {
            return Err(Error::Message("image save pixel size mismatch"));
        }
        // Includes the writer buffer. Reserve codec working memory before the
        // encoder starts; input pixels already carry their own byte permit.
        let scratch = self.format.scratch(size)?;
        let mut buffer = self.format.buffer_size(size);
        let _scratch = match self.budget.reserve(scratch) {
            Ok(permit) => permit,
            Err(_) if buffer > 64 * 1024 => {
                // Coalescing must not reject an export that fitted the old
                // streaming buffer. Fall back before creating a staging file.
                let permit = self.budget.reserve(scratch - (buffer - 64 * 1024))?;
                buffer = 64 * 1024;
                permit
            }
            Err(error) => return Err(error.into()),
        };
        let mut file = self.target.create()?;
        encode_buffered(
            &mut file,
            self.format,
            main,
            size,
            self.tags,
            cancelled,
            buffer,
        )?;
        check(cancelled)?;
        file.finish()?;
        Ok(())
    }
}
fn encode_buffered(
    file: &mut (impl Write + Seek),
    format: Format,
    main: Bytes,
    size: Size,
    tags: Tags,
    cancelled: &AtomicBool,
    buffer: usize,
) -> Result<()> {
    let mut output = Cancellable {
        inner: BufWriter::with_capacity(buffer, file),
        cancelled,
    };
    match format {
        Format::Bmp(depth) => bitmap::encode(&mut output, main.as_slice(), size, depth)?,
        Format::Png { alpha } => encoding::png(&mut output, main.as_slice(), size, alpha)?,
        Format::Jpeg { quality } => encoding::jpeg(&mut output, main.as_slice(), size, quality)?,
        Format::Tlg { six, alpha } => encoding::tlg(&mut output, main, size, six, alpha, tags)?,
    }
    output.flush()?;
    Ok(())
}
impl Format {
    fn buffer_size(self, size: Size) -> usize {
        if matches!(self, Self::Bmp(_)) {
            // Common save thumbnails fit into one write. Keep large exports
            // bounded, and charge every byte of capacity to the scratch budget.
            size.rgba_bytes()
                .unwrap_or(usize::MAX)
                .saturating_add(1078)
                .clamp(64 * 1024, 256 * 1024)
        } else {
            64 * 1024
        }
    }
    fn scratch(self, size: Size) -> Result<usize> {
        let w = size.width as u64;
        let h = size.height as u64;
        let bytes = match self {
            Self::Bmp(_) => w * 4 + self.buffer_size(size) as u64,
            Self::Png { .. } => w * 32 + 1024 * 1024,
            // jpeg-encoder's progressive path retains padded YCbCr rows and
            // three coefficient vectors (including their reserved capacity).
            Self::Jpeg { .. } => w.div_ceil(16) * 16 * h.div_ceil(16) * 16 * 9 + 64 * 1024,
            Self::Tlg { six: false, .. } => w * 32 + h.div_ceil(4) * 4 + 1024 * 1024,
            // tlg-rs 0.1.1 retains its encoded TLG6 bitstream. Bound worst-case
            // Golomb bytes and Vec growth, plus channel rows and filter LZSS.
            // At most 7 bytes per channel sample (escaped Golomb + run codes),
            // doubled for Vec capacity; include per-stripe lengths separately.
            Self::Tlg { six: true, .. } => {
                w * h * 56
                    + w * 192
                    + h.div_ceil(8) * 32
                    + w.div_ceil(8) * h.div_ceil(8) * 4
                    + 1024 * 1024
            }
        };
        usize::try_from(bytes).map_err(|_| Error::Message("image encoding size overflow"))
    }
}
fn check(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        // Interrupted would cause write_all to retry indefinitely.
        Err(io::Error::other("image saving cancelled"))
    } else {
        Ok(())
    }
}
struct Cancellable<'a, W> {
    inner: W,
    cancelled: &'a AtomicBool,
}
impl<W: Write> Write for Cancellable<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        check(self.cancelled)?;
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        check(self.cancelled)?;
        self.inner.flush()
    }
}
impl<W: Seek> Seek for Cancellable<'_, W> {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        check(self.cancelled)?;
        self.inner.seek(from)
    }
}

#[cfg(test)]
#[path = "../tests/save_buffer/internal.rs"]
mod tests;

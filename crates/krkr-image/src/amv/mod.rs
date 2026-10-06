//! AlphaMovie AJPM streams: indexed frames, two alpha modes, bounded CPU decoding.
mod entropy;
mod tables;
use crate::{Error, Result};
use krkr_assets::ReadPlan;
use krkr_protocol::{budget::Budget, graphics::Size, pixels::Bytes};
use std::{
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

struct Frame {
    offset: u64,
    length: usize,
    alpha: usize,
    size: Size,
}
pub struct Movie {
    plan: Arc<ReadPlan>,
    frames: Vec<Frame>,
    quant: [[u8; 64]; 2],
    compressed_alpha: bool,
    pub size: Size,
    pub rate: u32,
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn u16_at(bytes: &[u8], offset: usize) -> u32 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as u32
}
fn check(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(Error::Message("AMV operation cancelled"))
    } else {
        Ok(())
    }
}
fn size(width: u32, height: u32) -> Result<Size> {
    if width > 16384 || height > 16384 || width as u64 * height as u64 > 16 * 1024 * 1024 {
        return Err(Error::Message("AMV dimensions exceed pixel limit"));
    }
    Ok(Size { width, height })
}
impl Movie {
    pub fn open(plan: Arc<ReadPlan>, cancelled: &dyn Fn() -> bool) -> Result<Option<Self>> {
        if plan.bytes < 40 {
            return Ok(None);
        }
        let mut stream = plan.open_interruptible(cancelled)?;
        let mut header = [0; 40];
        stream.read_exact(&mut header)?;
        if &header[..4] != b"AJPM" {
            return Err(Error::Message("invalid AMV signature"));
        }
        let size = size(u16_at(&header, 32), u16_at(&header, 34))?;
        let count = u32_at(&header, 20) as usize;
        if count > 1_000_000 {
            return Err(Error::Message("AMV frame count limit"));
        }
        let compressed_alpha = u32_at(&header, 36) == 2;
        let quant_end = u32_at(&header, 12) as u64;
        if quant_end != if compressed_alpha { 168 } else { 232 } {
            return Err(Error::Message("invalid AMV quantization header"));
        }
        let mut quant = [[0; 64]; 2];
        for table in &mut quant {
            stream.read_exact(table)?;
        }
        stream.seek(SeekFrom::Start(quant_end))?;
        let mut frames = Vec::new();
        let mut offset = quant_end;
        for _ in 0..count {
            check(cancelled)?;
            let header_len = if compressed_alpha { 24 } else { 20 };
            let mut data = [0; 24];
            stream.read_exact(&mut data[..header_len])?;
            let size = self::size(u16_at(&data, 16), u16_at(&data, 18))?;
            let length = (u32_at(&data, 4) as usize)
                .checked_sub(header_len - 8)
                .ok_or(Error::Message("invalid AMV frame size"))?;
            let alpha = if compressed_alpha {
                u32_at(&data, 20) as usize
            } else {
                0
            };
            if alpha > length || length > 64 * 1024 * 1024 {
                return Err(Error::Message("invalid AMV payload size"));
            }
            offset += header_len as u64;
            let end = offset
                .checked_add(length as u64)
                .ok_or(Error::Message("AMV offset overflow"))?;
            if end > plan.bytes {
                return Err(Error::Message("truncated AMV frame"));
            }
            frames.push(Frame {
                offset,
                length,
                alpha,
                size,
            });
            // Zero-size frames in the reference consume only the frame header.
            if size.width != 0 && size.height != 0 {
                offset = end;
            }
            stream.seek(SeekFrom::Start(offset))?;
        }
        Ok(Some(Self {
            plan,
            frames,
            quant,
            compressed_alpha,
            size,
            rate: u32_at(&header, 28),
        }))
    }
    pub fn count(&self) -> usize {
        self.frames.len()
    }
    pub fn decode(
        &self,
        index: usize,
        budget: &Budget,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(Size, Bytes)> {
        let frame = self
            .frames
            .get(index)
            .ok_or(Error::Message("AMV frame index out of bounds"))?;
        let bytes = frame
            .size
            .rgba_bytes()
            .ok_or(Error::Message("AMV pixel size overflow"))?;
        let mut output = Bytes::zeroed(bytes, budget)?;
        if bytes == 0 {
            return Ok((frame.size, output));
        }
        if frame.size.width % 16 != 0 || frame.size.height % 16 != 0 {
            return Err(Error::Message(
                "AMV coded dimensions must be multiples of 16",
            ));
        }
        let mut stream = self.plan.open_interruptible(cancelled)?;
        stream.seek(SeekFrom::Start(frame.offset))?;
        let mut input = Bytes::zeroed(frame.length, budget)?;
        read(&mut *stream, input.as_mut_slice(), cancelled)?;
        let mut alpha = if self.compressed_alpha {
            Some(Bytes::zeroed(bytes / 4, budget)?)
        } else {
            None
        };
        if let Some(alpha) = &mut alpha {
            let mut zlib = flate2::read::ZlibDecoder::new(&input.as_slice()[..frame.alpha]);
            read(&mut zlib, alpha.as_mut_slice(), cancelled)?;
            let mut extra = [0];
            if zlib.read(&mut extra)? != 0 {
                return Err(Error::Message("AMV alpha plane size mismatch"));
            }
        }
        let mut bits = entropy::Bits::new(&input.as_slice()[frame.alpha..]);
        let (mut uv_dc, mut y_dc) = (0, 0);
        let width = frame.size.width as usize;
        for by in (0..frame.size.height as usize).step_by(16) {
            check(cancelled)?;
            for bx in (0..width).step_by(16) {
                let u = bits.block(true, &mut uv_dc, &self.quant[1])?;
                let v = bits.block(true, &mut uv_dc, &self.quant[1])?;
                let mut y = [[0; 64]; 4];
                for block in &mut y {
                    *block = bits.block(false, &mut y_dc, &self.quant[0])?;
                }
                let mut a = [[0; 64]; 4];
                if !self.compressed_alpha {
                    for block in &mut a {
                        *block = bits.block(false, &mut y_dc, &self.quant[0])?;
                    }
                }
                for row in 0..16 {
                    for col in 0..16 {
                        let block = row / 8 * 2 + col / 8;
                        let within = row % 8 * 8 + col % 8;
                        let luma = i32::from(y[block][within]);
                        let u = i32::from(u[row / 2 * 8 + col / 2]) - 128;
                        let v = i32::from(v[row / 2 * 8 + col / 2]) - 128;
                        let pixel = (by + row) * width + bx + col;
                        output.as_mut_slice()[pixel * 4..pixel * 4 + 4].copy_from_slice(&[
                            (luma + ((359 * v) >> 8)).clamp(0, 255) as u8,
                            (luma - ((88 * u + 183 * v) >> 8)).clamp(0, 255) as u8,
                            (luma + ((454 * u) >> 8)).clamp(0, 255) as u8,
                            alpha
                                .as_ref()
                                .map_or(a[block][within], |a| a.as_slice()[pixel]),
                        ]);
                    }
                }
            }
        }
        Ok((frame.size, output))
    }
}
fn read(
    stream: &mut (impl Read + ?Sized),
    bytes: &mut [u8],
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    for chunk in bytes.chunks_mut(16384) {
        check(cancelled)?;
        stream.read_exact(chunk)?;
    }
    Ok(())
}

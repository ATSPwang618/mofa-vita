//! Offline scale metadata keeps script coordinates independent of stored pixels.
use crate::{Error, Result};
use krkr_protocol::{budget::Budget, graphics::Size, pixels::Bytes};
use std::{io::Read, sync::atomic::AtomicBool};

pub const SUFFIX: &str = ".krkr-scale";
const MAGIC: &[u8; 8] = b"KRKRSCL1";
#[derive(Clone, Copy, Debug)]
pub struct Metadata {
    pub logical: Size,
    pub stored: Size,
}
impl Metadata {
    pub fn encode(self) -> Result<[u8; 24]> {
        self.validate(self.stored)?;
        let mut out = [0; 24];
        out[..8].copy_from_slice(MAGIC);
        for (chunk, value) in out[8..].as_chunks_mut::<4>().0.iter_mut().zip([
            self.logical.width,
            self.logical.height,
            self.stored.width,
            self.stored.height,
        ]) {
            chunk.copy_from_slice(&value.to_le_bytes());
        }
        Ok(out)
    }
    pub fn read(plan: krkr_assets::ReadPlan, cancelled: &AtomicBool) -> Result<Self> {
        if plan.bytes != 24 {
            return Err(Error::Message("invalid scale metadata length"));
        }
        let mut bytes = [0; 24];
        plan.open_interruptible(&|| cancelled.load(std::sync::atomic::Ordering::Relaxed))?
            .read_exact(&mut bytes)?;
        Self::decode(&bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != 24 || bytes.get(..8) != Some(MAGIC) {
            return Err(Error::Message("invalid scale metadata"));
        }
        let n: Vec<_> = bytes[8..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect();
        let metadata = Self {
            logical: Size {
                width: n[0],
                height: n[1],
            },
            stored: Size {
                width: n[2],
                height: n[3],
            },
        };
        metadata.validate(metadata.stored)?;
        Ok(metadata)
    }
    pub fn validate(self, stored: Size) -> Result<()> {
        crate::image_size(self.logical.width, self.logical.height)?;
        crate::image_size(self.stored.width, self.stored.height)?;
        // Compressed GPU storage rounds dimensions up to powers of two. It
        // can exceed the logical canvas even when the display is downscaled.
        // Both dimensions are bounded above; sampling works in either direction.
        if self.stored != stored {
            return Err(Error::Message(
                "scale metadata does not match stored image dimensions",
            ));
        }
        Ok(())
    }
}

/// Nearest logical sampling preserves pixel/atlas boundaries. The offline
/// downsampler has already performed the antialiasing in premultiplied alpha.
pub fn expand(
    source: &Bytes,
    stored: Size,
    logical: Size,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    expand_channels(source, stored, logical, 4, budget, cancelled)
}
pub(crate) fn expand_channels(
    source: &Bytes,
    stored: Size,
    logical: Size,
    channels: usize,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    Metadata { logical, stored }.validate(stored)?;
    if source.as_slice().len()
        != stored
            .rgba_bytes()
            .ok_or(Error::Message("image size overflow"))?
            / 4
            * channels
    {
        return Err(Error::Message("scaled pixel length mismatch"));
    }
    let mut out = Bytes::zeroed(
        logical
            .rgba_bytes()
            .ok_or(Error::Message("image size overflow"))?
            / 4
            * channels,
        budget,
    )?;
    // The nearest-neighbour coordinate is an integer rational sequence.
    // Advance its quotient/remainder instead of dividing for every pixel;
    // on 32-bit hosts the old u64 division is particularly expensive.
    let denominator = u64::from(logical.width) * 2;
    let first = u64::from(stored.width);
    let step = first * 2;
    let whole_step = (step / denominator) as usize;
    let remainder_step = step % denominator;
    let stride = logical.width as usize * channels;
    let mut previous_y = None;
    for y in 0..logical.height {
        crate::check(cancelled)?;
        let sy = ((u64::from(y) * 2 + 1) * u64::from(stored.height)
            / (u64::from(logical.height) * 2)) as usize;
        let start = y as usize * stride;
        if previous_y == Some(sy) {
            out.as_mut_slice().copy_within(start - stride..start, start);
            continue;
        }
        let mut sx = (first / denominator) as usize;
        let mut remainder = first % denominator;
        let input = &source.as_slice()[sy * stored.width as usize * channels..]
            [..stored.width as usize * channels];
        for pixel in out.as_mut_slice()[start..start + stride].chunks_exact_mut(channels) {
            pixel.copy_from_slice(&input[sx * channels..][..channels]);
            sx += whole_step;
            remainder += remainder_step;
            if remainder >= denominator {
                remainder -= denominator;
                sx += 1;
            }
        }
        previous_y = Some(sy);
    }
    Ok(out)
}

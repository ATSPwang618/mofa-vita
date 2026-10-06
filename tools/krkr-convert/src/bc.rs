//! In-process BC1/BC3 encoding. No intermediate image files or child processes.
use crate::media::Result;
use krkr_protocol::{graphics::Size, texture::Format};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Fast,
    #[default]
    Balanced,
    High,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Storage {
    #[default]
    Bc,
    BcCrunch,
}

#[derive(Default, serde::Serialize)]
pub struct Timings {
    pub bc_pack_ms: u64,
    pub bc_encode_ms: u64,
    pub quality_check_ms: u64,
    pub bc_attempts: u64,
}
#[derive(Default)]
struct Counters {
    pack: AtomicU64,
    bc: AtomicU64,
    check: AtomicU64,
    bc_count: AtomicU64,
}
fn elapsed(counter: &AtomicU64, start: Instant) {
    counter.fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
}

pub struct Encoder {
    quality: Quality,
    counters: Counters,
}
impl Encoder {
    pub fn new(quality: Quality) -> Self {
        Self {
            quality,
            counters: Default::default(),
        }
    }
    pub fn timings(&self) -> Timings {
        let c = &self.counters;
        Timings {
            bc_pack_ms: c.pack.load(Ordering::Relaxed) / 1000,
            bc_encode_ms: c.bc.load(Ordering::Relaxed) / 1000,
            quality_check_ms: c.check.load(Ordering::Relaxed) / 1000,
            bc_attempts: c.bc_count.load(Ordering::Relaxed),
        }
    }
    pub fn check(
        &self,
        size: Size,
        rgba: &[u8],
        data: &[u8],
        color: bool,
    ) -> Result<Option<String>> {
        let start = Instant::now();
        let result = crate::texture_quality::check(size, rgba, data, color);
        elapsed(&self.counters.check, start);
        result
    }

    pub fn encode(&self, size: Size, rgba: &[u8], format: Format) -> Result<Vec<u8>> {
        let transparent = match format {
            Format::Bc1Rgb => false,
            Format::Bc3Rgba => true,
            _ => return Err("encoder requires linear BC1 RGB or BC3 RGBA".into()),
        };
        let length = format
            .vita()
            .unwrap()
            .byte_len(size)
            .ok_or("BC tiles require POT dimensions >=8")?;
        if size.width > 1024 || size.height > 1024 || size.rgba_bytes() != Some(rgba.len()) {
            return Err("BC input must fit a 1024x1024 RGBA texture".into());
        }
        let quality = match self.quality {
            Quality::Fast => rgbcx::Quality::Fast,
            Quality::Balanced => rgbcx::Quality::Balanced,
            Quality::High => rgbcx::Quality::High,
        };
        let start = Instant::now();
        let blocks = rgbcx::encode(
            size.width,
            size.height,
            rgba,
            if transparent {
                rgbcx::Format::Bc3
            } else {
                rgbcx::Format::Bc1
            },
            quality,
        );
        elapsed(&self.counters.bc, start);
        self.counters.bc_count.fetch_add(1, Ordering::Relaxed);
        let blocks = blocks.map_err(|e| e.to_string())?;
        debug_assert_eq!(blocks.len(), length);
        krkr_image::compressed::ktx_format(size, format, &blocks).map_err(|e| e.to_string())
    }

    pub fn encode_packed(&self, size: Size, rgba: &[u8], format: Format) -> Result<Vec<u8>> {
        let ktx = self.encode(size, rgba, format)?;
        let start = Instant::now();
        let packed = krkr_image::packed_bc::wrap_packed_bc(&ktx).map_err(|e| e.to_string());
        elapsed(&self.counters.pack, start);
        packed
    }
}

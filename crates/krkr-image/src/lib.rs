//! Kirikiri image loading without VM or GPU types. Resource probing precedes
//! decoding so the host can reserve destination textures before pixel work.
pub mod amv;
mod bmp32;
mod codec;
pub mod compressed;
pub mod cursor;
pub mod export;
pub mod icon;
mod jpeg;
pub mod packed_bc;
mod png_image;
pub mod psd;
pub mod resolve;
pub mod save;
pub mod scale;
mod tlg;
mod transform;
use krkr_assets::ReadPlan;
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::{
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Message(&'static str),
    #[error("{0}")]
    Codec(String),
    #[error(transparent)]
    Budget(#[from] krkr_protocol::budget::BudgetError),
    #[error(transparent)]
    Asset(#[from] krkr_assets::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
pub type Tags = Vec<(String, String)>;
/// Rewrite legacy Shift-JIS TLG tags as UTF-8 without re-encoding pixels.
/// Returns `None` when the metadata already needs no changes.
pub fn normalize_tlg_metadata(data: &[u8]) -> Result<Option<Vec<u8>>> {
    tlg::normalize_metadata(data)
}

pub struct Request {
    pub main: ReadPlan,
    pub scale: Option<ReadPlan>,
    pub mask: Option<ReadPlan>,
    pub province: Option<ReadPlan>,
    pub key: u32,
    pub province_size: Option<Size>,
    pub(crate) grayscale: bool,
    pub budget: Budget,
}
pub struct Prepared {
    main: Encoded,
    main_logical: Size,
    mask: Option<Encoded>,
    province: Option<Encoded>,
    key: u32,
    pub size: Size,
    pub province_only: bool,
    grayscale: bool,
    budget: Budget,
}
struct Encoded {
    bytes: Bytes,
    format: codec::Format,
    size: Size,
    texture: Option<compressed::Header>,
}
pub struct Decoded {
    pub pixels: Pixels,
    pub tags: Tags,
}
impl Request {
    /// Construct a request after VM-side media have resolved their file plans.
    pub fn from_plans(
        main: ReadPlan,
        mask: Option<ReadPlan>,
        province: Option<ReadPlan>,
        key: u32,
        province_size: Option<Size>,
        grayscale: bool,
        budget: Budget,
    ) -> Self {
        Self {
            main,
            scale: None,
            mask,
            province,
            key,
            province_size,
            grayscale,
            budget,
        }
    }
    pub fn probe(self, cancelled: &AtomicBool) -> Result<Prepared> {
        let main = Encoded::read(self.main, &self.budget, cancelled)?;
        let logical = self
            .scale
            .map(|plan| scale::Metadata::read(plan, cancelled))
            .transpose()?;
        if let Some(metadata) = logical {
            metadata.validate(main.size)?;
        }
        let mask = self
            .mask
            .map(|plan| Encoded::read(plan, &self.budget, cancelled))
            .transpose()?;
        let province = self
            .province
            .map(|plan| Encoded::read(plan, &self.budget, cancelled))
            .transpose()?;
        let size = self
            .province_size
            .unwrap_or(logical.map_or(main.size, |m| m.logical));
        if let Some(mask) = &mask
            && mask.size != main.size
        {
            return Err(Error::Message("mask image size mismatch"));
        }
        let province_image = if self.province_size.is_some() {
            Some(&main)
        } else {
            province.as_ref()
        };
        if let Some(province) = province_image
            && !self.grayscale
            && (province.size.width > size.width || province.size.height > size.height)
        {
            return Err(Error::Message("province image size mismatch"));
        }
        Ok(Prepared {
            main_logical: logical.map_or(main.size, |m| m.logical),
            main,
            mask,
            province,
            key: self.key,
            size,
            province_only: self.province_size.is_some(),
            grayscale: self.grayscale,
            budget: self.budget,
        })
    }
}
impl Prepared {
    /// Actual encoded format chosen by the shared loader, independent of suffix.
    pub fn format_name(&self) -> &'static str {
        match self.main.format {
            codec::Format::Png => "png",
            codec::Format::Bmp => "bmp",
            codec::Format::Jpeg => "jpeg",
            codec::Format::Webp => "webp",
            codec::Format::Tlg => "tlg",
            codec::Format::Ktx => "ktx",
            codec::Format::PackedBc => "kbct",
        }
    }
    pub fn has_province(&self) -> bool {
        self.province_only || self.province.is_some()
    }
    /// Only ordinary color images can bypass CPU pixel transforms. Masks,
    /// color keys, palette indices and province loading retain the decoder.
    pub fn is_compressed_upload(&self) -> bool {
        matches!(
            self.main.format,
            codec::Format::Ktx | codec::Format::PackedBc
        ) && matches!(self.key, 0x02ffffff | 0x1fffffff)
            && self.mask.is_none()
            && !self.has_province()
            && !self.grayscale
    }
    /// Native block storage for an image that can bypass CPU transforms.
    /// Backends without format support may still allocate an RGBA fallback.
    pub fn compressed_upload_bytes(&self) -> Option<usize> {
        self.is_compressed_upload()
            .then(|| {
                let header = self.main.texture.as_ref().expect("validated KTX format");
                krkr_protocol::texture::Compressed::payload_len(
                    header.size,
                    header.tile_size,
                    header.format,
                )
            })
            .flatten()
    }
    pub fn into_compressed(self) -> Result<krkr_protocol::texture::Compressed> {
        self.into_compressed_with_tags().map(|(texture, _)| texture)
    }
    pub fn into_compressed_with_tags(self) -> Result<(krkr_protocol::texture::Compressed, Tags)> {
        if !self.is_compressed_upload() {
            return Err(Error::Message("image requires CPU pixel processing"));
        }
        let header = self.main.texture.expect("validated KTX header");
        let texture = krkr_protocol::texture::Compressed::tiled(
            self.main.size,
            header.tile_size,
            header.format,
            self.main.bytes,
            header.offset,
        )
        .map_err(Error::Message)?;
        Ok((texture, header.tags))
    }
    pub fn upload_size(&self) -> Size {
        if self.has_province() {
            self.size
        } else {
            self.main.size
        }
    }
    /// Peak additional decode storage. Encoded files are already budgeted by
    /// probing; main pixels can remain live while masks/province are decoded.
    pub fn decode_staging_bytes(&self) -> Result<usize> {
        let mut peak = self.main.decode_staging_bytes()?;
        let stored = self.main.size.rgba_bytes().unwrap();
        let output = self.upload_size().rgba_bytes().unwrap();
        if let Some(mask) = &self.mask {
            peak = peak.max(stored.saturating_add(mask.decode_staging_bytes()?));
        }
        if self.main.size != self.upload_size() {
            peak = peak.max(stored.saturating_add(output));
        }
        if self.key == 0x01ffffff {
            peak = peak.max(stored.saturating_add(self.main.size.width as usize * 4));
        }
        if let Some(province) = &self.province {
            peak = peak.max(
                output
                    .saturating_add(province.decode_staging_bytes()?)
                    .saturating_add(output / 4),
            );
        }
        Ok(peak)
    }
    pub fn decode_compact(self, cancelled: &AtomicBool) -> Result<Decoded> {
        self.decode_inner(true, cancelled)
    }
    pub fn decode(self, cancelled: &AtomicBool) -> Result<Decoded> {
        self.decode_inner(false, cancelled)
    }
    fn decode_inner(mut self, compact: bool, cancelled: &AtomicBool) -> Result<Decoded> {
        if compact && !self.has_province() {
            self.size = self.main.size;
        }
        check(cancelled)?;
        if self.province_only {
            let (small, _) = self.main.decode(
                if self.grayscale {
                    codec::Mode::Mask
                } else {
                    codec::Mode::Province
                },
                0,
                &self.budget,
                cancelled,
            )?;
            let (small, size) = if self.main.size != self.main_logical {
                (
                    scale::expand_channels(
                        &small,
                        self.main.size,
                        self.main_logical,
                        1,
                        &self.budget,
                        cancelled,
                    )?,
                    self.main_logical,
                )
            } else {
                (small, self.main.size)
            };
            let plane = transform::tile(small, size, self.size, &self.budget, cancelled)?;
            return Ok(Decoded {
                pixels: Pixels {
                    size: self.size,
                    main: None,
                    province: Some(plane),
                },
                tags: Vec::new(),
            });
        }
        let (mut main, tags) =
            self.main
                .decode(codec::Mode::Main, self.key, &self.budget, cancelled)?;
        let stored = self.main.size;
        drop(self.main);
        transform::color_key(
            main.as_mut_slice(),
            stored,
            self.key,
            &self.budget,
            cancelled,
        )?;
        if let Some(mask) = self.mask {
            let (mask, _) = mask.decode(codec::Mode::Mask, 0, &self.budget, cancelled)?;
            for (row, mask) in main
                .as_mut_slice()
                .chunks_mut(stored.width as usize * 4)
                .zip(mask.as_slice().chunks(stored.width as usize))
            {
                check(cancelled)?;
                for (pixel, alpha) in row.as_chunks_mut::<4>().0.iter_mut().zip(mask) {
                    pixel[3] = *alpha;
                }
            }
        }
        transform::matte(main.as_mut_slice(), self.key, cancelled)?;
        if stored != self.size {
            main = scale::expand(&main, stored, self.size, &self.budget, cancelled)?;
        }
        let province = if let Some(province) = self.province {
            let (small, _) = province.decode(codec::Mode::Province, 0, &self.budget, cancelled)?;
            Some(transform::tile(
                small,
                province.size,
                self.size,
                &self.budget,
                cancelled,
            )?)
        } else {
            None
        };
        Ok(Decoded {
            pixels: Pixels {
                size: self.size,
                main: Some(main),
                province,
            },
            tags,
        })
    }
}
impl Encoded {
    fn decode_staging_bytes(&self) -> Result<usize> {
        let rgba = self.size.rgba_bytes().unwrap();
        let workspace = match self.format {
            codec::Format::Png => {
                let rows = png_image::scratch_bytes(self.size);
                // Adam7 conversion can retain both its packed native plane
                // and the output. Ordinary row conversion needs neither copy.
                rows.saturating_add(if self.bytes.as_slice()[28] != 0 {
                    rgba
                } else {
                    0
                })
            }
            codec::Format::Jpeg => jpeg::scratch_bytes(self.size)?
                .saturating_add(self.bytes.as_slice().len())
                .saturating_add(64 * 1024),
            codec::Format::Tlg => {
                let header = tlg::probe(self.bytes.as_slice())?;
                header.decode_scratch_bytes()?
            }
            codec::Format::Bmp => rgba.saturating_add(16 * 1024),
            codec::Format::Webp => {
                let pixels = (self.size.width as usize).div_ceil(16)
                    * 16
                    * (self.size.height as usize).div_ceil(16)
                    * 16;
                pixels
                    .saturating_mul(8)
                    .saturating_add(rgba)
                    .saturating_add(self.bytes.as_slice().len())
                    .saturating_add(64 * 1024)
            }
            codec::Format::Ktx | codec::Format::PackedBc => 64 * 1024,
        };
        Ok(rgba.saturating_add(workspace))
    }
    fn read(plan: ReadPlan, budget: &Budget, cancelled: &AtomicBool) -> Result<Self> {
        let _profile = krkr_protocol::profile::span_detail("image.prepare", || {
            format!(
                "name={} bytes={}",
                String::from_utf16_lossy(&plan.name),
                plan.bytes
            )
        });
        check(cancelled)?;
        let length = usize::try_from(plan.bytes)
            .map_err(|_| Error::Message("encoded image is too large"))?;
        let mut bytes = Bytes::zeroed(length, budget)?;
        {
            let _profile = krkr_protocol::profile::span("image.read");
            let mut stream = plan.open_interruptible(&|| cancelled.load(Ordering::Relaxed))?;
            for chunk in bytes.as_mut_slice().chunks_mut(64 * 1024) {
                check(cancelled)?;
                stream.read_exact(chunk)?;
            }
        }
        let format = codec::Format::detect(bytes.as_slice(), &plan.name)?;
        if matches!(format, codec::Format::PackedBc) {
            let (blocks, header) = packed_bc::transcode(bytes.as_slice(), budget, cancelled)?;
            return Ok(Self {
                bytes: blocks,
                format,
                size: header.size,
                texture: Some(header),
            });
        }
        let header = if matches!(format, codec::Format::Ktx) {
            Some(compressed::probe(bytes.as_slice())?)
        } else {
            None
        };
        let size = if let Some(header) = &header {
            header.size
        } else {
            codec::probe(bytes.as_slice(), format, budget)?
        };
        Ok(Self {
            bytes,
            format,
            size,
            texture: header,
        })
    }
    fn decode(
        &self,
        mode: codec::Mode,
        key: u32,
        budget: &Budget,
        cancelled: &AtomicBool,
    ) -> Result<(Bytes, Tags)> {
        check(cancelled)?;
        if matches!(self.format, codec::Format::PackedBc) {
            if key & 0xff000000 == 0x03000000 {
                return Err(Error::Message(
                    "compressed texture has no color-key palette",
                ));
            }
            return compressed::decode_prepared(
                self.bytes.as_slice(),
                self.texture.as_ref().unwrap(),
                mode,
                budget,
                cancelled,
            );
        }
        codec::decode(
            self.bytes.as_slice(),
            self.format,
            self.size,
            mode,
            key,
            budget,
            cancelled,
        )
    }
}
fn check(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) {
        Err(Error::Message("image loading cancelled"))
    } else {
        Ok(())
    }
}
fn image_size(width: u32, height: u32) -> Result<Size> {
    let size = Size { width, height };
    if width == 0 || height == 0 || width >= 65536 || height >= 65536 || size.rgba_bytes().is_none()
    {
        return Err(Error::Message("invalid image dimensions"));
    }
    Ok(size)
}
fn reserve(budget: &Budget, bytes: usize) -> Result<Permit> {
    Ok(budget.reserve(bytes)?)
}

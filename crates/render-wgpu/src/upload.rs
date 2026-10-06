use crate::gpu::{Allocation, FORMAT, Gpu, Image, extent};
use krkr_protocol::{
    graphics::{DrawFace, Rect, Size},
    pixels::{Bytes, Pixels},
};
use krkr_render::{Error, Result};
use std::sync::Arc;

impl Gpu {
    pub fn patch_region(&self, image: &mut Image, rectangle: Rect, pixels: &Pixels) -> Result<()> {
        if image.size.rect().intersection(rectangle) != Some(rectangle)
            || pixels.size.width != rectangle.width
            || pixels.size.height != rectangle.height
            || pixels.main.is_none()
            || pixels.province.is_some()
        {
            return Err(Error::Message("invalid main-plane patch region"));
        }
        if rectangle == image.size.rect() {
            return self.patch_pixels(image, pixels);
        }
        // Upload privately before touching the target; existing copy machinery
        // preserves logical/compact images, snapshots and the province plane.
        let mut source = self.reserve_upload(pixels.size, true, false)?;
        self.upload(&mut source, pixels)?;
        self.copy_rect(
            image,
            &source.source(),
            pixels.size.rect(),
            rectangle.left,
            rectangle.top,
            image.size.rect(),
            DrawFace::Alpha,
            false,
        )
    }
    pub fn upload_scaled(
        &self,
        image: &mut Image,
        pixels: &Pixels,
        logical_size: Size,
    ) -> Result<()> {
        if pixels.province.is_some() || pixels.main.is_none() {
            return Err(Error::Message("compact upload requires only a main plane"));
        }
        if image.size == pixels.size && !image.has_province() {
            self.upload(image, pixels)?;
            *image = self.logical_image(image.shared_main(), logical_size)?;
        } else {
            let mut stored = self.reserve_upload(pixels.size, true, false)?;
            self.upload(&mut stored, pixels)?;
            *image = self.logical_image(stored, logical_size)?;
        }
        Ok(())
    }

    pub fn patch_pixels(&self, image: &mut Image, pixels: &Pixels) -> Result<()> {
        if image.size != pixels.size {
            return Err(Error::Message("patch dimensions differ from image"));
        }
        let mut next = self.prepare_upload(
            image,
            pixels.size,
            pixels.main.is_some(),
            pixels.province.is_some(),
        )?;
        self.upload(&mut next, pixels)?;
        if pixels.province.is_none() {
            next.province = image.province.clone();
            next.province_owners = image.province_owners.clone();
        }
        *image = next;
        Ok(())
    }
    /// AssignMainImageWithUpdate replaces only main; ChangeImageSize preserves
    /// the province intersection and fills newly exposed province pixels with 0.
    /// Build privately so upload/admission failure leaves the old image usable.
    pub fn assign_bitmap(&self, source: Option<&Image>, pixels: &Pixels) -> Result<Image> {
        if pixels.main.is_none() || pixels.province.is_some() {
            return Err(Error::Message(
                "Bitmap assignment requires only a main plane",
            ));
        }
        let main = self.allocation(pixels.size, FORMAT, &self.resident)?;
        let mut image = Image::new(Some(main), None, pixels.size);
        let province = source.and_then(|source| source.province.as_ref());
        let resized = if province.is_some() && source.unwrap().size != pixels.size {
            Some(self.allocation(pixels.size, wgpu::TextureFormat::R8Unorm, &self.resident)?)
        } else {
            None
        };
        self.upload(&mut image, pixels)?;
        if let Some(old) = province {
            if let Some(new) = resized {
                let mut encoder = self.device.create_command_encoder(&Default::default());
                self.clear(&mut encoder, &new, [0.0; 4]);
                let overlap = Size {
                    width: source.unwrap().size.width.min(pixels.size.width),
                    height: source.unwrap().size.height.min(pixels.size.height),
                }
                .rect();
                crate::copy::copy(&mut encoder, old, &new, overlap, 0, 0);
                self.submit(encoder, (old.clone(), new.clone()));
                self.check()?;
                image.province = Some(new);
            } else {
                image.province = Some(old.clone());
                image.province_owners = source.unwrap().province_owners.clone();
            }
        }
        Ok(image)
    }
    /// Reserve both destination planes before decoding starts. This image is
    /// private to a pending load and cannot appear in a scene before commit.
    pub fn reserve_upload(&self, size: Size, main: bool, province: bool) -> Result<Image> {
        let main = if main {
            Some(self.allocation(size, FORMAT, &self.resident)?)
        } else {
            None
        };
        let province = if province {
            Some(self.allocation(size, wgpu::TextureFormat::R8Unorm, &self.resident)?)
        } else {
            None
        };
        Ok(Image::new(main, province, size))
    }

    /// A province-only load preserves the source's main pixels and ownership.
    pub fn prepare_upload(
        &self,
        source: &Image,
        size: Size,
        main: bool,
        province: bool,
    ) -> Result<Image> {
        if !main && size != source.size {
            return Err(Error::Message("province image size mismatch"));
        }
        let mut image = self.reserve_upload(size, main, province)?;
        if !main {
            image.main = source.main.clone();
            image.deferred = source.deferred.clone();
            image.main_owners = source.main_owners.clone();
        }
        Ok(image)
    }
    pub fn upload(&self, image: &mut Image, pixels: &Pixels) -> Result<()> {
        if pixels.main.is_some() && image.main.is_none() && image.deferred.is_none() {
            return Err(Error::Message("upload has no reserved main plane"));
        }
        if image.size != pixels.size {
            return Err(Error::Message(
                "upload dimensions differ from the reserved image",
            ));
        }
        if image.province.is_some() != pixels.province.is_some() {
            return Err(Error::Message(
                "upload province plane differs from its reservation",
            ));
        }
        self.check_image_size(pixels.size)?;
        let planes = [
            pixels.main.as_ref().map(|p| (p, 4usize)),
            pixels.province.as_ref().map(|p| (p, 1usize)),
        ];
        let bytes = planes
            .into_iter()
            .flatten()
            .try_fold(0usize, |sum, (data, channels)| {
                let row = pixels.size.width as usize * channels;
                if data.as_slice().len() != row * pixels.size.height as usize {
                    return Err(Error::Message("upload pixel length mismatch"));
                }
                let length = row
                    .div_ceil(256)
                    .checked_mul(256)
                    .and_then(|stride| stride.checked_mul(pixels.size.height as usize))
                    .ok_or(Error::Message("upload size overflow"))?;
                sum.checked_add(length)
                    .ok_or(Error::Message("upload size overflow"))
            })?;
        let permit = self.staging.reserve(bytes)?;
        // Every supplied plane is replaced in full. A shared image needs new
        // storage, but neither its previous pixels nor a deferred recipe need
        // copying/rasterizing. Admit all storage before changing image owners.
        let replace_main = image.deferred.is_some() || Arc::strong_count(&image.main_owners) > 1;
        let replace_province = Arc::strong_count(&image.province_owners) > 1;
        let main = if pixels.main.is_some() {
            Some(if replace_main {
                self.allocation(image.size, FORMAT, &self.resident)?
            } else {
                image.main()?.clone()
            })
        } else {
            None
        };
        let province = if pixels.province.is_some() {
            Some(if replace_province {
                self.allocation(image.size, wgpu::TextureFormat::R8Unorm, &self.resident)?
            } else {
                image.province.as_ref().expect("reserved province").clone()
            })
        } else {
            None
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        for (target, plane) in [main.as_ref(), province.as_ref()].into_iter().zip(planes) {
            if let (Some(target), Some((data, channels))) = (target, plane) {
                self.upload_plane(&mut encoder, target, data, pixels.size, channels)?;
            }
        }
        self.submit(encoder, (main.clone(), province.clone(), permit));
        // CPU pixels can be released now: rows were copied directly into mapped
        // upload buffers, whose GPU references and byte permit survive submit.
        self.check()?;
        if let Some(main) = main {
            main.changed();
            image.main = Some(main);
            image.deferred = None;
            if replace_main {
                image.main_owners = Arc::new(());
            }
        }
        if let Some(province) = province {
            image.province = Some(province);
            if replace_province {
                image.province_owners = Arc::new(());
            }
        }
        Ok(())
    }
    fn upload_plane(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &Allocation,
        data: &Bytes,
        size: Size,
        channels: usize,
    ) -> Result<()> {
        let row = size.width as usize * channels;
        let stride = row.div_ceil(256) * 256;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image upload"),
            size: (stride * size.height as usize) as u64,
            usage: wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: true,
        });
        {
            let mut mapping = buffer
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|e| Error::Backend(e.to_string()))?;
            for (y, source) in data.as_slice().chunks_exact(row).enumerate() {
                mapping
                    .slice(y * stride..y * stride + row)
                    .copy_from_slice(source);
            }
        }
        buffer.unmap();
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: None,
                },
            },
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            extent(size),
        );
        Ok(())
    }
}

//! Logical image dimensions are independent of bounded texture capacity.
use crate::{
    copy::copy,
    gpu::{Allocation, FORMAT, Gpu, Image, rgba},
};
use krkr_protocol::graphics::{Rect, Size};
use krkr_render::Result;
use std::sync::Arc;

impl Image {
    fn reuses_capacity(&self, size: Size) -> bool {
        let fits = |plane: &Arc<Allocation>, owners: &Arc<()>| {
            Arc::strong_count(owners) == 1
                && plane.texture.width() >= size.width
                && plane.texture.height() >= size.height
                // A substantial shrink releases excessive retained capacity.
                && u64::from(size.width) * u64::from(size.height) * 4
                    >= u64::from(plane.texture.width()) * u64::from(plane.texture.height())
        };
        self.main
            .as_ref()
            .is_some_and(|p| fits(p, &self.main_owners))
            && self
                .province
                .as_ref()
                .is_none_or(|p| fits(p, &self.province_owners))
    }

    /// Required new resident bytes, excluding optional growth capacity.
    pub fn resize_write_bytes(&self, size: Size) -> usize {
        if self.size == size
            || self.reuses_capacity(size)
            || !self.has_province() && (self.deferred.is_some() || Self::prefers_deferred(size))
        {
            0
        } else {
            let bytes = size.rgba_bytes().unwrap_or(usize::MAX);
            bytes.saturating_add(if self.has_province() { bytes / 4 } else { 0 })
        }
    }
}

impl Gpu {
    fn resize_capacity(&self, image: &Image, size: Size) -> Size {
        // Reserve growth only for small incremental size changes, not a
        // constructor's one-time jump from its tiny default to a loaded canvas.
        if (size.width <= image.size.width && size.height <= image.size.height)
            || u64::from(size.width) * 2 > u64::from(image.size.width) * 3
            || u64::from(size.height) * 2 > u64::from(image.size.height) * 3
        {
            return size;
        }
        let channels = if image.has_province() { 5 } else { 4 };
        let required = u64::from(size.width) * u64::from(size.height) * channels;
        let preferred = required.saturating_mul(9) / 4;
        if preferred > self.resident.available() as u64 {
            self.trim_scratch();
        }
        // Leave headroom for presentation and subsequent drawing. Optional
        // capacity never raises the shared limit or excludes a required size.
        let available = (self.resident.available() as u64)
            .saturating_sub((self.scratch.limit() / 8) as u64)
            .max(required);
        let factor = (preferred.min(available) as f64 / required as f64).sqrt();
        let max = self.device.limits().max_texture_dimension_2d;
        let capacity = Size {
            width: ((f64::from(size.width) * factor) as u32).clamp(size.width, max),
            height: ((f64::from(size.height) * factor) as u32).clamp(size.height, max),
        };
        if u64::from(capacity.width) * u64::from(capacity.height) * channels > available {
            size
        } else {
            capacity
        }
    }

    /// Preserve overlap and initialize newly exposed pixels. Logical aliases
    /// force replacement; submitted GPU readers remain ordered before writes.
    pub fn resize(&self, image: &mut Image, size: Size, color: u32) -> Result<()> {
        if image.size == size {
            return Ok(());
        }
        self.check_image_size(size)?;
        if self.defer_resize(image, size, color)? {
            return Ok(());
        }
        self.materialize(image)?;
        image.main()?;
        let overlap = Size {
            width: size.width.min(image.size.width),
            height: size.height.min(image.size.height),
        };
        if image.reuses_capacity(size) {
            image.main()?.changed();
            let exposed = [
                Rect {
                    left: overlap.width as i32,
                    top: 0,
                    width: size.width - overlap.width,
                    height: overlap.height,
                },
                Rect {
                    left: 0,
                    top: overlap.height as i32,
                    width: size.width,
                    height: size.height - overlap.height,
                },
            ];
            if exposed.iter().any(|r| r.width != 0 && r.height != 0) {
                // Prepare both colors before either plane can be modified.
                let main_color = self.fill_color(rgba(color))?;
                let province_color = image
                    .province
                    .as_ref()
                    .map(|_| self.fill_color([0.0; 4]))
                    .transpose()?;
                let mut encoder = self.device.create_command_encoder(&Default::default());
                for (plane, color, pipeline) in std::iter::once((image.main()?, &main_color, 0))
                    .chain(
                        image
                            .province
                            .as_ref()
                            .zip(province_color.as_ref())
                            .map(|(p, c)| (p, c, 3)),
                    )
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("resize exposed pixels"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &plane.view,
                            depth_slice: None,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        })],
                        ..Default::default()
                    });
                    pass.set_pipeline(&self.fill_pipelines[pipeline]);
                    pass.set_bind_group(0, &color.bind, &[0]);
                    for rect in exposed.iter().filter(|r| r.width != 0 && r.height != 0) {
                        pass.set_scissor_rect(
                            rect.left as u32,
                            rect.top as u32,
                            rect.width,
                            rect.height,
                        );
                        pass.draw(0..3, 0..1);
                    }
                }
                self.submit(encoder, (image.source(), main_color, province_color));
                self.check()?;
            }
            image.size = size;
            return Ok(());
        }
        let capacity = self.resize_capacity(image, size);
        let main = self.allocation(capacity, FORMAT, &self.resident)?;
        let province = image
            .province
            .as_ref()
            .map(|_| self.allocation(capacity, wgpu::TextureFormat::R8Unorm, &self.resident))
            .transpose()?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.clear(&mut encoder, &main, rgba(color));
        copy(&mut encoder, image.main()?, &main, overlap.rect(), 0, 0);
        if let (Some(old), Some(new)) = (&image.province, &province) {
            self.clear(&mut encoder, new, [0.0; 4]);
            copy(&mut encoder, old, new, overlap.rect(), 0, 0);
        }
        self.submit(encoder, (image.source(), main.clone(), province.clone()));
        self.check()?;
        *image = Image::new(Some(main), province, size);
        Ok(())
    }
}

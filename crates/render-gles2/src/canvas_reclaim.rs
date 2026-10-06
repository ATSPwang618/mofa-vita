//! Last-resort lossless reclamation of uniform borders whose provenance was
//! lost by shaders. Normal rendering never reads pixels for this purpose.
use crate::{Gpu, Image, Result};
use glow::HasContext;
use krkr_protocol::{graphics::Rect, pixels::Bytes};
use std::{collections::HashSet, rc::Rc};

impl Gpu {
    pub fn reclaim_canvas_borders<'a>(
        &self,
        images: impl Iterator<Item = &'a mut Image>,
        headroom: usize,
    ) -> Result<()> {
        if !self.device.streamed_uploads() || self.resident.available() >= headroom {
            return Ok(());
        }
        let _profile = krkr_protocol::profile::span("gpu.reclaim_borders");
        let mut images: Vec<_> = images.collect();
        let mut seen = HashSet::new();
        let mut textures = Vec::new();
        for image in &images {
            if !image.canvas {
                continue;
            }
            if let Some(plane) = &image.main {
                for tile in &plane.tiles {
                    let texture = &tile.texture;
                    if tile.renderable()
                        && texture.allocation_bytes() >= 256 * 1024
                        && texture.border_scan_generation.get() != Some(texture.generation.get())
                        && seen.insert(Rc::as_ptr(texture) as usize)
                    {
                        textures.push((texture.allocation_bytes(), Rc::downgrade(texture)));
                    }
                }
            }
        }
        textures.sort_by_key(|(bytes, _)| std::cmp::Reverse(*bytes));
        for (_, weak) in textures {
            if self.resident.available() >= headroom {
                break;
            }
            let Some(texture) = weak.upgrade() else {
                continue;
            };
            let size = texture.size;
            let bytes = size.rgba_bytes().unwrap();
            if bytes > self.staging.available() {
                continue;
            }
            let mut pixels = Bytes::zeroed(bytes, &self.staging)?;
            unsafe {
                self.device
                    .gl
                    .bind_framebuffer(glow::FRAMEBUFFER, Some(texture.read_framebuffer()?));
                self.device.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                self.device.gl.read_pixels(
                    0,
                    0,
                    size.width as i32,
                    size.height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(pixels.as_mut_slice())),
                );
            }
            self.device.check()?;
            texture
                .border_scan_generation
                .set(Some(texture.generation.get()));
            let data = pixels.as_slice();
            let first: [u8; 4] = data[..4].try_into().unwrap();
            let mut left = size.width;
            let mut top = size.height;
            let mut right = 0;
            let mut bottom = 0;
            for (y, row) in data.chunks_exact(size.width as usize * 4).enumerate() {
                // Compare all channels: invisible RGB is script-observable.
                // Row extents avoid an integer divide for every pixel on ARM.
                if let Some(x) = row.as_chunks::<4>().0.iter().position(|p| *p != first) {
                    let end = row
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .rposition(|p| *p != first)
                        .unwrap();
                    left = left.min(x as u32);
                    top = top.min(y as u32);
                    right = right.max(end as u32 + 1);
                    bottom = bottom.max(y as u32 + 1);
                }
            }
            let damage = (right != 0).then(|| Rect {
                left: left as i32,
                top: top as i32,
                width: right - left,
                height: bottom - top,
            });
            let color = u32::from_le_bytes([first[2], first[1], first[0], first[3]]);
            texture.point_background(color, damage);
            drop(pixels);
            drop(texture);
            self.compact_canvases(images.iter_mut().map(|i| &mut **i), headroom)?;
        }
        Ok(())
    }
}

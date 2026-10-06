//! R8 glyph atlases and ordered text batches. Queued runs carry glyph masks,
//! so atlas locations are resolved at execution, never retained across eviction.
use crate::{
    blend::{PARAMETER_BYTES, Parameters},
    copy::copy,
    gpu::{Allocation, FORMAT, Gpu, Image, extent},
};
use etagere::{AllocId, AtlasAllocator, size2};
use krkr_protocol::{
    graphics::{Blend, BlendOptions, DrawFace, Rect, Size},
    text::{Glyph, Run, Style},
};
use krkr_render::{Error, Result};
use std::{collections::HashMap, sync::Arc};
use wgpu::util::DeviceExt;

struct Page {
    id: u64,
    image: Arc<Allocation>,
    allocator: AtlasAllocator,
    touched: u64,
}
#[derive(Clone)]
struct Slot {
    page: u64,
    image: Arc<Allocation>,
    rect: Rect,
    allocation: AllocId,
}
#[derive(Default)]
pub(crate) struct Atlas {
    pages: Vec<Page>,
    entries: HashMap<u64, (u64, Rect, AllocId)>,
    clock: u64,
    uploaded: u64,
}
struct Draw {
    slot: Slot,
    parameters: Parameters,
    clip: Rect,
}
struct Batch {
    range: std::ops::Range<usize>,
    bounds: Rect,
}
impl Atlas {
    fn resolve(
        &mut self,
        gpu: &Gpu,
        glyph: &Arc<Glyph>,
        pending: &mut Vec<(Arc<Glyph>, Slot)>,
    ) -> Result<Slot> {
        self.clock += 1;
        if let Some(&(id, rect, allocation)) = self.entries.get(&glyph.id) {
            let page = self
                .pages
                .iter_mut()
                .find(|p| p.id == id)
                .expect("cached atlas page");
            page.touched = self.clock;
            return Ok(Slot {
                page: id,
                image: page.image.clone(),
                rect,
                allocation,
            });
        }
        if let Some((_, slot)) = pending.iter().find(|(g, _)| g.id == glyph.id) {
            return Ok(slot.clone());
        }
        let size = glyph.size;
        if glyph.mask.as_slice().len() != size.width as usize * size.height as usize {
            return Err(Error::Message("invalid glyph mask length"));
        }
        for page in &mut self.pages {
            if let Some(a) = page
                .allocator
                .allocate(size2(size.width as i32, size.height as i32))
            {
                let slot = Slot {
                    page: page.id,
                    image: page.image.clone(),
                    rect: Rect {
                        left: a.rectangle.min.x,
                        top: a.rectangle.min.y,
                        width: size.width,
                        height: size.height,
                    },
                    allocation: a.id,
                };
                page.touched = self.clock;
                pending.push((glyph.clone(), slot.clone()));
                return Ok(slot);
            }
        }
        if self.pages.len() >= 8 {
            // In-flight and currently prepared draws pin pages. Eviction cannot
            // recycle their texels before their own GPU submission completes.
            let index = self
                .pages
                .iter()
                .enumerate()
                .filter(|(_, p)| Arc::strong_count(&p.image) == 1)
                .min_by_key(|(_, p)| p.touched)
                .map(|(i, _)| i)
                .ok_or(Error::Message("glyph atlas pages are all in use"))?;
            let page = self.pages.remove(index);
            self.entries.retain(|_, (id, _, _)| *id != page.id);
        }
        let page_size = Size {
            width: 512.max(size.width.next_power_of_two()),
            height: 512.max(size.height.next_power_of_two()),
        };
        let image = gpu.allocation(page_size, wgpu::TextureFormat::R8Unorm, &gpu.resident)?;
        let mut allocator =
            AtlasAllocator::new(size2(page_size.width as i32, page_size.height as i32));
        let a = allocator
            .allocate(size2(size.width as i32, size.height as i32))
            .expect("glyph fits new atlas page");
        let slot = Slot {
            page: self.clock,
            image: image.clone(),
            rect: Rect {
                left: a.rectangle.min.x,
                top: a.rectangle.min.y,
                width: size.width,
                height: size.height,
            },
            allocation: a.id,
        };
        self.pages.push(Page {
            id: self.clock,
            image,
            allocator,
            touched: self.clock,
        });
        pending.push((glyph.clone(), slot.clone()));
        Ok(slot)
    }
    fn rollback(&mut self, pending: &[(Arc<Glyph>, Slot)]) {
        for (_, slot) in pending {
            self.pages
                .iter_mut()
                .find(|p| p.id == slot.page)
                .expect("pinned atlas page")
                .allocator
                .deallocate(slot.allocation);
        }
    }
}
fn union(a: Rect, b: Rect) -> Rect {
    let left = a.left.min(b.left);
    let top = a.top.min(b.top);
    Rect {
        left,
        top,
        width: (a.left + a.width as i32)
            .max(b.left + b.width as i32)
            .saturating_sub(left) as u32,
        height: (a.top + a.height as i32)
            .max(b.top + b.height as i32)
            .saturating_sub(top) as u32,
    }
}
impl Gpu {
    pub fn uploaded_glyphs(&self) -> u64 {
        self.text.lock().unwrap().uploaded
    }
    pub fn draw_text(&self, image: &mut Image, run: &Run, style: Style, clip: Rect) -> Result<()> {
        self.materialize(image)?;
        image.main()?;
        let Some(clip) = clip.intersection(image.size.rect()) else {
            return Ok(());
        };
        if style.opacity == 0 || run.glyphs.is_empty() {
            return Ok(());
        }
        if matches!(style.face, DrawFace::Mask | DrawFace::Province)
            || (style.opacity < 0 && style.face != DrawFace::Alpha)
        {
            return Err(Error::Message("invalid text draw face or opacity"));
        }
        let mut atlas = self.text.lock().unwrap();
        let mut pending = Vec::new();
        let mut submitted = false;
        let result = (|| {
            let stride = self.device.limits().min_uniform_buffer_offset_alignment as usize;
            let metadata = run.glyphs.len()
                * (std::mem::size_of::<Draw>()
                    + std::mem::size_of::<Batch>()
                    + std::mem::size_of::<(Arc<Glyph>, Slot)>()
                    + stride * 2);
            let parameter_permit = self.staging.reserve(metadata)?;
            let mut draws = Vec::<Draw>::with_capacity(run.glyphs.len());
            let mut batches = Vec::<Batch>::with_capacity(run.glyphs.len());
            pending.reserve_exact(run.glyphs.len());
            for placed in &run.glyphs {
                let g = &placed.glyph;
                let x = placed.x.saturating_add(g.origin[0]);
                let y = placed.y.saturating_add(g.origin[1]);
                let Some(rect) = (Rect {
                    left: x,
                    top: y,
                    ..g.size.rect()
                })
                .intersection(clip) else {
                    continue;
                };
                let slot = atlas.resolve(self, g, &mut pending)?;
                let mut parameters = Parameters::new(
                    (slot.rect.left - x, slot.rect.top - y),
                    rect,
                    BlendOptions {
                        mode: Blend::Alpha,
                        face: style.face,
                        opacity: style.opacity.unsigned_abs().min(255) as u8,
                        hold_alpha: style.hold_alpha,
                    },
                );
                parameters.0[6] = style.opacity.into();
                parameters.0[7] |= 128 | if g.levels == 65 { 256 } else { 0 };
                parameters.0[8..12].copy_from_slice(&[
                    (placed.color >> 16 & 255) as i32,
                    (placed.color >> 8 & 255) as i32,
                    (placed.color & 255) as i32,
                    0,
                ]);
                if let Some(batch) = batches.last_mut()
                    && batch.bounds.intersection(rect).is_none()
                {
                    batch.range.end += 1;
                    batch.bounds = union(batch.bounds, rect);
                } else {
                    batches.push(Batch {
                        range: draws.len()..draws.len() + 1,
                        bounds: rect,
                    });
                }
                draws.push(Draw {
                    slot,
                    parameters,
                    clip: rect,
                });
            }
            if draws.is_empty() {
                return Ok(());
            }
            let scratch = Size {
                width: batches.iter().map(|b| b.bounds.width).max().unwrap(),
                height: batches.iter().map(|b| b.bounds.height).max().unwrap(),
            };
            let backdrop = self.temporary(scratch, FORMAT)?;
            let upload_bytes: usize = pending
                .iter()
                .map(|(g, _)| (g.size.width as usize).div_ceil(256) * 256 * g.size.height as usize)
                .sum();
            let upload_permit = self.staging.reserve(upload_bytes)?;
            let mut data = vec![0u8; draws.len() * stride];
            for batch in &batches {
                for index in batch.range.clone() {
                    draws[index].parameters.0[2] = batch.bounds.left;
                    draws[index].parameters.0[3] = batch.bounds.top;
                    data[index * stride..index * stride + PARAMETER_BYTES]
                        .copy_from_slice(bytemuck::cast_slice(&draws[index].parameters.0));
                }
            }
            let buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("text parameters"),
                    contents: &data,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
                });
            let mixer = self.mixer()?;
            let mut bindings = HashMap::new();
            for draw in &draws {
                bindings.entry(draw.slot.page).or_insert_with(|| {
                    mixer.bind(self, &draw.slot.image, Some(&backdrop), &buffer, None)
                });
            }
            self.independent(image, true, false)?;
            let target = image.main()?;
            let mut encoder = self.device.create_command_encoder(&Default::default());
            for (glyph, slot) in &pending {
                let row = glyph.size.width as usize;
                let stride = row.div_ceil(256) * 256;
                let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("glyph upload"),
                    size: (stride * glyph.size.height as usize) as u64,
                    usage: wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: true,
                });
                {
                    let mut data = buffer
                        .slice(..)
                        .get_mapped_range_mut()
                        .map_err(|e| Error::Backend(e.to_string()))?;
                    for (y, row) in glyph.mask.as_slice().chunks_exact(row).enumerate() {
                        data.slice(y * stride..y * stride + row.len())
                            .copy_from_slice(row);
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
                        texture: &slot.image.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: slot.rect.left as u32,
                            y: slot.rect.top as u32,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    extent(glyph.size),
                );
            }
            for batch in &batches {
                copy(&mut encoder, target, &backdrop, batch.bounds, 0, 0);
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("text batch"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &target.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    ..Default::default()
                });
                pass.set_pipeline(
                    mixer.pipeline(style.face == DrawFace::Opaque && style.hold_alpha),
                );
                for index in batch.range.clone() {
                    let draw = &draws[index];
                    let r = draw.clip;
                    pass.set_bind_group(0, &bindings[&draw.slot.page], &[(index * stride) as u32]);
                    pass.set_scissor_rect(r.left as u32, r.top as u32, r.width, r.height);
                    pass.draw(0..3, 0..1);
                }
            }
            for (glyph, slot) in &pending {
                atlas
                    .entries
                    .insert(glyph.id, (slot.page, slot.rect, slot.allocation));
            }
            atlas.uploaded += pending.len() as u64;
            self.submit(
                encoder,
                (
                    target.clone(),
                    backdrop,
                    draws,
                    parameter_permit,
                    upload_permit,
                ),
            );
            submitted = true;
            self.check()
        })();
        if result.is_err() && !submitted {
            atlas.rollback(&pending);
        }
        result
    }
}

use crate::gpu::{Allocation, Gpu, Image, rgba};
use krkr_protocol::graphics::{DrawFace, Fill, Rect};
use krkr_render::{Error, Result};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};
use wgpu::util::DeviceExt;

pub(crate) struct Color {
    pub(crate) bind: wgpu::BindGroup,
    _permit: krkr_render::budget::Permit,
}
#[derive(Default)]
pub(crate) struct Colors {
    entries: HashMap<[u32; 4], Arc<Color>>,
    order: VecDeque<[u32; 4]>,
}

struct Draw {
    target: Arc<Allocation>,
    color: Arc<Color>,
    pipeline: usize,
    rect: Rect,
}

#[derive(Default)]
pub(crate) struct Batch {
    draws: Vec<Draw>,
}

impl Gpu {
    /// Submit pending rectangles in call order, before other GPU operations or
    /// publishing a scene. Logical image snapshots still trigger COW as usual.
    pub fn flush_fills(&self) {
        let draws = {
            let mut batch = self.fill_batch.lock().unwrap();
            if batch.draws.is_empty() {
                return;
            }
            std::mem::take(&mut batch.draws)
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut start = 0;
        while start < draws.len() {
            let target = &draws[start].target;
            let end = draws[start..]
                .iter()
                .position(|draw| !Arc::ptr_eq(&draw.target, target))
                .map_or(draws.len(), |n| start + n);
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("pending fill rectangles"),
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
                for draw in &draws[start..end] {
                    pass.set_pipeline(&self.fill_pipelines[draw.pipeline]);
                    pass.set_bind_group(0, &draw.color.bind, &[0]);
                    pass.set_scissor_rect(
                        draw.rect.left as u32,
                        draw.rect.top as u32,
                        draw.rect.width,
                        draw.rect.height,
                    );
                    pass.draw(0..3, 0..1);
                }
            }
            start = end;
        }
        self.submit_direct(encoder, draws);
    }

    pub(crate) fn fill_color(&self, color: [f32; 4]) -> Result<Arc<Color>> {
        let key = color.map(f32::to_bits);
        let mut colors = self.fill_colors.lock().unwrap();
        if let Some(color) = colors.entries.get(&key) {
            return Ok(color.clone());
        }
        // A bounded cache for the repeated colors of script-drawn controls.
        // Entries and in-flight draws keep the same staging budget permit.
        if colors.entries.len() == 64 {
            let oldest = colors.order.pop_front().expect("cached color");
            colors.entries.remove(&oldest);
        }
        let permit = self
            .staging
            .reserve(self.device.limits().min_uniform_buffer_offset_alignment as usize * 2)?;
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fill color"),
                contents: bytemuck::cast_slice(&color),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fill color"),
            layout: &self.fill_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: buffer.as_entire_binding(),
            }],
        });
        let color = Arc::new(Color {
            bind,
            _permit: permit,
        });
        colors.order.push_back(key);
        colors.entries.insert(key, color.clone());
        Ok(color)
    }
    fn fill_main(&self, image: &mut Image, fill: &Fill) -> Result<()> {
        let Some(rect) = image.size.rect().intersection(fill.rectangle) else {
            return Ok(());
        };
        let (pipeline, color) = match fill.face {
            DrawFace::Mask => (2, [0.0, 0.0, 0.0, (fill.color & 255) as f32 / 255.0]),
            DrawFace::Opaque if fill.hold_alpha => (1, rgba(fill.color)),
            _ => (0, rgba(fill.color)),
        };
        if pipeline == 0
            && rect == image.size.rect()
            && (image.deferred.is_some() || Image::prefers_deferred(image.size))
        {
            return self.replace_solid(image, fill.color);
        }
        if self.edit_generated(image, rect, |tile, rectangle, _| {
            self.fill_main(tile, &Fill { rectangle, ..*fill })
        })? {
            return Ok(());
        }
        let color = self.fill_color(color)?;
        if pipeline == 0 && rect == image.size.rect() {
            self.independ_image(image, false, false)?;
        } else {
            self.independent(image, true, false)?;
        }
        let full = {
            let mut batch = self.fill_batch.lock().unwrap();
            batch.draws.push(Draw {
                target: image.main()?.clone(),
                color,
                pipeline,
                rect,
            });
            // Bound both deferred work and retained resources even when a
            // script draws without yielding a scene. Adjacent rows share a pass.
            batch.draws.len() >= 128
        };
        if full {
            self.flush_fills();
        }
        self.check()
    }
    pub fn fill(&self, image: &mut Image, fills: &[Fill]) -> Result<()> {
        if fills.is_empty() {
            return Ok(());
        }
        if let [fill] = fills
            && fill.face != DrawFace::Province
        {
            return self.fill_main(image, fill);
        }
        self.fill_many(image, fills)
    }
    fn fill_many(&self, image: &mut Image, fills: &[Fill]) -> Result<()> {
        self.independent(
            image,
            fills.iter().any(|f| {
                f.face != DrawFace::Province
                    && image.size.rect().intersection(f.rectangle).is_some()
            }),
            fills.iter().any(|f| {
                f.face == DrawFace::Province
                    && image.size.rect().intersection(f.rectangle).is_some()
            }),
        )?;
        let stride = self.device.limits().min_uniform_buffer_offset_alignment as usize;
        let bytes = stride
            .checked_mul(fills.len())
            .filter(|&n| n <= u32::MAX as usize)
            .ok_or(Error::Message("fill batch is too large"))?;
        let permit = self.staging.reserve(
            bytes
                .checked_mul(2)
                .ok_or(Error::Message("fill batch overflow"))?,
        )?;
        let mut data = vec![0u8; bytes];
        let mut prepared = Vec::with_capacity(fills.len());
        let mut province = image.province.clone();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        for (index, fill) in fills.iter().enumerate() {
            let Some(rect) = image.size.rect().intersection(fill.rectangle) else {
                continue;
            };
            let (target, pipeline, color) = match fill.face {
                DrawFace::Province => {
                    if fill.color & 255 == 0 {
                        if province.is_none() {
                            continue;
                        }
                        if rect == image.size.rect() {
                            province = None;
                            continue;
                        }
                    }
                    if province.is_none() {
                        let plane = self.allocation(
                            image.size,
                            wgpu::TextureFormat::R8Unorm,
                            &self.resident,
                        )?;
                        self.clear(&mut encoder, &plane, [0.0; 4]);
                        province = Some(plane);
                    }
                    (
                        province.as_ref().expect("province").clone(),
                        3,
                        [(fill.color & 255) as f32 / 255.0, 0.0, 0.0, 0.0],
                    )
                }
                DrawFace::Mask => (
                    image.main()?.clone(),
                    2,
                    [0.0, 0.0, 0.0, (fill.color & 255) as f32 / 255.0],
                ),
                DrawFace::Opaque if fill.hold_alpha => (image.main()?.clone(), 1, rgba(fill.color)),
                _ => (image.main()?.clone(), 0, rgba(fill.color)),
            };
            let offset = index * stride;
            data[offset..offset + 16].copy_from_slice(bytemuck::cast_slice(&color));
            prepared.push((target, pipeline, rect, offset as u32));
        }
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("fill batch"),
                contents: &data,
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("fill batch"),
            layout: &self.fill_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &buffer,
                    offset: 0,
                    size: wgpu::BufferSize::new(16),
                }),
            }],
        });
        // Preserve operation order, but adjacent draws to the same plane share
        // a render pass and every draw in the batch shares one uniform buffer.
        let mut start = 0;
        while start < prepared.len() {
            let target = &prepared[start].0;
            let end = prepared[start..]
                .iter()
                .position(|p| !std::sync::Arc::ptr_eq(&p.0, target))
                .map_or(prepared.len(), |n| start + n);
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("fill batch"),
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
                for (_, pipeline, rect, offset) in &prepared[start..end] {
                    pass.set_pipeline(&self.fill_pipelines[*pipeline]);
                    pass.set_bind_group(0, &bind, &[*offset]);
                    pass.set_scissor_rect(
                        rect.left as u32,
                        rect.top as u32,
                        rect.width,
                        rect.height,
                    );
                    pass.draw(0..3, 0..1);
                }
            }
            start = end;
        }
        self.submit(encoder, (prepared, permit));
        self.check()?;
        let replaced = match (&image.province, &province) {
            (Some(a), Some(b)) => !std::sync::Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        if replaced {
            image.province_owners = std::sync::Arc::new(());
        }
        image.province = province;
        Ok(())
    }
}

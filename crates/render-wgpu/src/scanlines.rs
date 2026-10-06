use crate::{
    copy::ImageSource,
    gpu::{FORMAT, Gpu, Image},
};
use krkr_protocol::{
    graphics::{Rect, Size},
    scanlines::Scanlines,
};
use krkr_render::{Error, Result};
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub(crate) struct Renderer {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}
impl Renderer {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scanline copies"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(32),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scanline copies"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("scanlines.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scanline copies"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { layout, pipeline }
    }
}
impl Gpu {
    pub fn copy_scanlines(
        &self,
        image: &mut Image,
        source: &ImageSource,
        rows: &Scanlines,
    ) -> Result<()> {
        let Some(rect) = rows.rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        let resolved = self.materialized_source(source)?;
        let source = resolved.as_ref().unwrap_or(source);
        self.materialize(image)?;
        let plane = source
            .main
            .as_ref()
            .ok_or(Error::Message("source has no main plane"))?;
        let bytes = rows
            .words
            .len()
            .checked_mul(4)
            .ok_or(Error::Message("scanline data size overflow"))?;
        if rows.words.len() < 8
            || !rows.words.len().is_multiple_of(8)
            || rows.words[1] as usize != rows.words.len() / 8 - 1
            || rows.words[3] != source.size.width as i32
            || rows.words[4] != source.size.height as i32
            || bytes > self.device.limits().max_storage_buffer_binding_size as usize
        {
            return Err(Error::Message("invalid scanline copy data"));
        }
        // Only aliasing needs a snapshot. Bound it to sampled rows, including a
        // right-neighbour that can cross into the following native scanline.
        let alias = Arc::strong_count(&image.main_owners) == 1 && Arc::ptr_eq(plane, image.main()?);
        let footprint = footprint(rows, source.size);
        let snapshot = alias
            .then(|| {
                self.temporary(
                    Size {
                        width: footprint.width,
                        height: footprint.height,
                    },
                    FORMAT,
                )
            })
            .transpose()?;
        let permit = self.staging.reserve(bytes)?;
        let previous = (rows.words[2] >= 2)
            .then(|| {
                self.temporary(
                    Size {
                        width: rect.width,
                        height: rect.height,
                    },
                    FORMAT,
                )
            })
            .transpose()?;
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let renderer = self.scanlines.get_or_init(|| Renderer::new(&self.device));
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("scanline mappings"),
                contents: bytemuck::cast_slice(&rows.words),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(previous) = &previous {
            crate::copy::copy(&mut encoder, &target, previous, rect, 0, 0);
            self.queue
                .write_buffer(&buffer, 20, bytemuck::cast_slice(&[rect.top]));
        }
        let input = if let Some(snapshot) = &snapshot {
            crate::copy::copy(&mut encoder, plane, snapshot, footprint, 0, 0);
            self.queue.write_buffer(
                &buffer,
                24,
                bytemuck::cast_slice(&[footprint.left, footprint.top]),
            );
            snapshot
        } else {
            plane
        };
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scanline copies"),
            layout: &renderer.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(
                        &previous.as_ref().unwrap_or(input).view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scanline copies"),
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
            pass.set_pipeline(&renderer.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.set_scissor_rect(rect.left as u32, rect.top as u32, rect.width, rect.height);
            pass.draw(0..3, 0..1);
        }
        self.submit(
            encoder,
            (target, plane.clone(), snapshot, previous, buffer, permit),
        );
        self.check()
    }
}
fn footprint(rows: &Scanlines, size: Size) -> Rect {
    let width = i64::from(size.width);
    let length = width * i64::from(size.height);
    let mut first = length - 1;
    let mut last = 0;
    for row in rows.words[8..]
        .as_chunks::<8>()
        .0
        .iter()
        .filter(|r| r[1] > 0)
    {
        let begin = i64::from(row[3]) * width + i64::from(row[2]);
        first = first.min(begin.clamp(0, length - 1));
        last = last.max(
            (begin + i64::from(row[1]) - 1 + i64::from(rows.words[2] != 0)).clamp(0, length - 1),
        );
    }
    let top = first.min(last) / width;
    let bottom = last.max(first) / width;
    Rect {
        left: 0,
        top: top as i32,
        width: size.width,
        height: (bottom - top + 1) as u32,
    }
}

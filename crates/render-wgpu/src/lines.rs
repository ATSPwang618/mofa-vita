use crate::gpu::{FORMAT, Gpu, Image};
use krkr_protocol::{
    graphics::{Rect, Size},
    lines::Lines,
};
use krkr_render::{Error, Result};
use wgpu::util::DeviceExt;

pub(crate) struct Renderer {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}
impl Renderer {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ordered lines"),
            entries: &[
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
            label: Some("ordered lines"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("lines.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ordered lines"),
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
    pub(crate) fn draw_lines(
        &self,
        image: &mut Image,
        rectangle: Rect,
        lines: &Lines,
    ) -> Result<()> {
        let Some(rect) = rectangle
            .intersection(lines.rectangle)
            .and_then(|r| r.intersection(image.size.rect()))
        else {
            return Ok(());
        };
        let bytes = lines
            .words
            .len()
            .checked_mul(4)
            .ok_or(Error::Message("line data size overflow"))?;
        if bytes > self.device.limits().max_storage_buffer_binding_size as usize {
            return Err(Error::Message("line data exceeds backend buffer limit"));
        }
        let permit = self.staging.reserve(bytes)?;
        let snapshot = self.temporary(
            Size {
                width: rect.width,
                height: rect.height,
            },
            FORMAT,
        )?;
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let renderer = self.lines.get_or_init(|| Renderer::new(&self.device));
        // Upload once without another CPU copy. A tiny header update locates
        // the snapshot when the backend clipped more than the script did.
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("line tile index"),
                contents: bytemuck::cast_slice(&lines.words),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            });
        self.queue.write_buffer(
            &buffer,
            24,
            bytemuck::cast_slice(&[rect.left as u32, rect.top as u32]),
        );
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ordered lines"),
            layout: &renderer.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&snapshot.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        crate::copy::copy(&mut encoder, &target, &snapshot, rect, 0, 0);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ordered lines"),
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
        self.submit(encoder, (target, snapshot, buffer, permit));
        self.check()
    }
}

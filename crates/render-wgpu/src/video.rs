//! Decoder frames are already CPU-owned; upload once, combine a side-by-side
//! mask on the GPU, and write the existing destination without resizing it.
use crate::gpu::{FORMAT, Gpu, Image};
use krkr_protocol::{graphics::Size, pixels::Pixels};
use krkr_render::{Error, Result};
use wgpu::util::DeviceExt;

pub(crate) struct Copier {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}
impl Copier {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("video alpha"),
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
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("video.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("video alpha"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("video alpha"),
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
    pub fn copy_pixels(
        &self,
        image: &mut Image,
        pixels: &Pixels,
        split_alpha: bool,
        limit: Size,
    ) -> Result<()> {
        self.materialize(image)?;
        image.main()?;
        if pixels.main.is_none() || pixels.province.is_some() {
            return Err(Error::Message("movie frame must contain only RGBA"));
        }
        let width = if split_alpha {
            pixels.size.width / 2
        } else {
            pixels.size.width
        };
        if width == 0 {
            return Err(Error::Message("movie frame width is empty"));
        }
        let size = Size {
            width: width.min(image.size.width).min(limit.width),
            height: pixels.size.height.min(image.size.height).min(limit.height),
        };
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let allocation = self.temporary(pixels.size, FORMAT)?;
        let mut source = Image::new(Some(allocation.clone()), None, pixels.size);
        let permit = self.staging.reserve(16)?;
        self.independent(image, true, false)?;
        self.upload(&mut source, pixels)?;
        let target = image.main()?.clone();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut parameters = None;
        if split_alpha {
            let copier = self.video_copier.get_or_init(|| Copier::new(&self.device));
            let buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("movie mask offset"),
                    contents: bytemuck::cast_slice(&[width, 0, 0, 0]),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("video alpha"),
                layout: &copier.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&allocation.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buffer.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("video alpha"),
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
            pass.set_pipeline(&copier.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.set_scissor_rect(0, 0, size.width, size.height);
            pass.draw(0..3, 0..1);
            drop(pass);
            parameters = Some(buffer);
        } else {
            crate::copy::copy(&mut encoder, &allocation, &target, size.rect(), 0, 0);
        }
        self.submit(encoder, (allocation, target, parameters, permit));
        self.check()
    }
}

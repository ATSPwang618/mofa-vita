use crate::{
    copy::ImageSource,
    gpu::{FORMAT, Gpu, Image},
};
use krkr_protocol::warp::Warp;
use krkr_render::{Error, Result};
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub(crate) struct Renderer {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}
impl Renderer {
    fn new(device: &wgpu::Device) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("integer image warps"),
            entries: &[
                texture(0),
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(64),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("integer image warps"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("warp.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("integer image warps"),
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
    pub fn warp(&self, image: &mut Image, source: &ImageSource, effect: &Warp) -> Result<()> {
        let rectangle = match effect {
            Warp::Stretch { destination, .. } => *destination,
            _ => image.size.rect(),
        };
        let Some(rect) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        let resolved = self.materialized_source(source)?;
        let source = resolved.as_ref().unwrap_or(source);
        self.materialize(image)?;
        let plane = source
            .main
            .as_ref()
            .ok_or(Error::Message("source has no main plane"))?;
        let mut data = vec![0u32; 16];
        data[1..5].copy_from_slice(&[
            source.size.width,
            source.size.height,
            image.size.width,
            image.size.height,
        ]);
        match effect {
            Warp::Lens {
                radius,
                power,
                table,
            } => {
                if table.as_slice().len() != 8192 * 4 {
                    return Err(Error::Message("invalid lens table"));
                }
                data[5] = radius.to_bits();
                data[6] = *power;
                data.extend(
                    table
                        .as_slice()
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| u32::from_le_bytes(*b)),
                );
            }
            Warp::Vortex { radians } => {
                data[0] = 1;
                data[5] = radians.to_bits();
            }
            Warp::Stretch {
                source,
                destination,
                opacity,
            } => {
                data[0] = 2;
                data[5..14].copy_from_slice(&[
                    source.left as u32,
                    source.top as u32,
                    source.width,
                    source.height,
                    destination.left as u32,
                    destination.top as u32,
                    destination.width,
                    destination.height,
                    *opacity as u32,
                ]);
            }
        }
        let permit = self.staging.reserve(data.len() * 4)?;
        let alias = Arc::strong_count(&image.main_owners) == 1 && Arc::ptr_eq(plane, image.main()?);
        let snapshot = alias
            .then(|| self.temporary(source.size, FORMAT))
            .transpose()?;
        let reads_target = matches!(effect,Warp::Stretch{opacity,..} if *opacity<255);
        let previous = reads_target
            .then(|| {
                self.temporary(
                    krkr_protocol::graphics::Size {
                        width: rect.width,
                        height: rect.height,
                    },
                    FORMAT,
                )
            })
            .transpose()?;
        data[14] = rect.left as u32;
        data[15] = rect.top as u32;
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let input = if let Some(snapshot) = &snapshot {
            crate::copy::copy(&mut encoder, plane, snapshot, source.size.rect(), 0, 0);
            snapshot
        } else {
            plane
        };
        if let Some(previous) = &previous {
            crate::copy::copy(&mut encoder, &target, previous, rect, 0, 0);
        }
        let renderer = self.warp.get_or_init(|| Renderer::new(&self.device));
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("warp parameters"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("integer image warps"),
            layout: &renderer.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &previous.as_ref().unwrap_or(input).view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("integer image warps"),
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

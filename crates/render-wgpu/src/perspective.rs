use crate::{
    copy::{ImageSource, copy},
    gpu::{FORMAT, Gpu, Image},
};
use krkr_protocol::{
    graphics::{Rect, Size},
    transform::Perspective,
};
use krkr_render::{Error, Result, perspective::Mapping};
use std::sync::Arc;
use wgpu::util::DeviceExt;

const PARAMETER_BYTES: usize = 10 * 16;
pub(crate) struct Projector {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
}
impl Projector {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("perspective"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PARAMETER_BYTES as u64),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("perspective.wgsl"));
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("perspective"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        // krkr2 PerspectiveAlphaBlend_a: RGB = S*Sa + D*(1-Sa),
        // alpha = Sa + Da*(1-Sa). Do not normalize RGB by destination alpha.
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("perspective"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: None,
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fragment"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    write_mask: wgpu::ColorWrites::ALL,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("perspective linear clamp"),
            min_filter: wgpu::FilterMode::Linear,
            mag_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self {
            layout,
            pipeline,
            sampler,
        }
    }
}
impl Gpu {
    pub fn perspective(
        &self,
        image: &mut Image,
        source: &ImageSource,
        mapping: Perspective,
        clip: Rect,
    ) -> Result<()> {
        let resolved = self.materialized_source(source)?;
        let source = resolved.as_ref().unwrap_or(source);
        self.materialize(image)?;
        let source_plane = source
            .main
            .as_ref()
            .ok_or(Error::Message("source has no main plane"))?;
        image.main()?;
        let mapping = Mapping::new(mapping)?;
        let Some(clip) = clip.intersection(image.size.rect()) else {
            return Ok(());
        };
        let projector = self.projector.get_or_init(|| Projector::new(&self.device));
        self.check()?;
        let alias =
            Arc::strong_count(&image.main_owners) == 1 && Arc::ptr_eq(source_plane, image.main()?);
        let footprint = mapping.source_region(clip, source.size);
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
        let permit = self.staging.reserve(PARAMETER_BYTES * 2)?;
        self.independent(image, true, false)?;
        let target = image.main()?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let (input, origin) = if let Some(snapshot) = &snapshot {
            copy(&mut encoder, source_plane, snapshot, footprint, 0, 0);
            (snapshot, [footprint.left as f32, footprint.top as f32])
        } else {
            (source_plane, [0.0, 0.0])
        };
        let mut data = [[0.0f32; 4]; 10];
        data[..3].copy_from_slice(&mapping.inverse);
        data[3..7].copy_from_slice(&mapping.points);
        data[7] = [image.size.width as f32, image.size.height as f32, 0.0, 0.0];
        data[8] = [
            origin[0],
            origin[1],
            input.texture.width() as f32,
            input.texture.height() as f32,
        ];
        let bounds = if snapshot.is_some() {
            Size {
                width: footprint.width,
                height: footprint.height,
            }
        } else {
            source.size
        };
        data[9] = [bounds.width as f32, bounds.height as f32, 0.0, 0.0];
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("perspective parameters"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("perspective"),
            layout: &projector.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&projector.sampler),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("perspective"),
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
            pass.set_pipeline(&projector.pipeline);
            pass.set_viewport(
                0.0,
                0.0,
                image.size.width as f32,
                image.size.height as f32,
                0.0,
                1.0,
            );
            pass.set_bind_group(0, &bind, &[]);
            pass.set_scissor_rect(clip.left as u32, clip.top as u32, clip.width, clip.height);
            pass.draw(0..6, 0..1);
        }
        self.submit(
            encoder,
            (target.clone(), source_plane.clone(), snapshot, permit),
        );
        self.check()
    }
}

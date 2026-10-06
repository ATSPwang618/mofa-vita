//! Main-plane filters use clipped GPU snapshots and the existing COW protocol.
use crate::gpu::{Allocation, FORMAT, Gpu, Image};
use krkr_protocol::{
    filter::{Filter, Kind},
    graphics::{Rect, Size},
};
use krkr_render::{Error, Result};
use wgpu::util::DeviceExt;

pub(crate) struct Processor {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}
const PARAMETER_BYTES: usize = 4128;
impl Processor {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image filter"),
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
                        min_binding_size: wgpu::BufferSize::new(PARAMETER_BYTES as u64),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image filter"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("filter.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("image filter"),
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
    fn draw(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        source: &Allocation,
        target: &Allocation,
        rect: Rect,
        data: &[u32; 1032],
    ) -> wgpu::Buffer {
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("filter parameters"),
                contents: bytemuck::cast_slice(data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image filter"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("image filter"),
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
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.set_scissor_rect(rect.left as u32, rect.top as u32, rect.width, rect.height);
        pass.draw(0..3, 0..1);
        drop(pass);
        buffer
    }
}
impl Gpu {
    pub(crate) fn filter(&self, image: &mut Image, rectangle: Rect, filter: &Filter) -> Result<()> {
        image.main()?;
        let Some(rect) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        let table = filter.table.as_slice();
        let length = table.len() / 4;
        let valid = table.len().is_multiple_of(4)
            && length <= 1024
            && match filter.kind {
                Kind::Lookup | Kind::Colorize { .. } => length == 256,
                Kind::Gaussian => length != 0 && !length.is_multiple_of(2),
                _ => length == 0,
            };
        if !valid {
            return Err(Error::Message("invalid image filter table"));
        }
        let blur = matches!(filter.kind, Kind::Gaussian);
        let passes = match filter.kind {
            Kind::Smudge { passes } => passes,
            Kind::Gaussian => 2,
            _ => 1,
        };
        if passes == 0 {
            return Ok(());
        }
        let size = Size {
            width: rect.width,
            height: rect.height,
        };
        let reads_destination = !matches!(
            filter.kind,
            Kind::RandomFill {
                legacy: false,
                hold_alpha: false,
                ..
            }
        );
        let snapshot = self.temporary(
            if reads_destination {
                size
            } else {
                Size {
                    width: 1,
                    height: 1,
                }
            },
            FORMAT,
        )?;
        let intermediate = if passes > 1 {
            Some(self.temporary(size, FORMAT)?)
        } else {
            None
        };
        let permit = self.staging.reserve(
            PARAMETER_BYTES
                .checked_mul(passes as usize)
                .ok_or(Error::Message("filter command size overflow"))?,
        )?;
        let mut data = [0u32; 1032];
        for (out, bytes) in data[8..].iter_mut().zip(table.as_chunks::<4>().0.iter()) {
            *out = u32::from_le_bytes(*bytes);
        }
        data[..4].copy_from_slice(&match filter.kind {
            Kind::Lookup => [0, 0, 0, 0],
            Kind::Colorize { amount } => [1, u32::from(amount), 0, 0],
            Kind::Modulate {
                hue,
                saturation,
                luminance,
            } => [2, hue.to_bits(), saturation.to_bits(), luminance.to_bits()],
            Kind::Noise { seed, level } => [
                if level.is_some() { 3 } else { 4 },
                seed,
                level.unwrap_or(0) as u32,
                0,
            ],
            Kind::Gaussian => [5, length as u32, 0, 0],
            Kind::Smudge { .. } => [8, 0, 0, 0],
            Kind::Xor { color } => [9, color, 0, 0],
            Kind::Dither { width, height } => [10, width, height, 0],
            Kind::RandomFill {
                seed, under, range, ..
            } => [7, seed, under as u32, range as u32],
        });
        if let Kind::RandomFill {
            legacy,
            monochrome,
            hold_alpha,
            rectangle,
            ..
        } = filter.kind
        {
            data[8..14].copy_from_slice(&[
                u32::from(monochrome),
                u32::from(hold_alpha),
                rectangle.left as u32,
                rectangle.top as u32,
                rectangle.width,
                u32::from(legacy),
            ]);
        }
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let processor = self.filter.get_or_init(|| Processor::new(&self.device));
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if reads_destination {
            crate::copy::copy(&mut encoder, &target, &snapshot, rect, 0, 0);
        }
        let mut buffers = Vec::with_capacity(passes as usize);
        data[6] = rect.width;
        data[7] = rect.height;
        for pass in 0..passes.saturating_sub(1) {
            let intermediate = intermediate.as_ref().unwrap();
            let (input, output) = if pass % 2 == 0 {
                (&snapshot, intermediate)
            } else {
                (intermediate, &snapshot)
            };
            buffers.push(processor.draw(self, &mut encoder, input, output, size.rect(), &data));
            if blur {
                data[0] = 6;
            }
        }
        data[4] = rect.left as u32;
        data[5] = rect.top as u32;
        buffers.push(processor.draw(
            self,
            &mut encoder,
            if passes % 2 == 0 {
                intermediate.as_ref().unwrap()
            } else {
                &snapshot
            },
            &target,
            rect,
            &data,
        ));
        self.submit(
            encoder,
            (
                snapshot,
                intermediate,
                target,
                buffers,
                permit,
                filter.table.clone(),
            ),
        );
        self.check()
    }
}

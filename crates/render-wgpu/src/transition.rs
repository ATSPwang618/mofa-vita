use crate::{
    ImageSource,
    gpu::{Allocation, FORMAT, Gpu, Image},
};
use krkr_protocol::{
    graphics::DrawFace,
    transition::{Effect, Frame},
};
use krkr_render::{Error, Result};
use std::sync::Arc;
use wgpu::util::DeviceExt;
mod custom;
pub(crate) struct DrawResources {
    _buffer: wgpu::Buffer,
    _custom: Option<custom::Resources>,
}

pub(crate) struct Transitions {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    custom: custom::Registry,
}
impl Transitions {
    pub(crate) fn collect(&self) {
        self.custom.collect();
    }
    fn new(gpu: &Gpu) -> Self {
        let device = &gpu.device;
        let mut entries: Vec<_> = (0..4)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            })
            .collect();
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 4,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(48),
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transition inputs"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transition layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("transition.wgsl"));
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("legacy transition"),
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
        Self {
            layout,
            pipeline,
            custom: custom::Registry::new(gpu, entries),
        }
    }
    pub(crate) fn draw(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &Allocation,
        inputs: [&Allocation; 3],
        frame: Frame,
        custom: Option<&krkr_protocol::transition::custom::Frame>,
    ) -> Result<DrawResources> {
        if let Some(custom) = custom {
            return self
                .custom
                .draw(gpu, encoder, target, inputs, frame, custom);
        }
        let (kind, vague, from, stay) = match frame.effect {
            Effect::CrossFade => (0, 0, 0, 0),
            Effect::Universal { vague } => (1, vague as i32, 0, 0),
            Effect::Scroll { from, stay } => (2, 0, from as i32, stay as i32),
            Effect::Custom => {
                return Err(Error::Message(
                    "custom transition has no executable instance",
                ));
            }
        };
        let face = match frame.face {
            DrawFace::Alpha => 0,
            DrawFace::AddAlpha => 4,
            _ => 1,
        };
        let parameters = [
            kind,
            frame.phase as i32,
            frame.effect.phases(frame.size) as i32,
            vague,
            face,
            from,
            stay,
            0,
            frame.size.width as i32,
            frame.size.height as i32,
            0,
            0,
        ];
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("transition phase"),
                contents: bytemuck::cast_slice(&parameters),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let lookup = gpu.mixer()?.lookup();
        let mut entries: Vec<_> = inputs
            .into_iter()
            .chain([lookup])
            .enumerate()
            .map(|(index, image)| wgpu::BindGroupEntry {
                binding: index as u32,
                resource: wgpu::BindingResource::TextureView(&image.view),
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: buffer.as_entire_binding(),
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("transition sources"),
            layout: &self.layout,
            entries: &entries,
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("transition"),
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
        pass.set_scissor_rect(0, 0, frame.size.width, frame.size.height);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        Ok(DrawResources {
            _buffer: buffer,
            _custom: None,
        })
    }
}
impl Gpu {
    pub(crate) fn transitions(&self) -> &Transitions {
        self.transitions.get_or_init(|| Transitions::new(self))
    }
    /// The input allocations remain alive through submission. An aliased output
    /// uses one bounded temporary, so sampling and writing never share a texture.
    pub fn transition(
        &self,
        target: &mut Image,
        first: &ImageSource,
        second: &ImageSource,
        rule: Option<&ImageSource>,
        frame: Frame,
    ) -> Result<()> {
        let resolved_first = self.materialized_source(first)?;
        let first = resolved_first.as_ref().unwrap_or(first);
        let resolved_second = self.materialized_source(second)?;
        let second = resolved_second.as_ref().unwrap_or(second);
        let resolved_rule = rule
            .filter(|r| r.province.is_none())
            .map(|r| self.materialized_source(r))
            .transpose()?
            .flatten();
        let rule = resolved_rule.as_ref().or(rule);
        if target.size != frame.size || first.size != frame.size || second.size != frame.size {
            return Err(Error::Message(
                "transition images must have equal dimensions",
            ));
        }
        if matches!(frame.effect,Effect::Universal{vague} if vague > i32::MAX as u32 / 255) {
            return Err(Error::Message(
                "transition vague exceeds integer kernel range",
            ));
        }
        let first = first
            .main
            .as_ref()
            .ok_or(Error::Message("transition source has no image"))?;
        let second = second
            .main
            .as_ref()
            .ok_or(Error::Message("transition source has no image"))?;
        let rule = if matches!(frame.effect, Effect::Universal { .. }) {
            let rule = rule.ok_or(Error::Message("universal transition requires a rule image"))?;
            if rule.size != frame.size {
                return Err(Error::Message("transition rule size mismatch"));
            }
            rule.province
                .as_ref()
                .or(rule.main.as_ref())
                .ok_or(Error::Message("rule has no pixel plane"))?
        } else {
            first
        };
        self.independent(target, true, false)?;
        let aliased = [first, second, rule]
            .iter()
            .any(|source| Arc::ptr_eq(target.main().unwrap(), source));
        let scratch = aliased
            .then(|| self.temporary(frame.size, FORMAT))
            .transpose()?;
        let output = scratch.as_ref().unwrap_or(target.main()?);
        let permit = self.staging.reserve(48)?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let parameters = self.transitions().draw(
            self,
            &mut encoder,
            output,
            [first, second, rule],
            frame,
            None,
        )?;
        if aliased {
            crate::copy::copy(
                &mut encoder,
                output,
                target.main()?,
                frame.size.rect(),
                0,
                0,
            );
        }
        self.submit(
            encoder,
            (
                first.clone(),
                second.clone(),
                rule.clone(),
                target.main()?.clone(),
                scratch,
                parameters,
                permit,
            ),
        );
        self.check()
    }
}

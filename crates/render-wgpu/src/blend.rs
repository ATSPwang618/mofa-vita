use crate::{
    copy::{ImageSource, copy},
    gpu::{Allocation, FORMAT, Gpu, Image, extent},
};
use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, Rect, Size};
use krkr_render::{Error, Result, blit};
use std::sync::Arc;
use wgpu::util::DeviceExt;

#[derive(Clone, Copy)]
pub(crate) struct Parameters(pub [i32; 28]);
pub(crate) const PARAMETER_BYTES: usize = std::mem::size_of::<Parameters>();
impl Parameters {
    pub fn new(offset: (i32, i32), clip: Rect, options: BlendOptions) -> Self {
        let mut data = [0; 28];
        data[..12].copy_from_slice(&[
            offset.0,
            offset.1,
            clip.left,
            clip.top,
            options.mode as i32,
            match options.face {
                DrawFace::Alpha => 0,
                DrawFace::Opaque => 1,
                DrawFace::Mask => 2,
                DrawFace::Province => 3,
                DrawFace::AddAlpha => 4,
            },
            options.opacity.into(),
            i32::from(options.hold_alpha),
            0,
            0,
            0,
            0,
        ]);
        Self(data)
    }
    pub fn needs_destination(self) -> bool {
        self.0[4] >= 0 && !(self.0[4] == Blend::Opaque as i32 && self.0[6] == 255)
    }
    pub fn copies_color(self) -> bool {
        self.0[4] == -2
            || self.0[4] == Blend::Opaque as i32
                && self.0[6] == 255
                && self.0[5] == 1
                && self.0[7] & 1 != 0
    }
}
pub(crate) struct Mixer {
    layout: wgpu::BindGroupLayout,
    pipelines: [wgpu::RenderPipeline; 2],
    lookup: Arc<Allocation>,
}
impl Mixer {
    // Bind an existing texture for a uniform-color source; the shader does not
    // sample it, and no full-window scratch allocation is needed.
    pub(crate) fn solid_source(&self) -> Arc<Allocation> {
        self.lookup.clone()
    }
    pub(crate) fn lookup(&self) -> &Allocation {
        &self.lookup
    }
    fn new(gpu: &Gpu) -> Result<Self> {
        let device = &gpu.device;
        let mut entries: Vec<_> = (0..3)
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
            binding: 3,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: wgpu::BufferSize::new(PARAMETER_BYTES as u64),
            },
            count: None,
        });
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 4,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(16),
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("legacy blend inputs"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("legacy blend"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("blend.wgsl"));
        let pipelines = [
            wgpu::ColorWrites::ALL,
            wgpu::ColorWrites::RED | wgpu::ColorWrites::GREEN | wgpu::ColorWrites::BLUE,
        ]
        .map(|write_mask| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("legacy integer blend"),
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
                        write_mask,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        });
        let size = Size {
            width: 256,
            height: 256 + 65,
        };
        let lookup = gpu.allocation(size, FORMAT, &gpu.resident)?;
        let permit = gpu.staging.reserve(256 * (256 + 65) * 8)?;
        let data = krkr_render::blend::lookup_table();
        gpu.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &lookup.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(1024),
                rows_per_image: None,
            },
            extent(size),
        );
        gpu.submit(
            device.create_command_encoder(&Default::default()),
            (lookup.clone(), permit),
        );
        gpu.check()?;
        Ok(Self {
            layout,
            pipelines,
            lookup,
        })
    }
    pub(crate) fn bind(
        &self,
        gpu: &Gpu,
        source: &Allocation,
        destination: Option<&Allocation>,
        buffer: &wgpu::Buffer,
        coefficients: Option<&wgpu::Buffer>,
    ) -> wgpu::BindGroup {
        gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("legacy blend"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: match coefficients {
                        Some(coefficients) => coefficients.as_entire_binding(),
                        None => wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer,
                            offset: 0,
                            size: wgpu::BufferSize::new(16),
                        }),
                    },
                },
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(
                        &destination.unwrap_or(&self.lookup).view,
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&self.lookup.view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer,
                        offset: 0,
                        size: wgpu::BufferSize::new(PARAMETER_BYTES as u64),
                    }),
                },
            ],
        })
    }
    pub(crate) fn pipeline(&self, color_only: bool) -> &wgpu::RenderPipeline {
        &self.pipelines[usize::from(color_only)]
    }
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &Allocation,
        source: &Allocation,
        destination: Option<&Allocation>,
        buffer: &wgpu::Buffer,
        offset: u32,
        clip: Rect,
        copies_color: bool,
        coefficients: Option<&wgpu::Buffer>,
    ) {
        let bind = self.bind(gpu, source, destination, buffer, coefficients);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("legacy blend"),
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
        pass.set_pipeline(&self.pipelines[usize::from(copies_color)]);
        pass.set_bind_group(0, &bind, &[offset]);
        pass.set_scissor_rect(clip.left as u32, clip.top as u32, clip.width, clip.height);
        pass.draw(0..3, 0..1);
    }
}
impl Gpu {
    pub(crate) fn mixer(&self) -> Result<&Mixer> {
        if let Some(mixer) = self.mixer.get() {
            return Ok(mixer);
        }
        let mixer = Mixer::new(self)?;
        Ok(self.mixer.get_or_init(|| mixer))
    }
    pub fn color_rect(
        &self,
        image: &mut Image,
        rectangle: Rect,
        color: u32,
        opacity: i16,
        face: DrawFace,
    ) -> Result<()> {
        if !matches!(
            face,
            DrawFace::Opaque | DrawFace::Alpha | DrawFace::AddAlpha
        ) {
            return self.fill(
                image,
                &[krkr_protocol::graphics::Fill {
                    rectangle,
                    color,
                    face,
                    hold_alpha: false,
                }],
            );
        }
        let Some(rect) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        if self.edit_generated(image, rect, |tile, local, _| {
            self.color_rect(tile, local, color, opacity, face)
        })? {
            return Ok(());
        }
        image.main()?;
        if face == DrawFace::AddAlpha && opacity < 0 {
            return Err(Error::Message(
                "negative opacity is not supported on additive alpha",
            ));
        }
        if opacity == 0 {
            return Ok(());
        }
        if opacity >= 255 {
            return self.fill(
                image,
                &[krkr_protocol::graphics::Fill {
                    rectangle: rect,
                    color: color | 0xff000000,
                    face,
                    hold_alpha: face == DrawFace::Opaque,
                }],
            );
        }
        let mut parameters = Parameters::new(
            (0, 0),
            rect,
            BlendOptions {
                mode: Blend::Alpha,
                face,
                opacity: 255,
                hold_alpha: false,
            },
        );
        parameters.0[6] = i32::from(opacity.clamp(-255, 255));
        parameters.0[7] = 2;
        parameters.0[8..12].copy_from_slice(&[
            (color >> 16 & 255) as i32,
            (color >> 8 & 255) as i32,
            (color & 255) as i32,
            255,
        ]);
        self.draw_pixels(image, None, rect, rect, parameters)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn operate_rect(
        &self,
        image: &mut Image,
        source: &ImageSource,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        options: BlendOptions,
    ) -> Result<()> {
        if image.deferred.is_some() {
            if !options.accepts_face() {
                return Err(Error::Message(
                    "operation is not supported on this draw face",
                ));
            }
            if options.is_noop() {
                return Ok(());
            }
            let Some((src, dst)) = blit::region(source.size, image.size, clip, rectangle, x, y)
            else {
                return Ok(());
            };
            self.edit_generated(image, dst, |tile, local, origin| {
                self.operate_rect(
                    tile,
                    source,
                    Rect {
                        left: src.left + origin.0 + local.left - dst.left,
                        top: src.top + origin.1 + local.top - dst.top,
                        width: local.width,
                        height: local.height,
                    },
                    local.left,
                    local.top,
                    tile.size.rect(),
                    options,
                )
            })?;
            return Ok(());
        }
        if source.deferred.is_some() {
            let Some((src, dst)) = blit::region(source.size, image.size, clip, rectangle, x, y)
            else {
                return Ok(());
            };
            let region = self.resolve_main(&source.snapshot_main(), src)?;
            let input = Image::new(
                Some(region.allocation),
                None,
                Size {
                    width: region.rectangle.width,
                    height: region.rectangle.height,
                },
            );
            return self.operate_rect(
                image,
                &input.source(),
                Rect {
                    left: src.left - region.rectangle.left,
                    top: src.top - region.rectangle.top,
                    ..src
                },
                dst.left,
                dst.top,
                clip,
                options,
            );
        }
        self.materialize(image)?;
        if !options.accepts_face() {
            return Err(Error::Message(
                "operation is not supported on this draw face",
            ));
        }
        let source_plane = source
            .main
            .as_ref()
            .ok_or(Error::Message("source has no main plane"))?;
        image.main()?;
        let Some((src, dst)) = blit::region(source.size, image.size, clip, rectangle, x, y) else {
            return Ok(());
        };
        if options.is_noop() {
            return Ok(());
        }
        if options.mode == Blend::Opaque
            && options.opacity == 255
            && options.face == DrawFace::Opaque
        {
            return self.copy_rect(
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                DrawFace::Opaque,
                options.hold_alpha,
            );
        }
        let parameters = Parameters::new((src.left - dst.left, src.top - dst.top), dst, options);
        self.draw_pixels(image, Some(source_plane.clone()), src, dst, parameters)
    }
    fn draw_pixels(
        &self,
        image: &mut Image,
        source: Option<Arc<Allocation>>,
        src: Rect,
        dst: Rect,
        mut parameters: Parameters,
    ) -> Result<()> {
        let mixer = self.mixer()?;
        let size = Size {
            width: dst.width,
            height: dst.height,
        };
        let backdrop = parameters
            .needs_destination()
            .then(|| self.temporary(size, FORMAT))
            .transpose()?;
        let alias = Arc::strong_count(&image.main_owners) == 1
            && source
                .as_ref()
                .is_some_and(|s| Arc::ptr_eq(s, image.main.as_ref().unwrap()));
        let source_copy = if alias && (src != dst || backdrop.is_none()) {
            Some(self.temporary(size, FORMAT)?)
        } else {
            None
        };
        let permit = self.staging.reserve(PARAMETER_BYTES * 2)?;
        self.independent(image, true, false)?;
        let target = image.main()?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(backdrop) = &backdrop {
            copy(&mut encoder, target, backdrop, dst, 0, 0);
        }
        let source = if let Some(source) = source {
            if alias {
                parameters.0[0] = -dst.left;
                parameters.0[1] = -dst.top;
                if let Some(temp) = &source_copy {
                    copy(&mut encoder, &source, temp, src, 0, 0);
                    temp.clone()
                } else {
                    backdrop.as_ref().unwrap().clone()
                }
            } else {
                source
            }
        } else {
            mixer.lookup.clone()
        };
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("blend parameters"),
                contents: bytemuck::cast_slice(&parameters.0),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
            });
        mixer.draw(
            self,
            &mut encoder,
            target,
            &source,
            backdrop.as_deref(),
            &buffer,
            0,
            dst,
            parameters.copies_color(),
            None,
        );
        self.submit(
            encoder,
            (target.clone(), source, source_copy, backdrop, permit),
        );
        self.check()
    }
}

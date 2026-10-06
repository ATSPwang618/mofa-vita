use crate::gpu::{Allocation, FORMAT, Gpu, Image, extent};
use krkr_protocol::graphics::{DrawFace, Fill, Rect, Size};
use krkr_render::{Result, blit};
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// A read dependency on the allocation at the time the operation was issued.
/// This is not a copy-on-write image; writes remain ordered on the GPU queue.
pub struct ImageSource {
    pub(crate) main: Option<Arc<Allocation>>,
    pub(crate) deferred: Option<Arc<crate::deferred::Deferred>>,
    pub(crate) main_owners: std::sync::Weak<()>,
    pub(crate) province: Option<Arc<Allocation>>,
    pub size: Size,
}
impl Image {
    pub fn source(&self) -> ImageSource {
        ImageSource {
            main: self.main.clone(),
            deferred: self.deferred.clone(),
            main_owners: Arc::downgrade(&self.main_owners),
            province: self.province.clone(),
            size: self.size,
        }
    }
    pub fn has_province(&self) -> bool {
        self.province.is_some()
    }
}

pub(crate) struct Copier {
    pub(crate) layout: wgpu::BindGroupLayout,
    pub(crate) pipelines: [wgpu::RenderPipeline; 4],
}
impl Copier {
    pub fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("copy inputs"),
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
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(48),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("copy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("copy.wgsl"));
        let pipelines = [
            wgpu::ColorWrites::ALL,
            wgpu::ColorWrites::RED | wgpu::ColorWrites::GREEN | wgpu::ColorWrites::BLUE,
            wgpu::ColorWrites::ALPHA,
            wgpu::ColorWrites::RED,
        ]
        .map(|mask| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("copy"),
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
                        format: if mask == wgpu::ColorWrites::RED {
                            wgpu::TextureFormat::R8Unorm
                        } else {
                            FORMAT
                        },
                        blend: None,
                        write_mask: mask,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        });
        Self { layout, pipelines }
    }
}
impl Gpu {
    #[allow(clippy::too_many_arguments)]
    pub fn copy_rect(
        &self,
        destination: &mut Image,
        source: &ImageSource,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        face: DrawFace,
        hold_alpha: bool,
    ) -> Result<()> {
        let Some((src, dst)) = blit::region(source.size, destination.size, clip, rectangle, x, y)
        else {
            return Ok(());
        };
        if face != DrawFace::Province && destination.deferred.is_some() {
            self.edit_generated(destination, dst, |tile, local, origin| {
                self.copy_rect(
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
                    face,
                    hold_alpha,
                )
            })?;
            return Ok(());
        }
        if face != DrawFace::Province && source.deferred.is_some() {
            let region = self.resolve_main(&source.snapshot_main(), src)?;
            let input = Image::new(
                Some(region.allocation),
                None,
                Size {
                    width: region.rectangle.width,
                    height: region.rectangle.height,
                },
            );
            return self.copy_rect(
                destination,
                &input.source(),
                Rect {
                    left: src.left - region.rectangle.left,
                    top: src.top - region.rectangle.top,
                    ..src
                },
                dst.left,
                dst.top,
                clip,
                face,
                hold_alpha,
            );
        }
        self.independent(
            destination,
            face != DrawFace::Province,
            face == DrawFace::Province,
        )?;
        let (source_plane, target, pipeline) = match face {
            DrawFace::Province => {
                let Some(plane) = &source.province else {
                    return self.fill(
                        destination,
                        &[Fill {
                            rectangle: dst,
                            color: 0,
                            face,
                            hold_alpha,
                        }],
                    );
                };
                let target = match &destination.province {
                    Some(plane) => plane.clone(),
                    None => self.allocation(
                        destination.size,
                        wgpu::TextureFormat::R8Unorm,
                        &self.resident,
                    )?,
                };
                (plane.clone(), target, 3)
            }
            DrawFace::Mask => (
                source
                    .main
                    .as_ref()
                    .ok_or(krkr_render::Error::Message("source has no main plane"))?
                    .clone(),
                destination.main()?.clone(),
                2,
            ),
            DrawFace::Opaque if hold_alpha => (
                source
                    .main
                    .as_ref()
                    .ok_or(krkr_render::Error::Message("source has no main plane"))?
                    .clone(),
                destination.main()?.clone(),
                1,
            ),
            _ => (
                source
                    .main
                    .as_ref()
                    .ok_or(krkr_render::Error::Message("source has no main plane"))?
                    .clone(),
                destination.main()?.clone(),
                0,
            ),
        };
        // A render attachment cannot simultaneously be sampled, even for
        // disjoint rectangles. Snapshot only the source rectangle for aliasing.
        let temporary = if Arc::ptr_eq(&source_plane, &target) {
            Some(self.temporary(
                Size {
                    width: src.width,
                    height: src.height,
                },
                source_plane.texture.format(),
            )?)
        } else {
            None
        };
        // Full-channel copies need no sampling or conversion. Use the texture
        // copy path for RGBA and R8; channel masks retain the shader path.
        let direct = pipeline == 0 || pipeline == 3;
        let permit = if direct {
            None
        } else {
            Some(self.staging.reserve(96)?)
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if face == DrawFace::Province && destination.province.is_none() {
            self.clear(&mut encoder, &target, [0.0; 4]);
        }
        let (input, offset) = if let Some(temporary) = &temporary {
            copy(&mut encoder, &source_plane, temporary, src, 0, 0);
            (temporary, [-dst.left, -dst.top, 0, 0])
        } else {
            (
                &source_plane,
                [src.left - dst.left, src.top - dst.top, 0, 0],
            )
        };
        if direct {
            let rectangle = if temporary.is_some() {
                Rect {
                    left: 0,
                    top: 0,
                    width: src.width,
                    height: src.height,
                }
            } else {
                src
            };
            copy(
                &mut encoder,
                input,
                &target,
                rectangle,
                dst.left as u32,
                dst.top as u32,
            );
        } else {
            let buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("copy offset"),
                    contents: bytemuck::cast_slice(&[offset, [0; 4], [0; 4]]),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("copy"),
                layout: &self.copier.layout,
                entries: &[
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
                    label: Some("copy"),
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
                pass.set_pipeline(&self.copier.pipelines[pipeline]);
                pass.set_bind_group(0, &bind, &[0]);
                pass.set_scissor_rect(dst.left as u32, dst.top as u32, dst.width, dst.height);
                pass.draw(0..3, 0..1);
            }
        }
        self.submit(encoder, (source_plane, target.clone(), temporary, permit));
        self.check()?;
        if face == DrawFace::Province {
            if destination.province.is_none() {
                destination.province_owners = Arc::new(());
            }
            destination.province = Some(target);
        }
        Ok(())
    }
}
impl Gpu {
    pub fn copy_wrapped(
        &self,
        destination: &mut Image,
        source: &ImageSource,
        rect: Rect,
        dest: Rect,
        shift: (i32, i32),
        clip: Rect,
    ) -> Result<()> {
        let resolved = self.materialized_source(source)?;
        let source = resolved.as_ref().unwrap_or(source);
        let Some(dst) = dest
            .intersection(clip)
            .and_then(|r| r.intersection(destination.size.rect()))
        else {
            return Ok(());
        };
        if rect.width == 0
            || rect.height == 0
            || rect.intersection(source.size.rect()) != Some(rect)
        {
            return Err(krkr_render::Error::Message(
                "wrapped source rectangle is outside image",
            ));
        }
        let source_plane = source
            .main
            .as_ref()
            .ok_or(krkr_render::Error::Message("source has no main plane"))?
            .clone();
        self.independent(destination, true, false)?;
        let target = destination.main()?.clone();
        let temporary = if Arc::ptr_eq(&source_plane, &target) {
            Some(self.temporary(
                Size {
                    width: rect.width,
                    height: rect.height,
                },
                FORMAT,
            )?)
        } else {
            None
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let (input, origin) = if let Some(temp) = &temporary {
            copy(&mut encoder, &source_plane, temp, rect, 0, 0);
            (temp, [0, 0])
        } else {
            (&source_plane, [rect.left, rect.top])
        };
        let data = [
            // Reduce before the shader adds the destination coordinate, so
            // extreme signed offsets cannot wrap the addition. Negative exact
            // multiples select zero; the original pointer loop selected the
            // out-of-bounds denominator in that case.
            [
                shift.0.rem_euclid(rect.width as i32),
                shift.1.rem_euclid(rect.height as i32),
                origin[0],
                origin[1],
            ],
            [rect.width as i32, rect.height as i32, 1, 0],
            [0; 4],
        ];
        let permit = self.staging.reserve(96)?;
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("wrapped copy"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("wrapped copy"),
            layout: &self.copier.layout,
            entries: &[
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
                label: Some("wrapped copy"),
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
            pass.set_pipeline(&self.copier.pipelines[0]);
            pass.set_bind_group(0, &bind, &[0]);
            pass.set_scissor_rect(dst.left as u32, dst.top as u32, dst.width, dst.height);
            pass.draw(0..3, 0..1);
        }
        self.submit(encoder, (source_plane, target, temporary, permit));
        self.check()
    }
}
pub(crate) fn copy(
    encoder: &mut wgpu::CommandEncoder,
    source: &Allocation,
    target: &Allocation,
    rect: Rect,
    x: u32,
    y: u32,
) {
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &source.texture,
            mip_level: 0,
            origin: wgpu::Origin3d {
                x: rect.left as u32,
                y: rect.top as u32,
                z: 0,
            },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &target.texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        extent(Size {
            width: rect.width,
            height: rect.height,
        }),
    );
}

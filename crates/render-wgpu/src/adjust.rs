use crate::gpu::{FORMAT, Gpu, Image};
use krkr_protocol::graphics::{Adjustment, Rect, Size};
use krkr_render::Result;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub(crate) struct Adjuster {
    layout: wgpu::BindGroupLayout,
    pipelines: [wgpu::RenderPipeline; 2],
}
impl Adjuster {
    fn new(device: &wgpu::Device) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("image adjustment"),
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
                        min_binding_size: wgpu::BufferSize::new(4112),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("image adjustment"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("adjust.wgsl"));
        let pipelines = [FORMAT, wgpu::TextureFormat::R8Unorm].map(|format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("image adjustment"),
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
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
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
    pub fn adjust(&self, image: &mut Image, rectangle: Rect, operation: &Adjustment) -> Result<()> {
        self.materialize(image)?;
        if let Adjustment::Lines(lines) = operation {
            return self.draw_lines(image, rectangle, lines);
        }
        if let Adjustment::BoxBlur { radius, alpha } = operation {
            return self.box_blur(image, rectangle, *radius, *alpha);
        }
        if let Adjustment::Filter(filter) = operation {
            return self.filter(image, rectangle, filter);
        }
        if matches!(operation, Adjustment::Gamma { .. } | Adjustment::GrayScale) {
            return self.adjust_points(image, rectangle, operation);
        }
        let Some(rect) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        image.main()?;
        let plugin = matches!(
            operation,
            Adjustment::ColorField { .. } | Adjustment::Gradient { .. }
        );
        let reads_destination = matches!(operation, Adjustment::Gradient { blend: true, .. });
        // Plugin fills only detach logically shared main planes. A blending
        // gradient snapshots its clipped rectangle; a replacement needs merely
        // a distinct one-pixel binding, never a copy of the whole destination.
        let source = if plugin {
            self.temporary(
                if reads_destination {
                    Size {
                        width: rect.width,
                        height: rect.height,
                    }
                } else {
                    Size {
                        width: 1,
                        height: 1,
                    }
                },
                FORMAT,
            )?
        } else {
            image.main()?.clone()
        };
        let flip = matches!(operation, Adjustment::Flip { .. });
        let target = if plugin {
            self.independent(image, true, false)?;
            image.main()?.clone()
        } else {
            self.allocation(image.size, FORMAT, &self.resident)?
        };
        let province = if flip {
            image
                .province
                .as_ref()
                .map(|_| self.allocation(image.size, wgpu::TextureFormat::R8Unorm, &self.resident))
                .transpose()?
        } else {
            None
        };
        let permit = self.staging.reserve(4112 * 2)?;
        let mut data = [[0u32; 4]; 257];
        match operation {
            Adjustment::Lines(_) | Adjustment::Filter(_) | Adjustment::BoxBlur { .. } => {
                unreachable!("filters use their own passes")
            }
            Adjustment::Gradient {
                bounds,
                from,
                to,
                vertical,
                blend,
            } => {
                data[0] = [5, u32::from(*vertical), u32::from(*blend), 0];
                data[1] = [*from, *to, 0, 0];
                data[2] = [
                    bounds.left as u32,
                    bounds.top as u32,
                    bounds.width,
                    bounds.height,
                ];
                // The reference SSE2 AlphaBlend handles aligned four-pixel
                // groups and scalar edges differently at full source alpha.
                data[4] = [rect.left as u32, rect.top as u32, rect.width, rect.height];
                data[3] = [rect.left as u32, rect.top as u32, 0, 0];
            }
            Adjustment::ColorField {
                size,
                hsv,
                axes,
                values,
            } => {
                data[0] = [6, u32::from(*hsv), size.width, size.height];
                data[1][..3].copy_from_slice(&axes.map(|v| v as u32));
                data[2][..3].copy_from_slice(&values.map(|v| {
                    if *hsv {
                        (v as f32).to_bits()
                    } else {
                        v as i32 as u32
                    }
                }));
            }
            Adjustment::Gamma { table, additive } => {
                data[0][0] = u32::from(*additive);
                data[1..].copy_from_slice(table.as_ref());
            }
            Adjustment::GrayScale => data[0][0] = 2,
            Adjustment::Flip { horizontal } => {
                data[0] = [
                    if *horizontal { 3 } else { 4 },
                    0,
                    image.size.width,
                    image.size.height,
                ];
            }
        }
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("gamma table"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let adjuster = self.adjuster.get_or_init(|| Adjuster::new(&self.device));
        let mut encoder = self.device.create_command_encoder(&Default::default());
        for (index, (src, dst)) in std::iter::once((&source, &target))
            .chain(image.province.as_ref().zip(province.as_ref()))
            .enumerate()
        {
            if plugin {
                if reads_destination {
                    crate::copy::copy(&mut encoder, dst, src, rect, 0, 0);
                }
            } else {
                crate::copy::copy(&mut encoder, src, dst, image.size.rect(), 0, 0);
            }
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("image adjustment"),
                layout: &adjuster.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&src.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: buffer.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("image adjustment"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &dst.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&adjuster.pipelines[index]);
            pass.set_bind_group(0, &bind, &[]);
            pass.set_scissor_rect(rect.left as u32, rect.top as u32, rect.width, rect.height);
            pass.draw(0..3, 0..1);
        }
        self.submit(
            encoder,
            (
                image.source(),
                source,
                target.clone(),
                province.clone(),
                permit,
            ),
        );
        self.check()?;
        image.main = Some(target);
        image.main_owners = Arc::new(());
        if flip {
            image.province = province;
            image.province_owners = Arc::new(());
        }
        Ok(())
    }

    /// Pointwise corrections need only the current strip's input snapshot.
    /// Keep a unique canvas writable rather than allocating a second full plane.
    fn adjust_points(
        &self,
        image: &mut Image,
        rectangle: Rect,
        operation: &Adjustment,
    ) -> Result<()> {
        let Some(rect) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        let width = rect.width.min(256);
        let source = self.temporary(
            Size {
                width,
                height: rect.height,
            },
            FORMAT,
        )?;
        let permit = self
            .staging
            .reserve(4112 * rect.width.div_ceil(width) as usize)?;
        let mut data = [[0u32; 4]; 257];
        match operation {
            Adjustment::Gamma { table, additive } => {
                data[0][0] = u32::from(*additive);
                data[1..].copy_from_slice(table.as_ref());
            }
            Adjustment::GrayScale => data[0][0] = 2,
            _ => unreachable!("point corrections only"),
        }
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let adjuster = self.adjuster.get_or_init(|| Adjuster::new(&self.device));
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let mut buffers = Vec::new();
        for offset in (0..rect.width).step_by(width as usize) {
            let strip = Rect {
                left: rect.left + offset as i32,
                width: width.min(rect.width - offset),
                ..rect
            };
            crate::copy::copy(&mut encoder, &target, &source, strip, 0, 0);
            data[0][1] = strip.left as u32;
            data[0][2] = strip.top as u32;
            let buffer = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("point correction strip"),
                    contents: bytemuck::cast_slice(&data),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("point correction"),
                layout: &adjuster.layout,
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
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("point correction"),
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
                pass.set_pipeline(&adjuster.pipelines[0]);
                pass.set_bind_group(0, &bind, &[]);
                pass.set_scissor_rect(
                    strip.left as u32,
                    strip.top as u32,
                    strip.width,
                    strip.height,
                );
                pass.draw(0..3, 0..1);
            }
            buffers.push(buffer);
        }
        self.submit(encoder, (target, source, buffers, permit));
        self.check()
    }
}

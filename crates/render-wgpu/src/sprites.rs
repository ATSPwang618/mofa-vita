//! One ordered tile pass for an atlas of affine sprites, sharing blend.wgsl.
use crate::{
    copy::ImageSource,
    gpu::{FORMAT, Gpu, Image},
};
use krkr_protocol::{
    budget::Permit,
    graphics::{BlendOptions, DrawFace, Rect, Size},
    sprites::Sprites,
    transform::Transform,
};
use krkr_render::{
    Error, Result,
    transform::{Mapping, validate_source},
};
use std::sync::Arc;
use wgpu::util::DeviceExt;
pub(crate) struct Renderer {
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
}
impl Renderer {
    fn new(device: &wgpu::Device) -> Self {
        let mut entries = (0..3)
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
            .collect::<Vec<_>>();
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 5,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(64),
            },
            count: None,
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ordered sprites"),
            entries: &entries,
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ordered sprites"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ordered sprites"),
            source: wgpu::ShaderSource::Wgsl(
                concat!(
                    include_str!("blend.wgsl"),
                    "\n",
                    include_str!("sprites.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ordered sprites"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vertex"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("sprite_fragment"),
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
fn rect_words(r: Rect) -> [u32; 4] {
    [
        r.left as u32,
        r.top as u32,
        r.left as u32 + r.width,
        r.top as u32 + r.height,
    ]
}
fn pack(
    gpu: &Gpu,
    batch: &Sprites,
    source: Size,
    clip: Rect,
    options: BlendOptions,
) -> Result<Option<(Rect, Vec<u32>, Permit)>> {
    let count = batch
        .clear
        .len()
        .checked_add(batch.sprites.len())
        .ok_or(Error::Message("sprite count overflow"))?;
    let records_permit = gpu.staging.reserve(
        count
            .checked_mul(64)
            .ok_or(Error::Message("sprite packet overflow"))?,
    )?;
    let mut records = Vec::<[u32; 16]>::with_capacity(count);
    let mut affected: Option<Rect> = None;
    let mut add = |bounds: Rect, record: [u32; 16]| {
        affected = Some(if let Some(old) = affected {
            let x = old.left.min(bounds.left);
            let y = old.top.min(bounds.top);
            Rect {
                left: x,
                top: y,
                width: (old.left + old.width as i32)
                    .max(bounds.left + bounds.width as i32)
                    .saturating_sub(x) as u32,
                height: (old.top + old.height as i32)
                    .max(bounds.top + bounds.height as i32)
                    .saturating_sub(y) as u32,
            }
        } else {
            bounds
        });
        records.push(record);
    };
    if options.face != DrawFace::Province {
        for r in &batch.clear {
            if let Some(r) = r.intersection(clip) {
                let mut row = [0; 16];
                row[1..5].copy_from_slice(&rect_words(r));
                add(r, row);
            }
        }
    }
    for sprite in &batch.sprites {
        if sprite.opacity == 0 || sprite.source.width == 0 || sprite.source.height == 0 {
            continue;
        }
        validate_source(sprite.source, source)?;
        let Some(mapping) = Mapping::new(sprite.source, Transform::Affine(sprite.points), clip)?
        else {
            continue;
        };
        let mut row = [0; 16];
        row[0] = 1;
        row[1..5].copy_from_slice(&rect_words(mapping.bounds));
        row[5..9].copy_from_slice(&rect_words(sprite.source));
        row[9..15].copy_from_slice(&mapping.inverse.map(f32::to_bits));
        row[15] = u32::from(sprite.opacity);
        add(mapping.bounds, row);
    }
    let Some(rect) = affected else {
        return Ok(None);
    };
    let ox = rect.left as u32 / 32;
    let oy = rect.top as u32 / 32;
    let tx = (rect.left as u32 + rect.width).div_ceil(32) - ox;
    let ty = (rect.top as u32 + rect.height).div_ceil(32) - oy;
    let tiles = (tx as usize)
        .checked_mul(ty as usize)
        .ok_or(Error::Message("sprite tile overflow"))?;
    let counts_permit = gpu.staging.reserve(tiles * 4)?;
    let mut counts = vec![0u32; tiles];
    let tile_range = |r: &[u32; 16]| {
        (
            (r[1] / 32 - ox)..r[3].div_ceil(32) - ox,
            (r[2] / 32 - oy)..r[4].div_ceil(32) - oy,
        )
    };
    for r in &records {
        let (xs, ys) = tile_range(r);
        for y in ys {
            for x in xs.clone() {
                counts[(y * tx + x) as usize] += 1;
            }
        }
    }
    let indices = counts
        .iter()
        .try_fold(0usize, |a, &b| a.checked_add(b as usize))
        .ok_or(Error::Message("sprite tile index overflow"))?;
    let record_start = 16 + tiles * 2;
    let index_start = record_start + records.len() * 16;
    let words = index_start
        .checked_add(indices)
        .ok_or(Error::Message("sprite packet overflow"))?;
    let bytes = words
        .checked_mul(4)
        .ok_or(Error::Message("sprite packet overflow"))?;
    if bytes > gpu.device.limits().max_storage_buffer_binding_size as usize {
        return Err(Error::Message("sprite packet exceeds backend buffer limit"));
    }
    let permit = gpu.staging.reserve(bytes)?;
    let mut data = vec![0; words];
    let face = match options.face {
        DrawFace::Alpha => 0,
        DrawFace::Opaque => 1,
        DrawFace::AddAlpha => 4,
        DrawFace::Mask => 2,
        DrawFace::Province => 3,
    };
    data[..10].copy_from_slice(&[
        tx,
        ox,
        oy,
        record_start as u32,
        index_start as u32,
        options.mode as u32,
        face,
        u32::from(options.hold_alpha),
        rect.left as u32,
        rect.top as u32,
    ]);
    let mut offset = 0;
    for (i, count) in counts.iter_mut().enumerate() {
        data[16 + i * 2] = offset;
        offset += *count;
        data[17 + i * 2] = offset;
        *count = data[16 + i * 2];
    }
    for (i, r) in records.iter().enumerate() {
        data[record_start + i * 16..record_start + i * 16 + 16].copy_from_slice(r);
        let (xs, ys) = tile_range(r);
        for y in ys {
            for x in xs.clone() {
                let slot = &mut counts[(y * tx + x) as usize];
                data[index_start + *slot as usize] = i as u32;
                *slot += 1;
            }
        }
    }
    drop((counts_permit, records_permit));
    Ok(Some((rect, data, permit)))
}
impl Gpu {
    pub fn draw_sprites(
        &self,
        image: &mut Image,
        source: &ImageSource,
        batch: &Sprites,
        clip: Rect,
        options: BlendOptions,
    ) -> Result<()> {
        let Some(clip) = clip.intersection(image.size.rect()) else {
            return Ok(());
        };
        if options.face == DrawFace::Province {
            for r in &batch.clear {
                if let Some(rectangle) = r.intersection(clip) {
                    self.fill(
                        image,
                        &[krkr_protocol::graphics::Fill {
                            rectangle,
                            color: 0,
                            face: DrawFace::Province,
                            hold_alpha: false,
                        }],
                    )?;
                }
            }
        }
        let Some((rect, data, permit)) = pack(self, batch, source.size, clip, options)? else {
            return Ok(());
        };
        let resolved = self.materialized_source(source)?;
        let source = resolved.as_ref().unwrap_or(source);
        self.materialize(image)?;
        let plane = source
            .main
            .as_ref()
            .ok_or(Error::Message("particle atlas has no main plane"))?;
        let alias = Arc::strong_count(&image.main_owners) == 1 && Arc::ptr_eq(plane, image.main()?);
        let atlas = alias
            .then(|| self.temporary(source.size, FORMAT))
            .transpose()?;
        let previous = self.temporary(
            Size {
                width: rect.width,
                height: rect.height,
            },
            FORMAT,
        )?;
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let renderer = self.sprites.get_or_init(|| Renderer::new(&self.device));
        let buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("sprite tiles"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let mixer = self.mixer()?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let input = if let Some(atlas) = &atlas {
            crate::copy::copy(&mut encoder, plane, atlas, source.size.rect(), 0, 0);
            atlas
        } else {
            plane
        };
        crate::copy::copy(&mut encoder, &target, &previous, rect, 0, 0);
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ordered sprites"),
            layout: &renderer.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&input.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&previous.view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&mixer.lookup().view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: buffer.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ordered sprites"),
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
            (target, plane.clone(), atlas, previous, buffer, permit),
        );
        self.check()
    }
}

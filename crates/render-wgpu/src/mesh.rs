use crate::gpu::{Allocation, FORMAT, Gpu, Image};
use krkr_protocol::{
    graphics::ImageId,
    mesh::{Batch, Blend, Texture},
    pixels::Pixels,
};
use krkr_render::{Error, Result};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
};
use wgpu::util::DeviceExt;

struct Cached {
    owner: Weak<Pixels>,
    image: Image,
}
pub(crate) struct Renderer {
    layout: wgpu::BindGroupLayout,
    pipelines: [wgpu::RenderPipeline; 3],
    sampler: wgpu::Sampler,
    textures: Mutex<HashMap<usize, Cached>>,
    mask: Mutex<Option<Arc<Allocation>>>,
}
/// Logical image owners pin snapshots and force copy-on-write for aliasing.
pub struct Prepared {
    textures: Vec<Image>,
}
impl Renderer {
    fn new(device: &wgpu::Device) -> Self {
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mesh resources"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(48),
                    },
                    count: None,
                },
                texture(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(3),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("mesh.wgsl"));
        let alpha = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::SrcAlpha,
            dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
            operation: wgpu::BlendOperation::Add,
        };
        let pipelines = [
            wgpu::BlendState {
                color: alpha,
                alpha: wgpu::BlendComponent {
                    operation: wgpu::BlendOperation::Max,
                    src_factor: wgpu::BlendFactor::One,
                    dst_factor: wgpu::BlendFactor::One,
                },
            },
            wgpu::BlendState {
                color: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Dst,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
                alpha: wgpu::BlendComponent {
                    src_factor: wgpu::BlendFactor::Zero,
                    dst_factor: wgpu::BlendFactor::One,
                    operation: wgpu::BlendOperation::Add,
                },
            },
            wgpu::BlendState {
                color: alpha,
                alpha,
            },
        ]
        .map(|blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("textured mesh"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: 16,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                    })],
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        Self {
            layout,
            pipelines,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("mesh linear clamp"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            textures: Mutex::new(HashMap::new()),
            mask: Mutex::new(None),
        }
    }
}
impl Gpu {
    /// Admission estimate excludes resident immutable assets, so a warm frame
    /// does not evict unrelated image caches to make room for fictitious uploads.
    pub fn mesh_upload_bytes(&self, batch: &Batch) -> usize {
        let renderer = self.meshes.get_or_init(|| Renderer::new(&self.device));
        let cache = renderer.textures.lock().unwrap();
        let mut seen = std::collections::HashSet::new();
        batch.draws.iter().fold(0usize, |bytes, draw| {
            let Texture::Pixels(pixels) = &draw.texture else {
                return bytes;
            };
            let key = Arc::as_ptr(pixels) as usize;
            if !seen.insert(key)
                || cache
                    .get(&key)
                    .is_some_and(|e| e.owner.upgrade().is_some_and(|p| Arc::ptr_eq(&p, pixels)))
            {
                return bytes;
            }
            bytes.saturating_add(pixels.size.rgba_bytes().unwrap_or(usize::MAX))
        })
    }
    pub fn collect_mesh_textures(&self) {
        if let Some(renderer) = self.meshes.get() {
            renderer
                .textures
                .lock()
                .unwrap()
                .retain(|_, entry| entry.owner.strong_count() > 0);
        }
    }
    pub fn prepare_meshes(
        &self,
        batch: &Batch,
        images: &HashMap<ImageId, Image>,
    ) -> Result<Prepared> {
        let renderer = self.meshes.get_or_init(|| Renderer::new(&self.device));
        let mut cache = renderer.textures.lock().unwrap();
        cache.retain(|_, entry| entry.owner.strong_count() > 0);
        let mut textures = Vec::with_capacity(batch.draws.len());
        for draw in &batch.draws {
            let geometry = &draw.geometry;
            if !geometry.indices.len().is_multiple_of(3)
                || geometry
                    .indices
                    .iter()
                    .any(|&i| usize::from(i) >= geometry.vertices.len())
                || geometry
                    .vertices
                    .iter()
                    .any(|v| v.position.iter().chain(&v.uv).any(|x| !x.is_finite()))
                || !draw.opacity.is_finite()
                || draw.color.iter().any(|x| !x.is_finite())
                || draw.masks.iter().any(|&i| i >= batch.draws.len())
            {
                return Err(Error::Message("invalid textured mesh"));
            }
            let mut image = match &draw.texture {
                Texture::Image(image) => images
                    .get(&image.id)
                    .ok_or(Error::Message("mesh texture released"))?
                    .shared(),
                Texture::Pixels(pixels) => {
                    let key = Arc::as_ptr(pixels) as usize;
                    if !cache
                        .get(&key)
                        .is_some_and(|e| e.owner.upgrade().is_some_and(|p| Arc::ptr_eq(&p, pixels)))
                    {
                        let image = self.assign_bitmap(None, pixels)?;
                        cache.insert(
                            key,
                            Cached {
                                owner: Arc::downgrade(pixels),
                                image,
                            },
                        );
                    }
                    cache[&key].image.shared()
                }
            };
            self.materialize(&mut image)?;
            image.main()?;
            textures.push(image);
        }
        if batch.order.iter().any(|&i| i >= batch.draws.len())
            || batch
                .clear
                .is_some_and(|c| c.iter().any(|v| !v.is_finite()))
        {
            return Err(Error::Message("invalid mesh batch order or clear color"));
        }
        Ok(Prepared { textures })
    }
    pub fn draw_meshes(&self, image: &mut Image, batch: &Batch, prepared: Prepared) -> Result<()> {
        self.materialize(image)?;
        if prepared.textures.len() != batch.draws.len() {
            return Err(Error::Message("mesh resource count mismatch"));
        }
        let renderer = self.meshes.get_or_init(|| Renderer::new(&self.device));
        let alignment = u64::from(self.device.limits().min_uniform_buffer_offset_alignment);
        let stride = 48u64.div_ceil(alignment) * alignment;
        let vertex_count = batch
            .draws
            .iter()
            .map(|d| d.geometry.vertices.len())
            .sum::<usize>();
        let index_count = batch
            .draws
            .iter()
            .map(|d| d.geometry.indices.len())
            .sum::<usize>();
        let vertex_bytes = vertex_count
            .checked_mul(16)
            .ok_or(Error::Message("mesh buffer overflow"))?;
        let index_bytes = index_count
            .checked_mul(2)
            .and_then(|n| n.checked_add(3))
            .map(|n| n & !3)
            .ok_or(Error::Message("mesh buffer overflow"))?;
        let uniform_bytes = (batch.draws.len() as u64)
            .checked_mul(stride * 2)
            .ok_or(Error::Message("mesh buffer overflow"))?;
        if uniform_bytes > u64::from(u32::MAX)
            || [vertex_bytes as u64, index_bytes as u64, uniform_bytes]
                .iter()
                .any(|&n| n > self.device.limits().max_buffer_size)
        {
            return Err(Error::Message("mesh exceeds backend buffer limits"));
        }
        let bytes = vertex_bytes
            .saturating_add(index_bytes)
            .saturating_add(uniform_bytes as usize)
            .max(4);
        let permit = self.staging.reserve(bytes.saturating_mul(2))?;
        let mut vertices = Vec::<[f32; 4]>::with_capacity(vertex_count);
        let mut indices = Vec::<u16>::with_capacity(index_bytes / 2);
        let mut uniforms = vec![0f32; uniform_bytes as usize / 4];
        let mut ranges = Vec::with_capacity(batch.draws.len());
        for (i, draw) in batch.draws.iter().enumerate() {
            let base = vertices.len() as i32;
            let first = indices.len() as u32;
            vertices.extend(
                draw.geometry
                    .vertices
                    .iter()
                    .map(|v| [v.position[0], v.position[1], v.uv[0], v.uv[1]]),
            );
            indices.extend_from_slice(&draw.geometry.indices);
            ranges.push((base, first..indices.len() as u32));
            for masked in 0..2 {
                let offset = (i * 2 + masked) * stride as usize / 4;
                uniforms[offset..offset + 4].copy_from_slice(&draw.color);
                uniforms[offset + 4..offset + 8].copy_from_slice(&[
                    draw.opacity,
                    u8::from(draw.solid_color) as f32,
                    masked as f32,
                    u8::from(matches!(draw.blend, Blend::LayerAlpha)) as f32,
                ]);
                let texture = &prepared.textures[i];
                let allocation = texture.main()?;
                uniforms[offset + 8..offset + 12].copy_from_slice(&[
                    texture.size.width as f32,
                    texture.size.height as f32,
                    allocation.texture.width() as f32,
                    allocation.texture.height() as f32,
                ]);
            }
        }
        indices.resize(index_bytes / 2, 0);
        let buffer = |label, contents: &[u8], usage| {
            self.device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some(label),
                    contents: if contents.is_empty() {
                        &[0; 4]
                    } else {
                        contents
                    },
                    usage,
                })
        };
        let vertex_buffer = buffer(
            "mesh vertices",
            bytemuck::cast_slice(&vertices),
            wgpu::BufferUsages::VERTEX,
        );
        let index_buffer = buffer(
            "mesh indices",
            bytemuck::cast_slice(&indices),
            wgpu::BufferUsages::INDEX,
        );
        let uniform_buffer = buffer(
            "mesh parameters",
            bytemuck::cast_slice(&uniforms),
            wgpu::BufferUsages::UNIFORM,
        );
        let has_mask = batch.order.iter().any(|&i| {
            batch.draws[i].visible
                && batch.draws[i]
                    .masks
                    .iter()
                    .any(|&m| batch.draws[m].opacity > 0.)
        });
        // One scratch target for the batch, reused after each masked group.
        let mask = if has_mask {
            let mut cached = renderer.mask.lock().unwrap();
            if !cached.as_ref().is_some_and(|a| {
                a.texture.width() == image.size.width && a.texture.height() == image.size.height
            }) {
                cached.take();
                *cached = Some(self.allocation(image.size, FORMAT, &self.scratch)?);
            }
            // Each command buffer includes mask construction and consumption.
            // Queue order permits reuse even while earlier frames are in flight.
            cached.clone()
        } else {
            renderer.mask.lock().unwrap().take();
            None
        };
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let groups = prepared
            .textures
            .iter()
            .map(|texture| {
                let source = texture.main().expect("validated mesh texture");
                [false, true].map(|masked| {
                    self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("mesh draw"),
                        layout: &renderer.layout,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer: &uniform_buffer,
                                    offset: 0,
                                    size: wgpu::BufferSize::new(48),
                                }),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&source.view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: wgpu::BindingResource::Sampler(&renderer.sampler),
                            },
                            wgpu::BindGroupEntry {
                                binding: 3,
                                resource: wgpu::BindingResource::TextureView(
                                    &if masked {
                                        mask.as_ref().unwrap_or(source)
                                    } else {
                                        source
                                    }
                                    .view,
                                ),
                            },
                        ],
                    })
                })
            })
            .collect::<Vec<_>>();
        let state = DrawState {
            renderer,
            batch,
            groups: &groups,
            ranges: &ranges,
            stride,
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        if let Some(color) = batch.clear {
            self.clear(&mut encoder, &target, color);
        }
        // Keep a single render pass for consecutive unmasked draws or draws
        // using the same mask. Only a changed mask requires a target switch.
        let mut cursor = 0;
        while cursor < batch.order.len() {
            let draw = &batch.draws[batch.order[cursor]];
            if !draw.visible {
                cursor += 1;
                continue;
            }
            let masked = draw.masks.iter().any(|&m| batch.draws[m].opacity > 0.);
            if masked {
                let target = mask.as_ref().expect("allocated mesh mask");
                self.clear(&mut encoder, target, [0.; 4]);
                let mut pass = mesh_pass(&mut encoder, target);
                pass.set_vertex_buffer(0, vertex_buffer.slice(..));
                pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                for &index in &draw.masks {
                    if batch.draws[index].visible && batch.draws[index].opacity > 0. {
                        state.issue(&mut pass, index, false);
                    }
                }
            }
            let masks = &draw.masks;
            let mut pass = mesh_pass(&mut encoder, &target);
            pass.set_viewport(
                0.0,
                0.0,
                image.size.width as f32,
                image.size.height as f32,
                0.0,
                1.0,
            );
            pass.set_scissor_rect(0, 0, image.size.width, image.size.height);
            pass.set_vertex_buffer(0, vertex_buffer.slice(..));
            pass.set_index_buffer(index_buffer.slice(..), wgpu::IndexFormat::Uint16);
            while cursor < batch.order.len() {
                let index = batch.order[cursor];
                let next = &batch.draws[index];
                if !next.visible {
                    cursor += 1;
                    continue;
                }
                let next_masked = next.masks.iter().any(|&m| batch.draws[m].opacity > 0.);
                if next_masked != masked || (masked && next.masks != *masks) {
                    break;
                }
                state.issue(&mut pass, index, masked);
                cursor += 1;
            }
        }
        self.submit(encoder, (target, mask, prepared.textures, permit));
        self.check()
    }
}
fn mesh_pass<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    target: &Allocation,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("mesh triangles"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &target.view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}
struct DrawState<'a> {
    renderer: &'a Renderer,
    batch: &'a Batch,
    groups: &'a [[wgpu::BindGroup; 2]],
    ranges: &'a [(i32, std::ops::Range<u32>)],
    stride: u64,
}
impl DrawState<'_> {
    fn issue(&self, pass: &mut wgpu::RenderPass<'_>, index: usize, masked: bool) {
        let pipeline = match self.batch.draws[index].blend {
            Blend::AlphaMax => 0,
            Blend::MultiplyAdd => 1,
            Blend::Alpha | Blend::LayerAlpha => 2,
        };
        pass.set_pipeline(&self.renderer.pipelines[pipeline]);
        pass.set_bind_group(
            0,
            &self.groups[index][usize::from(masked)],
            &[((index * 2 + usize::from(masked)) as u64 * self.stride) as u32],
        );
        pass.draw_indexed(self.ranges[index].1.clone(), self.ranges[index].0, 0..1);
    }
}

#[cfg(test)]
#[path = "mesh_test.rs"]
mod tests;

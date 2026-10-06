use krkr_render::overlay::{Counter, SIZE};
use krkr_render_wgpu::Gpu;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Configure the display before starting the desktop event loop.
pub fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub(super) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub(super) struct Overlay {
    pub counter: Counter,
    texture: wgpu::Texture,
    bindings: wgpu::BindGroup,
    pipeline: wgpu::RenderPipeline,
    _permit: krkr_protocol::budget::Permit,
}
impl Overlay {
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Result<Self, String> {
        let permit = gpu
            .resident
            .reserve(SIZE.rgba_bytes().unwrap())
            .map_err(|e| e.to_string())?;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("statistics overlay"),
            size: wgpu::Extent3d {
                width: SIZE.width,
                height: SIZE.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("statistics overlay"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
        let bindings = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("statistics overlay"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(
                    &texture.create_view(&Default::default()),
                ),
            }],
        });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("statistics overlay"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::include_wgsl!("overlay.wgsl"));
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("statistics overlay"),
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
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            });
        Ok(Self {
            counter: Counter::new(Instant::now()),
            texture,
            bindings,
            pipeline,
            _permit: permit,
        })
    }
    pub fn refresh(&mut self, gpu: &Gpu, graphics_bytes: usize, now: Instant) -> bool {
        let Some(fps) = self.counter.sample(now) else {
            return false;
        };
        let data = krkr_render::overlay::pixels(
            fps,
            memory_stats::memory_stats().map(|m| m.physical_mem),
            false,
            graphics_bytes,
        );
        gpu.queue.write_texture(
            self.texture.as_image_copy(),
            &data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(SIZE.width * 4),
                rows_per_image: Some(SIZE.height),
            },
            self.texture.size(),
        );
        true
    }
    pub fn draw(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        size: krkr_render::Size,
    ) {
        if size.width <= 16 || size.height <= 16 {
            return;
        }
        let scale = ((size.width - 16) as f32 / SIZE.width as f32)
            .min((size.height - 16) as f32 / SIZE.height as f32)
            .min(1.0);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_viewport(
                8.0,
                8.0,
                SIZE.width as f32 * scale,
                SIZE.height as f32 * scale,
                0.0,
                1.0,
            );
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bindings, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_draws_only_on_the_presented_target_and_samples_once_per_second() {
        let gpu = pollster::block_on(Gpu::new(
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env()),
            None,
        ))
        .unwrap();
        let size = krkr_render::Size {
            width: 320,
            height: 96,
        };
        let canvas = gpu.create_image(size, 0xff336699).unwrap();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("overlay test"),
            size: wgpu::Extent3d {
                width: size.width,
                height: size.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = target.create_view(&Default::default());
        let blitter = wgpu::util::TextureBlitter::new(&gpu.device, format);
        let mut overlay = Overlay::new(&gpu, format).unwrap();
        let now = Instant::now();
        assert!(overlay.refresh(&gpu, 4 << 20, now));
        assert!(!overlay.refresh(&gpu, 4 << 20, now));
        gpu.present_to_with(&canvas, &blitter, &view, |encoder| {
            overlay.draw(encoder, &view, size)
        })
        .unwrap();
        let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(size.width * size.height * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            target.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(size.width * 4),
                    rows_per_image: Some(size.height),
                },
            },
            target.size(),
        );
        gpu.queue.submit([encoder.finish()]);
        let (send, receive) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                send.send(result).unwrap();
            });
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .unwrap();
        receive.recv().unwrap().unwrap();
        let data = buffer.slice(..).get_mapped_range().unwrap();
        assert_eq!(&data[..4], &[0x33, 0x66, 0x99, 255]);
        let background = (10 * size.width as usize + 10) * 4;
        assert_ne!(&data[background..background + 4], &[0x33, 0x66, 0x99, 255]);
        assert!(data.as_chunks::<4>().0.contains(&[118, 222, 255, 255]));
        drop(data);
        buffer.unmap();
        // Presentation must never burn the host overlay into the script image.
        let mut read = gpu.readback(&canvas, size.rect(), false).unwrap();
        gpu.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(10)),
            })
            .unwrap();
        let pixels = read.take().unwrap().unwrap();
        assert!(
            pixels
                .data
                .as_slice()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0x33, 0x66, 0x99, 255])
        );
    }
}

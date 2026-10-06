//! Kirikiri box blur: clipped output, full-image neighbourhood, one final
//! average. Sliding-sum compute passes reuse a bounded column strip. The
//! immutable source preserves neighbours across strips without changing pixels.
use crate::{Gpu, Image};
use krkr_protocol::graphics::Rect;
use krkr_render::{Error, Result};
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

pub(crate) struct Blur {
    horizontal: wgpu::ComputePipeline,
    vertical: wgpu::ComputePipeline,
    sums: Mutex<Option<Arc<Sums>>>,
}
struct Sums {
    buffer: wgpu::Buffer,
    _permit: krkr_render::budget::Permit,
}
impl Blur {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("box_blur.wgsl"));
        let pipeline = |entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &shader,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        Self {
            horizontal: pipeline("horizontal"),
            vertical: pipeline("vertical"),
            sums: Mutex::new(None),
        }
    }
    pub(crate) fn trim(&self) {
        let mut sums = self.sums.lock().unwrap();
        if sums.as_ref().is_some_and(|s| Arc::strong_count(s) == 1) {
            *sums = None;
        }
    }
    fn sums(&self, gpu: &Gpu, bytes: u64) -> Result<Arc<Sums>> {
        {
            let mut sums = self.sums.lock().unwrap();
            if let Some(sums) = sums.as_ref().filter(|s| s.buffer.size() >= bytes) {
                // These internal passes all use one ordered queue. The next
                // horizontal pass can reuse storage after the previous vertical
                // pass without waiting for its CPU completion callback.
                return Ok(sums.clone());
            }
            *sums = None;
        }
        if bytes > gpu.scratch.available() as u64 {
            gpu.trim_scratch();
        }
        let permit = gpu.scratch.reserve(bytes as usize).map_err(|error| {
            Error::Backend(format!(
                "{error}: box blur sums, requested={bytes}, scratch={}/{}",
                gpu.scratch.used(),
                gpu.scratch.limit()
            ))
        })?;
        let sums = Arc::new(Sums {
            buffer: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("box blur integer sums"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }),
            _permit: permit,
        });
        *self.sums.lock().unwrap() = Some(sums.clone());
        Ok(sums)
    }
}
impl Gpu {
    pub(crate) fn box_blur(
        &self,
        image: &mut Image,
        rectangle: Rect,
        radius: [u32; 2],
        alpha: bool,
    ) -> Result<()> {
        image.main()?;
        let Some(rect) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        let area = (u64::from(radius[0]) * 2 + 1)
            .checked_mul(u64::from(radius[1]) * 2 + 1)
            .filter(|area| *area < 1 << 24)
            .ok_or(Error::Message(
                "box blur area must be smaller than 16 million pixels",
            ))?;
        if radius == [0, 0] {
            return Ok(());
        }
        let radius = [
            radius[0].min(image.size.width - 1),
            radius[1].min(image.size.height - 1),
        ];
        let top = (rect.top as u32).saturating_sub(radius[1]);
        let bottom = (rect.top as u32 + rect.height)
            .saturating_add(radius[1])
            .min(image.size.height);
        let rows = bottom - top;
        // A full RGBA u32 intermediate costs four times the source texture.
        // Keep at most 256 columns, and preserve the source during all strips.
        let bounded_width = rect.width.min(256);
        let saved_sums = u64::from(rect.width - bounded_width) * u64::from(rows) * 16;
        let extra_copy = if image.main_write_bytes() == 0 {
            image.size.rgba_bytes().unwrap() as u64
        } else {
            0
        };
        // A tiny clipped region in a large unique image can be cheaper in place.
        let strip_width = if saved_sums > extra_copy {
            bounded_width
        } else {
            rect.width
        };
        let bytes = u64::from(strip_width) * u64::from(rows) * 16;
        if bytes > self.device.limits().max_storage_buffer_binding_size {
            return Err(Error::Message(
                "box blur intermediate exceeds GPU buffer limits",
            ));
        }
        self.poll()?;
        let blur = self.box_blur.get_or_init(|| Blur::new(&self.device));
        let sums = blur.sums(self, bytes)?;
        // Image ownership, rather than Texture's Arc count, controls COW.
        // Retaining the original Image forces one destination allocation for a
        // striped blur. A single strip can still use the existing in-place path.
        let original = (strip_width < rect.width).then(|| image.shared_main());
        self.independent(image, true, false)?;
        let target = image.main()?.clone();
        let source = original
            .as_ref()
            .map_or(Ok(&target), |image| image.main())?;
        for offset in (0..rect.width).step_by(strip_width as usize) {
            let left = rect.left as u32 + offset;
            let width = strip_width.min(rect.width - offset);
            let parameters = [
                [left, rect.top as u32, width, rect.height],
                [image.size.width, image.size.height, radius[0], radius[1]],
                [top, rows, u32::from(alpha), u32::from(area < 256)],
            ];
            let uniforms_permit = self.staging.reserve(48)?;
            let uniforms = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("box blur parameters"),
                    contents: bytemuck::cast_slice(&parameters),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let horizontal = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("box blur horizontal"),
                layout: &blur.horizontal.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&source.view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: sums.buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniforms.as_entire_binding(),
                    },
                ],
            });
            let vertical = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("box blur vertical"),
                layout: &blur.vertical.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: sums.buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&target.view),
                    },
                ],
            });
            let mut encoder = self.device.create_command_encoder(&Default::default());
            for (pipeline, bindings, count) in [
                (&blur.horizontal, &horizontal, rows),
                (&blur.vertical, &vertical, width),
            ] {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bindings, &[]);
                pass.dispatch_workgroups(count.div_ceil(64), 1, 1);
            }
            self.submit(
                encoder,
                (
                    target.clone(),
                    source.clone(),
                    sums.clone(),
                    uniforms,
                    uniforms_permit,
                ),
            );
        }
        self.check()
    }
}

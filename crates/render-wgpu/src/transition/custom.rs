use super::*;
use krkr_protocol::{pixels::Bytes, transition::custom::Frame as CustomFrame};
use std::{
    collections::HashMap,
    sync::{Mutex, Weak},
};

struct Kernel {
    source: Arc<str>,
    pipeline: std::sync::OnceLock<wgpu::RenderPipeline>,
}
struct Table {
    source: Weak<Bytes>,
    buffer: wgpu::Buffer,
    _permit: krkr_protocol::budget::Permit,
}
pub(super) struct Registry {
    layout: wgpu::BindGroupLayout,
    kernels: Mutex<HashMap<&'static str, Arc<Kernel>>>,
    tables: Mutex<HashMap<usize, Arc<Table>>>,
}
pub(crate) struct Resources {
    _table: Arc<Table>,
    _source: Arc<Bytes>,
    _frame: CustomFrame,
    _permit: krkr_protocol::budget::Permit,
    _upload: Option<krkr_protocol::budget::Permit>,
}
impl Registry {
    pub fn new(gpu: &Gpu, mut entries: Vec<wgpu::BindGroupLayoutEntry>) -> Self {
        entries[4].ty = wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: wgpu::BufferSize::new(112),
        };
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: 5,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: wgpu::BufferSize::new(4),
            },
            count: None,
        });
        Self {
            layout: gpu
                .device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("custom transition"),
                    entries: &entries,
                }),
            kernels: Mutex::new(HashMap::from([
                (
                    "krkr.extrans.v1",
                    Arc::new(Kernel {
                        source: include_str!("extrans.wgsl").into(),
                        pipeline: Default::default(),
                    }),
                ),
                (
                    "krkr.nagano.v1",
                    Arc::new(Kernel {
                        source: include_str!("nagano.wgsl").into(),
                        pipeline: Default::default(),
                    }),
                ),
            ])),
            tables: Default::default(),
        }
    }
    pub fn collect(&self) {
        self.tables
            .lock()
            .unwrap()
            .retain(|_, table| table.source.strong_count() != 0);
    }
    pub fn draw(
        &self,
        gpu: &Gpu,
        encoder: &mut wgpu::CommandEncoder,
        target: &Allocation,
        inputs: [&Allocation; 3],
        frame: Frame,
        custom: &CustomFrame,
    ) -> Result<DrawResources> {
        let kernel = self
            .kernels
            .lock()
            .unwrap()
            .get(custom.instance.kernel())
            .cloned()
            .ok_or(Error::Message("custom transition kernel is not registered"))?;
        let payload = custom
            .instance
            .prepare(frame.size, custom.elapsed, custom.duration, &gpu.staging)
            .map_err(Error::Backend)?;
        let bytes = payload.table.as_slice();
        if bytes.is_empty()
            || !bytes.len().is_multiple_of(4)
            || bytes.len() > gpu.device.limits().max_storage_buffer_binding_size as usize
        {
            return Err(Error::Message("invalid transition table size"));
        }
        self.collect();
        let key = Arc::as_ptr(&payload.table) as usize;
        let (table, upload) = {
            let mut tables = self.tables.lock().unwrap();
            if let Some(table) = tables.get(&key) {
                (table.clone(), None)
            } else {
                let permit = gpu.resident.reserve(bytes.len())?;
                let upload = gpu.staging.reserve(bytes.len())?;
                let buffer = gpu
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("transition table"),
                        contents: bytes,
                        usage: wgpu::BufferUsages::STORAGE,
                    });
                let table = Arc::new(Table {
                    source: Arc::downgrade(&payload.table),
                    buffer,
                    _permit: permit,
                });
                tables.insert(key, table.clone());
                (table, Some(upload))
            }
        };
        let pipeline = kernel.pipeline.get_or_init(|| {
            let layout = gpu
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("custom transition"),
                    bind_group_layouts: &[Some(&self.layout)],
                    immediate_size: 0,
                });
            let shader = gpu
                .device
                .create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(custom.instance.kernel()),
                    source: wgpu::ShaderSource::Wgsl(kernel.source.to_string().into()),
                });
            gpu.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(custom.instance.kernel()),
                    layout: Some(&layout),
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
                })
        });
        let permit = gpu.staging.reserve(112)?;
        let face = match frame.face {
            DrawFace::Alpha => 0,
            DrawFace::AddAlpha => 4,
            _ => 1,
        };
        let mut parameters = [0u32; 28];
        parameters[..12].copy_from_slice(&[
            0,
            frame.phase,
            u32::MAX,
            0,
            face,
            0,
            0,
            0,
            frame.size.width,
            frame.size.height,
            0,
            0,
        ]);
        parameters[12..].copy_from_slice(&payload.parameters);
        let buffer = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("custom transition frame"),
                contents: bytemuck::cast_slice(&parameters),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let lookup = gpu.mixer()?.lookup();
        let mut entries: Vec<_> = inputs
            .into_iter()
            .chain([lookup])
            .enumerate()
            .map(|(i, image)| wgpu::BindGroupEntry {
                binding: i as u32,
                resource: wgpu::BindingResource::TextureView(&image.view),
            })
            .collect();
        entries.push(wgpu::BindGroupEntry {
            binding: 4,
            resource: buffer.as_entire_binding(),
        });
        entries.push(wgpu::BindGroupEntry {
            binding: 5,
            resource: table.buffer.as_entire_binding(),
        });
        let bind = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("custom transition frame"),
            layout: &self.layout,
            entries: &entries,
        });
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("custom transition"),
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
        pass.set_pipeline(pipeline);
        pass.set_scissor_rect(0, 0, frame.size.width, frame.size.height);
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
        drop(pass);
        Ok(DrawResources {
            _buffer: buffer,
            _custom: Some(Resources {
                _table: table,
                _source: payload.table,
                _frame: custom.clone(),
                _permit: permit,
                _upload: upload,
            }),
        })
    }
}
impl Gpu {
    pub fn transition_kernels(&self) -> Vec<String> {
        self.transitions()
            .custom
            .kernels
            .lock()
            .unwrap()
            .keys()
            .map(|s| (*s).to_owned())
            .collect()
    }
    /// Host API. The shader follows the transition layout (four textures,
    /// 112-byte uniform, read-only u32 table). No script supplies shader code.
    pub fn register_transition_kernel(&self, name: &'static str, source: Arc<str>) -> Result<()> {
        let mut kernels = self.transitions().custom.kernels.lock().unwrap();
        if name.is_empty() || kernels.contains_key(name) {
            return Err(Error::Message("duplicate or empty transition kernel"));
        }
        kernels.insert(
            name,
            Arc::new(Kernel {
                source,
                pipeline: Default::default(),
            }),
        );
        Ok(())
    }
}

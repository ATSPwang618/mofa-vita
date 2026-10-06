use krkr_protocol::graphics::Size;
use krkr_render::{
    Error, Result,
    budget::{Budget, Permit},
};
use std::sync::{Arc, Mutex};

pub(crate) const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
pub(crate) struct Allocation {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub(crate) permit: Arc<Permit>,
    // Changes at the GPU write boundary, rather than when a future VM request
    // is queued. Cache signatures observe this without becoming image owners.
    pub(crate) generation: std::sync::atomic::AtomicU64,
}
impl Allocation {
    pub(crate) fn changed(&self) {
        self.generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}
pub struct Image {
    pub(crate) main: Option<Arc<Allocation>>,
    pub(crate) deferred: Option<Arc<crate::deferred::Deferred>>,
    pub(crate) province: Option<Arc<Allocation>>,
    pub size: Size,
    pub(crate) main_owners: Arc<()>,
    pub(crate) province_owners: Arc<()>,
}
impl Image {
    pub(crate) fn new(
        main: Option<Arc<Allocation>>,
        province: Option<Arc<Allocation>>,
        size: Size,
    ) -> Self {
        Self {
            main,
            deferred: None,
            province,
            size,
            main_owners: Arc::new(()),
            province_owners: Arc::new(()),
        }
    }
    /// Logical image owners are counted separately from GPU submissions and
    /// read dependencies, which pin allocations but must not trigger COW.
    pub(crate) fn main(&self) -> Result<&Arc<Allocation>> {
        self.main
            .as_ref()
            .ok_or(Error::Message("image has no main plane"))
    }
    pub fn shared(&self) -> Self {
        Self {
            main: self.main.clone(),
            deferred: self.deferred.clone(),
            province: self.province.clone(),
            size: self.size,
            main_owners: self.main_owners.clone(),
            province_owners: self.province_owners.clone(),
        }
    }
    pub fn shared_main(&self) -> Self {
        let mut image = Self::new(self.main.clone(), None, self.size);
        image.deferred = self.deferred.clone();
        image.main_owners = self.main_owners.clone();
        image
    }
    /// Resident bytes required to detach a logically shared main plane.
    pub fn main_write_bytes(&self) -> usize {
        if self.deferred.is_some() || Arc::strong_count(&self.main_owners) > 1 {
            self.size.rgba_bytes().unwrap_or(usize::MAX)
        } else {
            0
        }
    }
    pub fn province_write_bytes(&self) -> usize {
        if self.province.is_none() || Arc::strong_count(&self.province_owners) > 1 {
            self.size.rgba_bytes().unwrap_or(usize::MAX) / 4
        } else {
            0
        }
    }
}
pub struct Gpu {
    pub(crate) generated: Mutex<Vec<std::sync::Weak<crate::deferred::Deferred>>>,
    pub(crate) fill_colors: Mutex<crate::fill::Colors>,
    pub(crate) fill_batch: Mutex<crate::fill::Batch>,
    pub(crate) box_blur: std::sync::OnceLock<crate::box_blur::Blur>,
    pub(crate) meshes: std::sync::OnceLock<crate::mesh::Renderer>,
    pub(crate) video_copier: std::sync::OnceLock<crate::video::Copier>,
    pub(crate) projector: std::sync::OnceLock<crate::perspective::Projector>,
    pub(crate) adjuster: std::sync::OnceLock<crate::adjust::Adjuster>,
    pub(crate) lines: std::sync::OnceLock<crate::lines::Renderer>,
    pub(crate) sprites: std::sync::OnceLock<crate::sprites::Renderer>,
    pub(crate) scanlines: std::sync::OnceLock<crate::scanlines::Renderer>,
    pub(crate) warp: std::sync::OnceLock<crate::warp::Renderer>,
    pub(crate) filter: std::sync::OnceLock<crate::filter::Processor>,
    pub(crate) filter_axes: Mutex<krkr_render::resample::Cache>,
    pub(crate) transitions: std::sync::OnceLock<crate::transition::Transitions>,
    pub(crate) text: Mutex<crate::text::Atlas>,
    pub(crate) mixer: std::sync::OnceLock<crate::blend::Mixer>,
    pub(crate) copier: crate::copy::Copier,
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub resident: Budget,
    pub scratch: Budget,
    pub staging: Budget,
    pub(crate) fill_layout: wgpu::BindGroupLayout,
    pub(crate) fill_pipelines: [wgpu::RenderPipeline; 4],
    failure: Arc<Mutex<Option<String>>>,
    scratch_pool: Mutex<Vec<Arc<Allocation>>>,
    small_images: Mutex<Vec<(u32, Image)>>,
    resident_pool: Mutex<crate::texture_pool::TexturePool>,
    pub(crate) scene_cache: Mutex<crate::scene_cache::Cache>,
    in_flight: Arc<std::sync::atomic::AtomicUsize>,
}
impl Gpu {
    pub async fn new(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: surface,
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("krkr desktop GPU"),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| Error::Backend(e.to_string()))?;
        let failure = Arc::new(Mutex::new(None));
        let captured = failure.clone();
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            captured
                .lock()
                .unwrap()
                .get_or_insert_with(|| error.to_string());
        }));
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let fill_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("fill color"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(16),
                },
                count: None,
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("fill layout"),
            bind_group_layouts: &[Some(&fill_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("fill.wgsl"));
        let fill_pipelines = [
            wgpu::ColorWrites::ALL,
            wgpu::ColorWrites::RED | wgpu::ColorWrites::GREEN | wgpu::ColorWrites::BLUE,
            wgpu::ColorWrites::ALPHA,
            wgpu::ColorWrites::RED,
        ]
        .map(|mask| {
            let format = if mask == wgpu::ColorWrites::RED {
                wgpu::TextureFormat::R8Unorm
            } else {
                FORMAT
            };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("fill"),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: mask,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        let copier = crate::copy::Copier::new(&device);
        if let Some(error) = scope.pop().await {
            return Err(Error::Backend(error.to_string()));
        }
        Ok(Self {
            generated: Mutex::new(Vec::new()),
            fill_colors: Mutex::new(Default::default()),
            fill_batch: Mutex::new(Default::default()),
            meshes: Default::default(),
            projector: Default::default(),
            adjuster: std::sync::OnceLock::new(),
            lines: std::sync::OnceLock::new(),
            sprites: std::sync::OnceLock::new(),
            scanlines: std::sync::OnceLock::new(),
            warp: std::sync::OnceLock::new(),
            video_copier: std::sync::OnceLock::new(),
            filter: std::sync::OnceLock::new(),
            filter_axes: Default::default(),
            transitions: Default::default(),
            instance,
            adapter,
            device,
            queue,
            mixer: std::sync::OnceLock::new(),
            text: Mutex::new(Default::default()),
            resident: Budget::new(64 * 1024 * 1024),
            scratch: Budget::new(12 * 1024 * 1024),
            staging: Budget::new(32 * 1024 * 1024),
            fill_layout,
            fill_pipelines,
            copier,
            failure,
            scratch_pool: Mutex::new(Vec::new()),
            small_images: Mutex::new(Vec::new()),
            resident_pool: Mutex::new(crate::texture_pool::TexturePool::default()),
            scene_cache: Mutex::default(),
            in_flight: Default::default(),
            box_blur: std::sync::OnceLock::new(),
        })
    }
    pub fn poll(&self) -> Result<()> {
        self.scene_cache.lock().unwrap().trim();
        self.collect_mesh_textures();
        self.device
            .poll(wgpu::PollType::Poll)
            .map_err(|e| Error::Backend(e.to_string()))?;
        if let Some(transitions) = self.transitions.get() {
            transitions.collect();
        }
        self.check()
    }
    pub fn has_pending_work(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed) != 0
    }
    pub(crate) fn check(&self) -> Result<()> {
        match &*self.failure.lock().unwrap() {
            Some(error) => Err(Error::Backend(error.clone())),
            None => Ok(()),
        }
    }
    pub(crate) fn allocation(
        &self,
        size: Size,
        format: wgpu::TextureFormat,
        budget: &Budget,
    ) -> Result<Arc<Allocation>> {
        self.check_image_size(size)?;
        let bytes = size
            .rgba_bytes()
            .ok_or(Error::Message("image byte size overflow"))?;
        let bytes = if format == wgpu::TextureFormat::R8Unorm {
            bytes / 4
        } else {
            bytes
        };
        if std::ptr::eq(budget, &self.resident) && format == FORMAT {
            let cached = self.resident_pool.lock().unwrap().find(size, format);
            if let Some(image) = cached {
                // Match new wgpu storage's zero initialization, including
                // untouched pixels of partial uploads.
                image.changed();
                let mut encoder = self.device.create_command_encoder(&Default::default());
                self.clear(&mut encoder, &image, [0.0; 4]);
                self.submit(encoder, image.clone());
                self.check()?;
                return Ok(image);
            }
        }
        let mut reservation = budget.reserve(bytes);
        if reservation.is_err() {
            // Optional small uniform images share resident allocations. They
            // are the first local owners to release under memory pressure.
            self.small_images.lock().unwrap().clear();
            self.scene_cache.lock().unwrap().clear();
            self.trim_generated();
            self.trim_resident_pool();
            if std::ptr::eq(budget, &self.resident) {
                // Desktop pools can share a total ceiling. Idle scratch must
                // not prevent an authoritative image from using that capacity.
                self.trim_scratch();
            }
            reservation = budget.reserve(bytes);
        }
        if reservation.is_err() && bytes <= budget.limit() {
            // Old resize/COW versions may only be held by submitted work. A
            // nonblocking poll does not guarantee that its permits are freed.
            // Apply bounded backpressure only on allocation pressure; never
            // replay a partially executed graphics operation or ignore its limit.
            self.flush_fills();
            self.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_millis(100)),
                })
                .map_err(|e| Error::Backend(format!("GPU allocation wait: {e}")))?;
            self.check()?;
            self.trim_resident_pool();
            if std::ptr::eq(budget, &self.resident) {
                self.trim_scratch();
            }
            reservation = budget.reserve(bytes);
        }
        let permit = reservation.map_err(|error| {
            let pool = if std::ptr::eq(budget, &self.resident) {
                "resident"
            } else {
                "scratch"
            };
            Error::Backend(format!(
                "{error}: {pool} texture {}x{} {format:?}, requested={bytes}, used={}, limit={}, available={}",
                size.width,
                size.height,
                budget.used(),
                budget.limit(),
                budget.available()
            ))
        })?;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("layer image"),
            size: extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | if format == FORMAT {
                    wgpu::TextureUsages::STORAGE_BINDING
                } else {
                    wgpu::TextureUsages::empty()
                },
            view_formats: &[],
        });
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(Error::Backend(error.to_string()));
        }
        self.check()?;
        let view = texture.create_view(&Default::default());
        let allocation = Arc::new(Allocation {
            generation: Default::default(),
            texture,
            view,
            permit: Arc::new(permit),
        });
        // R8 glyph pages own their own eviction policy and use allocation
        // reference counts to detect active draws. Do not add a pool owner.
        if std::ptr::eq(budget, &self.resident) && format == FORMAT {
            self.resident_pool
                .lock()
                .unwrap()
                .remember(&allocation, budget.limit());
        }
        Ok(allocation)
    }
    pub(crate) fn temporary(
        &self,
        size: Size,
        format: wgpu::TextureFormat,
    ) -> Result<Arc<Allocation>> {
        let mut pool = self.scratch_pool.lock().unwrap();
        if let Some(image) = pool.iter().find(|image| {
            Arc::strong_count(image) == 1
                && image.texture.width() == size.width
                && image.texture.height() == size.height
                && image.texture.format() == format
        }) {
            return Ok(image.clone());
        }
        let bytes = size
            .rgba_bytes()
            .ok_or(Error::Message("temporary image byte size overflow"))?;
        let bytes = if format == wgpu::TextureFormat::R8Unorm {
            bytes / 4
        } else {
            bytes
        };
        if bytes > self.scratch.available() || pool.len() >= 128 {
            pool.retain(|image| Arc::strong_count(image) > 1);
        }
        if pool.len() >= 128 {
            return Err(Error::Message("temporary image capacity reached"));
        }
        if bytes > self.scratch.available()
            && let Some(blur) = self.box_blur.get()
        {
            blur.trim();
        }
        let image = self.allocation(size, format, &self.scratch)?;
        pool.push(image.clone());
        Ok(image)
    }
    /// Idle slots can be reused on the ordered queue, including when their
    /// accounting permits are still pinned by previously submitted work.
    pub(crate) fn reusable_scratch_bytes(&self) -> usize {
        self.scratch_pool
            .lock()
            .unwrap()
            .iter()
            .filter(|image| Arc::strong_count(image) == 1)
            .map(|image| {
                image.texture.width() as usize
                    * image.texture.height() as usize
                    * if image.texture.format() == wgpu::TextureFormat::R8Unorm {
                        1
                    } else {
                        4
                    }
            })
            .sum()
    }

    /// Evict coefficients and idle temporary surfaces. Submitted work keeps its permits.
    pub fn trim_scratch(&self) {
        self.filter_axes.lock().unwrap().clear();
        self.trim_scratch_except(None);
    }
    pub(crate) fn trim_scratch_except(&self, size: Option<Size>) {
        self.trim_resident_pool();
        self.scratch_pool.lock().unwrap().retain(|image| {
            Arc::strong_count(image) > 1
                || size.is_some_and(|size| {
                    image.texture.width() == size.width
                        && image.texture.height() == size.height
                        && image.texture.format() == FORMAT
                })
        });
        if let Some(blur) = self.box_blur.get() {
            blur.trim();
        }
    }
    pub(crate) fn check_image_size(&self, size: Size) -> Result<()> {
        let max = self.device.limits().max_texture_dimension_2d;
        if size.width == 0 || size.height == 0 || size.width > max || size.height > max {
            return Err(Error::Backend(format!(
                "image dimensions exceed backend limits: requested={}x{}, allowed=1..={max} per axis",
                size.width, size.height,
            )));
        }
        Ok(())
    }
    pub fn create_image(&self, size: Size, color: u32) -> Result<Image> {
        self.check_image_size(size)?;
        if Image::prefers_deferred(size) {
            return self.generated_solid(size, color);
        }
        // Layer constructors commonly create a tiny uniform image and then
        // replace/resize it. Share immutable initial pixels; normal writes use
        // the same plane COW as loaded images. Retain at most 64 KiB of pixels.
        let small = size.width <= 32 && size.height <= 32;
        if small
            && let Some((_, image)) = self
                .small_images
                .lock()
                .unwrap()
                .iter()
                .find(|(cached_color, image)| *cached_color == color && image.size == size)
        {
            return Ok(image.shared());
        }
        let main = self.allocation(size, FORMAT, &self.resident)?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.clear(&mut encoder, &main, rgba(color));
        self.submit(encoder, vec![main.clone()]);
        self.check()?;
        let image = Image::new(Some(main), None, size);
        if small {
            let mut cache = self.small_images.lock().unwrap();
            if cache.len() == 16 {
                cache.remove(0);
            }
            cache.push((color, image.shared()));
        }
        Ok(image)
    }

    pub fn cached_subtree_hits(&self) -> u64 {
        self.scene_cache.lock().unwrap().hits
    }
    pub fn trim_resident_pool(&self) {
        self.resident_pool.lock().unwrap().trim();
    }
    pub fn create_province(&self, size: Size) -> Result<Image> {
        let province = self.allocation(size, wgpu::TextureFormat::R8Unorm, &self.resident)?;
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.clear(&mut encoder, &province, [0.0; 4]);
        self.submit(encoder, province.clone());
        self.check()?;
        Ok(Image::new(None, Some(province), size))
    }
    pub(crate) fn clear(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        target: &Allocation,
        color: [f32; 4],
    ) {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear image"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target.view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: color[0].into(),
                        g: color[1].into(),
                        b: color[2].into(),
                        a: color[3].into(),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    pub(crate) fn submit<T: Send + 'static>(&self, encoder: wgpu::CommandEncoder, resources: T) {
        self.flush_fills();
        self.submit_direct(encoder, resources);
    }
    pub(crate) fn submit_direct<T: Send + 'static>(
        &self,
        encoder: wgpu::CommandEncoder,
        resources: T,
    ) {
        self.queue.submit([encoder.finish()]);
        // wgpu owns the native resource references; keep our accounting permits
        // until the same submission is done, including cancellation/resize.
        let in_flight = self.in_flight.clone();
        in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.queue.on_submitted_work_done(move || {
            drop(resources);
            in_flight.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
}
pub(crate) fn extent(size: Size) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: size.width,
        height: size.height,
        depth_or_array_layers: 1,
    }
}
pub(crate) fn rgba(color: u32) -> [f32; 4] {
    [
        (color >> 16 & 255) as f32 / 255.0,
        (color >> 8 & 255) as f32 / 255.0,
        (color & 255) as f32 / 255.0,
        (color >> 24) as f32 / 255.0,
    ]
}

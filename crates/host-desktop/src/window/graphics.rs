use krkr_protocol::{
    graphics::{Command, ImageId, Scene, Size},
    window::{Request, Response},
};
use krkr_render_wgpu::{Gpu, Image, Readback};
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};
use winit::window::Window;

use crate::memory::{RESIDENT_BYTES, SCRATCH_BYTES, TEXTURE_BYTES};

pub(super) struct Graphics {
    pub gpu: Gpu,
    texture_budget: krkr_protocol::budget::Budget,
    images: HashMap<ImageId, Image>,
    lifetimes: HashMap<ImageId, Weak<krkr_protocol::graphics::ImageLifetime>>,
    pending: Vec<(Request, Readback)>,
    prepared: Option<krkr_render_wgpu::DrawPreparation>,
    cache: krkr_protocol::image_cache::Cache,
    last_maintenance: std::time::Instant,
    needs_maintenance: bool,
}
pub(super) struct Surface {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    blitter: wgpu::util::TextureBlitter,
    image: Option<Image>,
    logical: Option<Image>,
    scene_ready: bool,
    viewport: krkr_protocol::viewport::Viewport,
    composed: bool,
    overlay: Option<super::overlay::Overlay>,
}
/// The published tree and its image versions form one commit. Later script
/// writes detach only changed planes through the existing GPU COW mechanism.
#[derive(Default)]
pub(super) struct SceneSnapshot {
    scene: Scene,
    images: HashMap<ImageId, Image>,
}
impl SceneSnapshot {
    pub fn capture(mut scene: Scene, graphics: Option<&Graphics>) -> Result<Self, String> {
        // Ordinary composition never reads an invisible subtree. Do not pin
        // its image versions and force later writes to copy hidden large images.
        // Transitions may explicitly read hidden sources, so retain that graph.
        if scene.transitions.is_empty() {
            let mut visible = vec![false; scene.nodes.len()];
            for (index, node) in scene.nodes.iter_mut().enumerate() {
                let parent_visible = if let Some(parent) = node.parent {
                    if parent >= index {
                        return Err("scene parent must precede its children".into());
                    }
                    visible[parent]
                } else {
                    true
                };
                visible[index] = parent_visible && node.visible && node.opacity != 0;
                if !visible[index] {
                    node.image = None;
                }
            }
        }
        let mut images = HashMap::new();
        for reference in scene
            .nodes
            .iter()
            .filter_map(|node| node.image.as_ref())
            .chain(
                scene
                    .transitions
                    .iter()
                    .filter_map(|transition| transition.rule.as_ref()),
            )
        {
            if let std::collections::hash_map::Entry::Vacant(entry) = images.entry(reference.id) {
                let image = graphics
                    .and_then(|graphics| graphics.images.get(&reference.id))
                    .ok_or("published scene image is no longer available")?;
                entry.insert(image.shared());
            }
        }
        Ok(Self { scene, images })
    }

    fn release_images(&mut self) {
        self.images.clear();
        // Protocol leases also keep images in the host's working image table.
        // After rasterization only the completed canvas is needed for redraw.
        for node in &mut self.scene.nodes {
            node.image = None;
        }
        self.scene.transitions.clear();
    }
}
impl Graphics {
    pub fn new(
        window: Arc<Window>,
        staging: krkr_protocol::budget::Budget,
        cache: krkr_protocol::image_cache::Cache,
    ) -> Result<(Self, Surface), String> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        let mut gpu =
            pollster::block_on(Gpu::new(instance, Some(&surface))).map_err(|e| e.to_string())?;
        if krkr_protocol::diagnostics::enabled() {
            let adapter = gpu.adapter.get_info();
            eprintln!(
                "GPU: {} ({:?}, {:?})",
                adapter.name, adapter.device_type, adapter.backend
            );
        }
        let texture_budget = krkr_protocol::budget::Budget::new(TEXTURE_BYTES);
        gpu.resident = texture_budget.child(TEXTURE_BYTES);
        gpu.scratch = texture_budget.child(SCRATCH_BYTES);
        gpu.staging = staging;
        let surface = Surface::configure(&gpu, surface, &window)?;
        Ok((
            Self {
                gpu,
                texture_budget,
                images: HashMap::new(),
                lifetimes: HashMap::new(),
                pending: Vec::new(),
                prepared: None,
                cache,
                last_maintenance: std::time::Instant::now(),
                needs_maintenance: false,
            },
            surface,
        ))
    }
    pub fn surface(&self, window: Arc<Window>) -> Result<Surface, String> {
        let surface = self
            .gpu
            .instance
            .create_surface(window.clone())
            .map_err(|e| e.to_string())?;
        Surface::configure(&self.gpu, surface, &window)
    }
    fn execute(&mut self, command: &Command) -> Result<Outcome, String> {
        if self.prepared.as_ref().is_some_and(|p| !p.active()) {
            self.prepared = None;
        }
        let main_fill = matches!(command, Command::Fill { fills, .. }
            if fills.iter().all(|fill| fill.face != krkr_protocol::graphics::DrawFace::Province));
        if !main_fill {
            self.gpu.flush_fills();
        }
        let allocation = match command {
            Command::Fill { image, .. } if main_fill => self
                .images
                .get(&image.id)
                .map_or(0, Image::main_write_bytes),
            Command::Meshes { image, size, batch } => {
                let target = self.images.get(&image.id).filter(|i| i.size == *size);
                let alias = batch.draws.iter().any(|draw| matches!(&draw.texture, krkr_protocol::mesh::Texture::Image(source) if source.id == image.id));
                let target_bytes = if alias {
                    size.rgba_bytes().unwrap_or(usize::MAX)
                } else {
                    target.map_or_else(
                        || size.rgba_bytes().unwrap_or(usize::MAX),
                        Image::main_write_bytes,
                    )
                };
                target_bytes.saturating_add(self.gpu.mesh_upload_bytes(batch))
            }
            Command::ComposeScene { image, size, .. } => self
                .images
                .get(&image.id)
                .filter(|i| i.size == *size)
                .map_or_else(
                    || size.rgba_bytes().unwrap_or(usize::MAX),
                    Image::main_write_bytes,
                ),
            Command::Create { size, .. } => Image::create_write_bytes(*size),
            Command::Resize { image, size, .. } => self
                .images
                .get(&image.id)
                .map_or(0, |image| image.resize_write_bytes(*size)),
            Command::EnableImage { size, source, .. } => {
                let main = size.rgba_bytes().unwrap_or(usize::MAX);
                let province = source
                    .as_ref()
                    .and_then(|source| self.images.get(&source.id))
                    .is_some_and(Image::has_province);
                main.saturating_add(if province { main / 4 } else { 0 })
            }
            Command::PrepareUpload {
                size,
                main,
                province,
                ..
            }
            | Command::BeginUpload {
                size,
                main,
                province,
                ..
            } => {
                let bytes = size.rgba_bytes().unwrap_or(usize::MAX);
                (if *main { bytes } else { 0usize }).saturating_add(if *province {
                    bytes / 4
                } else {
                    0
                })
            }
            Command::UploadScaled { image, pixels, .. } => self
                .images
                .get(&image.id)
                .filter(|target| target.size == pixels.size && !target.has_province())
                .map_or_else(
                    || pixels.size.rgba_bytes().unwrap_or(usize::MAX),
                    Image::main_write_bytes,
                ),
            Command::CreateProvince { size, .. } => size.rgba_bytes().unwrap_or(usize::MAX) / 4,
            Command::LoadCompressed { texture, .. } => {
                texture.size.rgba_bytes().unwrap_or(usize::MAX)
            }
            Command::AssignBitmap { source, pixels, .. } => {
                let main = pixels.size.rgba_bytes().unwrap_or(usize::MAX);
                let resize_province = source
                    .as_ref()
                    .and_then(|source| self.images.get(&source.id))
                    .is_some_and(|source| source.has_province() && source.size != pixels.size);
                main.saturating_add(if resize_province { main / 4 } else { 0 })
            }
            Command::PatchRegion {
                image,
                rectangle,
                pixels,
            } => {
                let temporary = pixels.size.rgba_bytes().unwrap_or(usize::MAX);
                temporary.saturating_add(self.images.get(&image.id).map_or(0, |target| {
                    if *rectangle == target.size.rect() {
                        0
                    } else {
                        target.main_write_bytes()
                    }
                }))
            }
            Command::PatchPixels { pixels, .. } => {
                let main = pixels.size.rgba_bytes().unwrap_or(usize::MAX);
                (if pixels.main.is_some() { main } else { 0usize }).saturating_add(
                    if pixels.province.is_some() {
                        main / 4
                    } else {
                        0
                    },
                )
            }
            Command::Independ {
                image, province, ..
            } => self.images.get(&image.id).map_or(0, |image| {
                if *province {
                    if image.has_province() {
                        image.province_write_bytes()
                    } else {
                        0
                    }
                } else {
                    image.main_write_bytes()
                }
            }),
            Command::Copy { image, face, .. } | Command::Color { image, face, .. } => {
                self.images.get(&image.id).map_or(0, |image| {
                    if *face == krkr_protocol::graphics::DrawFace::Province {
                        image.province_write_bytes()
                    } else {
                        image.main_write_bytes()
                    }
                })
            }
            Command::Sprites {
                image,
                options,
                batch,
                ..
            } => self.images.get(&image.id).map_or(0, |image| {
                image.main_write_bytes().saturating_add(
                    if options.face == krkr_protocol::graphics::DrawFace::Province
                        && !batch.clear.is_empty()
                    {
                        image.province_write_bytes()
                    } else {
                        0
                    },
                )
            }),
            Command::WrappedCopy { image, .. }
            | Command::Scanlines { image, .. }
            | Command::Warp { image, .. }
            | Command::Perspective { image, .. }
            | Command::PiledCopy { image, .. }
            | Command::Operate { image, .. }
            | Command::Text { image, .. } => self
                .images
                .get(&image.id)
                .map_or(0, Image::main_write_bytes),
            Command::Transform {
                image,
                transform,
                operation,
                clip,
                clear,
                ..
            } => self.images.get(&image.id).map_or(0, |image| {
                image.transform_write_bytes(*transform, *operation, *clip, *clear)
            }),
            Command::Adjust { image, .. } | Command::Fill { image, .. } => self
                .images
                .get(&image.id)
                .and_then(|i| i.size.rgba_bytes())
                .unwrap_or(0)
                .saturating_mul(2),
            _ => 0,
        };
        // Commands retain their synchronous responses. Completion queries and
        // full lifetime scans are maintenance, not a prerequisite for each
        // ordered copy/fill/resize. Pressure still forces immediate reclamation.
        if allocation
            > RESIDENT_BYTES
                .saturating_sub(self.gpu.resident.used())
                .min(self.gpu.resident.available())
            || self.gpu.staging.used() >= self.gpu.staging.limit() / 2
        {
            self.maintain()?;
            self.gpu.trim_resident_pool();
        }
        while allocation
            > RESIDENT_BYTES
                .saturating_sub(self.gpu.resident.used())
                .min(self.gpu.resident.available())
            && self.cache.evict()
        {
            self.collect_images();
        }
        match command {
            Command::Sprites {
                image,
                source,
                batch,
                clip,
                options,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .draw_sprites(target, &source, batch, *clip, *options)
                    .map_err(|e| e.to_string())?;
            }
            Command::Warp {
                image,
                source,
                effect,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .warp(target, &source, effect)
                    .map_err(|e| e.to_string())?;
            }
            Command::Scanlines {
                image,
                source,
                rows,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .copy_scanlines(target, &source, rows)
                    .map_err(|e| e.to_string())?;
            }
            Command::PreparedDraw(batch) => {
                let image = self
                    .images
                    .get(&batch.image().id)
                    .ok_or("prepared draw image has been released")?;
                let prepared = self
                    .prepared
                    .take()
                    .ok_or("prepared draw resources have been released")?;
                self.gpu
                    .draw_prepared(image, batch, prepared)
                    .map_err(|e| e.to_string())?;
            }
            Command::SnapshotMain { image, source } => {
                let snapshot = self
                    .images
                    .get(&source.id)
                    .ok_or("snapshot source has been released")?
                    .shared_main();
                self.images.insert(image.id, snapshot);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
            }
            Command::ComposeScene { image, size, scene } => {
                let mut target = match self.images.remove(&image.id).filter(|i| i.size == *size) {
                    Some(target) => target,
                    None => self.gpu.create_image(*size, 0).map_err(|e| e.to_string())?,
                };
                let result = self.gpu.compose(&mut target, scene, &self.images);
                self.images.insert(image.id, target);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
                result.map_err(|e| e.to_string())?;
            }
            Command::Meshes { image, size, batch } => {
                let prepared = self
                    .gpu
                    .prepare_meshes(batch, &self.images)
                    .map_err(|e| e.to_string())?;
                if !self.images.get(&image.id).is_some_and(|i| i.size == *size) {
                    let target = self.gpu.create_image(*size, 0).map_err(|e| e.to_string())?;
                    self.images.insert(image.id, target);
                    self.lifetimes
                        .insert(image.id, Arc::downgrade(&image.lifetime));
                }
                self.gpu
                    .draw_meshes(
                        self.images.get_mut(&image.id).expect("mesh target"),
                        batch,
                        prepared,
                    )
                    .map_err(|e| e.to_string())?;
            }
            Command::WrappedCopy {
                image,
                source,
                rectangle,
                destination,
                shift,
                clip,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .copy_wrapped(target, &source, *rectangle, *destination, *shift, *clip)
                    .map_err(|e| e.to_string())?;
            }
            Command::PiledCopy {
                image,
                scene,
                size,
                rectangle,
                x,
                y,
                clip,
            } => {
                let target_size = self
                    .images
                    .get(&image.id)
                    .ok_or("image has been released")?
                    .size;
                let Some((source, destination)) =
                    krkr_render::blit::region(*size, target_size, *clip, *rectangle, *x, *y)
                else {
                    return Ok(Outcome::Ready(Response::Done));
                };
                let size = krkr_protocol::graphics::Size {
                    width: source.width,
                    height: source.height,
                };
                let mut snapshot = self
                    .gpu
                    .create_surface_image(size)
                    .map_err(|e| e.to_string())?;
                self.gpu
                    .compose_region(
                        &mut snapshot,
                        scene,
                        &self.images,
                        (source.left, source.top),
                    )
                    .map_err(|e| e.to_string())?;
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .copy_rect(
                        target,
                        &snapshot.source(),
                        size.rect(),
                        destination.left,
                        destination.top,
                        *clip,
                        krkr_protocol::graphics::DrawFace::Alpha,
                        false,
                    )
                    .map_err(|e| e.to_string())?;
            }
            Command::Adjust {
                image,
                rectangle,
                operation,
            } => {
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .adjust(target, *rectangle, operation)
                    .map_err(|e| e.to_string())?;
            }
            Command::Text {
                image,
                run,
                style,
                clip,
            } => {
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .draw_text(target, run, *style, *clip)
                    .map_err(|e| e.to_string())?;
            }
            Command::Perspective {
                image,
                source,
                mapping,
                clip,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .perspective(target, &source, *mapping, *clip)
                    .map_err(|e| e.to_string())?;
            }
            Command::Transform {
                image,
                source,
                rectangle,
                transform,
                sampling,
                operation,
                clip,
                clear,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .transform(
                        target, &source, *rectangle, *transform, *sampling, *operation, *clip,
                        *clear,
                    )
                    .map_err(|e| e.to_string())?;
            }
            Command::Color {
                image,
                rectangle,
                color,
                opacity,
                face,
            } => {
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .color_rect(target, *rectangle, *color, *opacity, *face)
                    .map_err(|e| e.to_string())?;
            }
            Command::Operate {
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                options,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .operate_rect(target, &source, *rectangle, *x, *y, *clip, *options)
                    .map_err(|e| e.to_string())?;
            }
            Command::EnableImage {
                image,
                source,
                size,
                color,
            } => {
                let mut target = self
                    .gpu
                    .create_image(*size, *color)
                    .map_err(|e| e.to_string())?;
                if let Some(source) = source {
                    let source = self
                        .images
                        .get(&source.id)
                        .ok_or("province source has been released")?
                        .source();
                    self.gpu
                        .copy_rect(
                            &mut target,
                            &source,
                            source.size.rect(),
                            0,
                            0,
                            size.rect(),
                            krkr_protocol::graphics::DrawFace::Province,
                            false,
                        )
                        .map_err(|e| e.to_string())?;
                }
                self.images.insert(image.id, target);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
            }
            Command::CreateProvince {
                image,
                size,
                operation,
            } => {
                let mut target = self.gpu.create_province(*size).map_err(|e| e.to_string())?;
                match operation {
                    krkr_protocol::graphics::ProvinceOperation::Reserve => Ok(()),
                    krkr_protocol::graphics::ProvinceOperation::Fill(fill) => {
                        self.gpu.fill(&mut target, &[*fill])
                    }
                    krkr_protocol::graphics::ProvinceOperation::Copy {
                        source,
                        rectangle,
                        x,
                        y,
                        clip,
                    } => {
                        let source = self
                            .images
                            .get(&source.id)
                            .ok_or("province source has been released")?
                            .source();
                        self.gpu.copy_rect(
                            &mut target,
                            &source,
                            *rectangle,
                            *x,
                            *y,
                            *clip,
                            krkr_protocol::graphics::DrawFace::Province,
                            false,
                        )
                    }
                }
                .map_err(|e| e.to_string())?;
                self.images.insert(image.id, target);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
            }
            Command::Assign { image, source } => {
                let shared = self
                    .images
                    .get(&source.id)
                    .ok_or("assignment source has been released")?
                    .shared();
                self.images.insert(image.id, shared);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
            }
            Command::Independ {
                image,
                province,
                copy,
            } => {
                let target = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .independ_image(target, *province, *copy)
                    .map_err(|e| e.to_string())?;
            }
            Command::AssignBitmap {
                image,
                source,
                pixels,
            } => {
                let source = source
                    .as_ref()
                    .map(|source| {
                        self.images
                            .get(&source.id)
                            .ok_or("assignment source has been released")
                    })
                    .transpose()?;
                let target = self
                    .gpu
                    .assign_bitmap(source, pixels)
                    .map_err(|e| e.to_string())?;
                self.images.insert(image.id, target);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
            }
            Command::PrepareUpload {
                image,
                size,
                main,
                province,
                source,
            }
            | Command::BeginUpload {
                image,
                size,
                main,
                province,
                source,
                ..
            } => {
                let allocation = if let Some(source) = source {
                    let source = self
                        .images
                        .get(&source.id)
                        .ok_or("source image has been released")?;
                    self.gpu.prepare_upload(source, *size, *main, *province)
                } else {
                    self.gpu.reserve_upload(*size, *main, *province)
                }
                .map_err(|e| e.to_string())?;
                self.images.insert(image.id, allocation);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
            }
            Command::LoadCompressed {
                image,
                texture,
                logical_size,
            } => {
                let pixels = krkr_image::compressed::decode(
                    texture,
                    &self.gpu.staging,
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .map_err(|e| e.to_string())?;
                let mut target = self
                    .gpu
                    .reserve_upload(pixels.size, true, false)
                    .map_err(|e| e.to_string())?;
                self.gpu
                    .upload_scaled(&mut target, &pixels, *logical_size)
                    .map_err(|e| {
                        format!(
                            "{e}; compressed upload stored={:?} logical={logical_size:?}",
                            pixels.size
                        )
                    })?;
                let bytes = pixels.size.rgba_bytes().unwrap();
                self.images.insert(image.id, target);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
                return Ok(Outcome::Ready(Response::ImageStorage(bytes)));
            }
            Command::Upload { image, pixels } => {
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("upload image has been released")?;
                self.gpu.upload(image, pixels).map_err(|e| e.to_string())?;
            }
            Command::UploadYuv { .. } | Command::CopyYuv { .. } => {
                return Err("YV12 frames require the Vita GPU video path".into());
            }
            Command::UploadScaled {
                image,
                pixels,
                logical_size,
            } => {
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("upload image has been released")?;
                self.gpu
                    .upload_scaled(image, pixels, *logical_size)
                    .map_err(|e| {
                        format!(
                            "{e}; upload stored={:?} logical={logical_size:?}",
                            pixels.size
                        )
                    })?;
            }
            Command::PatchPixels { image, pixels } => {
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("patch image has been released")?;
                self.gpu
                    .patch_pixels(image, pixels)
                    .map_err(|e| e.to_string())?;
            }
            Command::PatchRegion {
                image,
                rectangle,
                pixels,
            } => {
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("patch image has been released")?;
                self.gpu
                    .patch_region(image, *rectangle, pixels)
                    .map_err(|e| e.to_string())?;
            }
            Command::CopyPixels {
                image,
                pixels,
                split_alpha,
                size,
            } => {
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("movie image has been released")?;
                self.gpu
                    .copy_pixels(image, pixels, *split_alpha, *size)
                    .map_err(|e| e.to_string())?;
            }
            Command::Create {
                image,
                lifetime,
                size,
                color,
            } => {
                if lifetime.upgrade().is_some() {
                    let allocation = self
                        .gpu
                        .create_image(*size, *color)
                        .map_err(|e| e.to_string())?;
                    self.images.insert(*image, allocation);
                    self.lifetimes.insert(*image, lifetime.clone());
                }
            }
            Command::Resize { image, size, color } => {
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .resize(image, *size, *color)
                    .map_err(|e| e.to_string())?;
            }
            Command::Fill { image, fills } => {
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu.fill(image, fills).map_err(|e| e.to_string())?;
            }
            Command::Copy {
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                face,
                hold_alpha,
            } => {
                let source = self
                    .images
                    .get(&source.id)
                    .ok_or("source image has been released")?
                    .source();
                let image = self
                    .images
                    .get_mut(&image.id)
                    .ok_or("image has been released")?;
                self.gpu
                    .copy_rect(
                        image,
                        &source,
                        *rectangle,
                        *x,
                        *y,
                        *clip,
                        *face,
                        *hold_alpha,
                    )
                    .map_err(|e| e.to_string())?;
            }
            Command::ReadHitPlane { image, province } => {
                let image = self
                    .images
                    .get(&image.id)
                    .ok_or("image has been released")?;
                if *province && !image.has_province() {
                    return Ok(Outcome::Ready(Response::HitPlane(
                        krkr_protocol::hit::Plane {
                            size: image.size,
                            data: krkr_protocol::hit::Data::Empty,
                        },
                    )));
                }
                return self
                    .gpu
                    .readback(image, image.size.rect(), *province)
                    .map(Outcome::Reading)
                    .map_err(|e| e.to_string());
            }
            Command::ReadImage { image } | Command::ReadRegion { image, .. } => {
                let image = self
                    .images
                    .get(&image.id)
                    .ok_or("image has been released")?;
                return self
                    .gpu
                    .readback(
                        image,
                        match command {
                            Command::ReadRegion { rectangle, .. } => *rectangle,
                            _ => image.size.rect(),
                        },
                        false,
                    )
                    .map(Outcome::Reading)
                    .map_err(|e| e.to_string());
            }
            Command::ReadProvince { image } => {
                let image = self
                    .images
                    .get(&image.id)
                    .ok_or("image has been released")?;
                if !image.has_province() {
                    return Ok(Outcome::Ready(Response::Image(
                        krkr_protocol::pixels::Pixels {
                            size: image.size,
                            main: None,
                            province: None,
                        },
                    )));
                }
                return self
                    .gpu
                    .readback(image, image.size.rect(), true)
                    .map(Outcome::Reading)
                    .map_err(|e| e.to_string());
            }
            Command::Pixel {
                image,
                x,
                y,
                province,
            } => {
                let image = self
                    .images
                    .get(&image.id)
                    .ok_or("image has been released")?;
                let rect = krkr_protocol::graphics::Rect {
                    left: *x,
                    top: *y,
                    width: 1,
                    height: 1,
                };
                if *province
                    && (!image.has_province() || image.size.rect().intersection(rect).is_none())
                {
                    return Ok(Outcome::Ready(Response::Pixel(0)));
                }
                return self
                    .gpu
                    .readback(image, rect, *province)
                    .map(Outcome::Reading)
                    .map_err(|e| e.to_string());
            }
        }
        Ok(Outcome::Ready(Response::Done))
    }
    pub fn request(&mut self, request: Request) {
        let krkr_protocol::window::Command::Graphics(command) = &request.command else {
            unreachable!()
        };
        let result = self.execute(command);
        self.needs_maintenance = true;
        if matches!(&result, Ok(Outcome::Ready(Response::Done))) {
            let (image, source) = match command {
                Command::Fill { image, .. }
                | Command::Resize { image, .. }
                | Command::Independ {
                    image,
                    province: false,
                    ..
                } => (Some(image.clone()), None),
                Command::Create {
                    image, lifetime, ..
                } => (
                    lifetime
                        .upgrade()
                        .map(|lifetime| krkr_protocol::graphics::ImageRef {
                            id: *image,
                            lifetime,
                        }),
                    None,
                ),
                Command::Copy { image, source, .. } | Command::Operate { image, source, .. } => {
                    (Some(image.clone()), Some(source))
                }
                Command::PreparedDraw(batch) => (Some(batch.image().clone()), batch.source()),
                _ => (None, None),
            };
            if let Some(image) = image
                && let Some(target) = self.images.get(&image.id)
                && let Some((batch, prepared)) = self.gpu.prepare_draws(
                    target,
                    image,
                    source.and_then(|s| self.images.get(&s.id).map(|i| (i, s.clone()))),
                )
            {
                self.prepared = Some(prepared);
                request.offer_draws(batch);
            }
        }

        match result {
            Ok(Outcome::Ready(response)) => request.respond(Ok(response)),
            Ok(Outcome::Reading(readback)) => self.pending.push((request, readback)),
            Err(error) => request.respond(Err(format!(
                "{error} (GPU resident={}/{}, scratch={}/{}, textures total={}/{}, staging={}/{})",
                self.gpu.resident.used(),
                self.gpu.resident.limit(),
                self.gpu.scratch.used(),
                self.gpu.scratch.limit(),
                self.texture_budget.used(),
                self.texture_budget.limit(),
                self.gpu.staging.used(),
                self.gpu.staging.limit(),
            ))),
        }
    }
    pub fn commit(&mut self) {
        // Complete ordered commands before capturing image versions.
        self.gpu.flush_fills();
    }
    pub fn poll(&mut self) -> Result<bool, String> {
        // Synchronous graphics requests can wake the OS loop thousands of
        // times per update. Bound redundant maintenance to once per millisecond;
        // readbacks bypass this gate, and pending work schedules an idle wake.
        if self.pending.is_empty()
            && self.last_maintenance.elapsed() < std::time::Duration::from_millis(1)
        {
            return Ok(self.needs_maintenance || self.gpu.has_pending_work());
        }
        self.maintain()?;
        let mut index = 0;
        while index < self.pending.len() {
            if self.pending[index].0.cancelled() {
                self.pending.swap_remove(index);
                continue;
            }
            if let Some(result) = self.pending[index].1.take() {
                let (request, _) = self.pending.swap_remove(index);
                if matches!(
                    request.command,
                    krkr_protocol::window::Command::Graphics(Command::ReadHitPlane { .. })
                ) {
                    request.respond(result.map_err(|e| e.to_string()).and_then(|pixels| {
                        krkr_protocol::hit::Plane::from_pixels(
                            pixels.size,
                            pixels.data,
                            pixels.channels,
                            &self.gpu.staging,
                        )
                        .map(Response::HitPlane)
                        .map_err(|e| e.to_string())
                    }));
                    continue;
                }
                let whole_image = matches!(
                    request.command,
                    krkr_protocol::window::Command::Graphics(
                        Command::ReadImage { .. }
                            | Command::ReadProvince { .. }
                            | Command::ReadRegion { .. }
                    )
                );
                request.respond(
                    result
                        .map(|pixels| {
                            if whole_image {
                                let (main, province) = if pixels.channels == 1 {
                                    (None, Some(pixels.data))
                                } else {
                                    (Some(pixels.data), None)
                                };
                                return Response::Image(krkr_protocol::pixels::Pixels {
                                    size: pixels.size,
                                    main,
                                    province,
                                });
                            }
                            let p = pixels.data.as_slice();
                            Response::Pixel(if pixels.channels == 1 {
                                p[0] as u32
                            } else {
                                u32::from_be_bytes([p[3], p[0], p[1], p[2]])
                            })
                        })
                        .map_err(|e| e.to_string()),
                );
            } else {
                index += 1;
            }
        }
        Ok(!self.pending.is_empty() || self.gpu.has_pending_work())
    }
    fn maintain(&mut self) -> Result<(), String> {
        if self.prepared.as_ref().is_some_and(|p| !p.active()) {
            self.prepared = None;
        }
        self.gpu.poll().map_err(|e| e.to_string())?;
        self.collect_images();
        self.last_maintenance = std::time::Instant::now();
        self.needs_maintenance = false;

        Ok(())
    }
    fn collect_images(&mut self) {
        self.images.retain(|id, _| {
            if self.lifetimes[id].upgrade().is_some() {
                true
            } else {
                self.lifetimes.remove(id);
                false
            }
        });
    }
}
enum Outcome {
    Ready(Response),
    Reading(Readback),
}
impl Surface {
    fn configure(
        gpu: &Gpu,
        surface: wgpu::Surface<'static>,
        window: &Window,
    ) -> Result<Self, String> {
        let size = window.inner_size();
        let caps = surface.get_capabilities(&gpu.adapter);
        let mut config = surface
            .get_default_config(&gpu.adapter, size.width.max(1), size.height.max(1))
            .ok_or("surface is not supported by the GPU")?;
        // Layer pixels are legacy display-encoded bytes. Present through an
        // unorm view to avoid applying an additional sRGB transfer function.
        config.format = caps
            .formats
            .iter()
            .copied()
            .find(|f| !f.is_srgb())
            .unwrap_or(config.format);
        let view_format = config.format.remove_srgb_suffix();
        if view_format != config.format {
            config.view_formats.push(view_format);
        }
        config.present_mode = wgpu::PresentMode::AutoVsync;
        config.desired_maximum_frame_latency = 2;
        surface.configure(&gpu.device, &config);
        let blitter = wgpu::util::TextureBlitter::new(&gpu.device, view_format);
        let overlay = super::overlay::enabled()
            .then(|| super::overlay::Overlay::new(gpu, view_format))
            .transpose()?;
        Ok(Self {
            surface,
            config,
            blitter,
            image: None,
            logical: None,
            scene_ready: false,
            viewport: Default::default(),
            composed: false,
            overlay,
        })
    }
    pub fn draw(
        &mut self,
        graphics: &mut Graphics,
        window: &Arc<Window>,
        snapshot: &mut SceneSnapshot,
    ) -> Result<(), String> {
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        let size = Size {
            width: size.width,
            height: size.height,
        };
        if size.width != self.config.width || size.height != self.config.height {
            self.config.width = size.width;
            self.config.height = size.height;
            self.surface.configure(&graphics.gpu.device, &self.config);
        }
        // configure can finish old GPU work. Run completion callbacks before
        // reserving the replacement frame so its old permits can be reused.
        graphics.maintain()?;
        while graphics.gpu.resident.used() > RESIDENT_BYTES && graphics.cache.evict() {
            graphics.collect_images();
        }
        if self.image.as_ref().is_none_or(|image| image.size != size) {
            self.composed = false;
            self.image = None;
        }
        if let Some(overlay) = &mut self.overlay {
            overlay.refresh(
                &graphics.gpu,
                graphics.texture_budget.used(),
                std::time::Instant::now(),
            );
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&graphics.gpu.device, &self.config);
                window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                let mut restored = graphics.surface(window.clone())?;
                // Surface loss does not lose device textures. Retain the
                // completed canvas so recovery needs no old layer versions.
                restored.logical = self.logical.take();
                restored.scene_ready = self.scene_ready;
                restored.viewport = self.viewport;
                *self = restored;
                window.request_redraw();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("surface validation failed".into());
            }
        };
        let new_frame = !self.scene_ready;
        if !self.scene_ready {
            let scene = &snapshot.scene;
            let primary = scene
                .nodes
                .iter()
                .find(|node| node.parent.is_none() && node.visible);
            // Keep compose_window's canvas/viewport convention. With the
            // identity viewport the original composition used client bounds.
            let logical_size = if let Some(primary) = primary
                && scene.viewport != Default::default()
            {
                self.viewport = scene.viewport;
                let rect = primary.rectangle;
                Size {
                    width: rect.width,
                    height: rect.height,
                }
            } else {
                self.viewport = Default::default();
                size
            };
            // A 1:1 canvas is already the complete window image. Release the
            // disposable mapping target before composition needs scratch space.
            if logical_size == size && self.viewport.destination(logical_size) == size.rect() {
                self.image = None;
            }
            if self
                .logical
                .as_ref()
                .is_none_or(|canvas| canvas.size != logical_size)
            {
                self.logical = None;
                self.logical = Some(
                    graphics
                        .gpu
                        .create_surface_image(logical_size)
                        .map_err(|e| e.to_string())?,
                );
            }
            graphics
                .gpu
                .compose(
                    self.logical.as_mut().expect("logical canvas"),
                    scene,
                    &snapshot.images,
                )
                .map_err(|e| {
                    format!(
                        "scene composition: {e} (resident={}/{}, scratch={}/{}, textures total={}/{}, staging={}/{})",
                        graphics.gpu.resident.used(),
                        graphics.gpu.resident.limit(),
                        graphics.gpu.scratch.used(),
                        graphics.gpu.scratch.limit(),
                        graphics.texture_budget.used(),
                        graphics.texture_budget.limit(),
                        graphics.gpu.staging.used(),
                        graphics.gpu.staging.limit()
                    )
                })?;
            self.scene_ready = true;
            // The submission pins resources until GPU completion. The host
            // need not keep full old Layer images after their frame is encoded.
            snapshot.release_images();
        }
        let canvas = self.logical.as_ref().expect("completed canvas");
        let direct = canvas.size == size && self.viewport.destination(canvas.size) == size.rect();
        if direct {
            self.image = None;
            self.composed = false;
        } else {
            if self.image.is_none() {
                self.image =
                    Some(graphics.gpu.create_surface_image(size).map_err(|e| {
                        format!("window frame {}x{}: {e}", size.width, size.height)
                    })?);
                self.composed = false;
            }
            if !self.composed {
                graphics
                    .gpu
                    .map_window(
                        self.image.as_mut().expect("surface image"),
                        canvas,
                        self.viewport,
                    )
                    .map_err(|e| e.to_string())?;
                self.composed = true;
            }
        }
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(self.config.format.remove_srgb_suffix()),
            ..Default::default()
        });
        graphics
            .gpu
            .present_to_with(
                if direct {
                    canvas
                } else {
                    self.image.as_ref().expect("surface image")
                },
                &self.blitter,
                &view,
                |encoder| {
                    if let Some(overlay) = &self.overlay {
                        overlay.draw(encoder, &view, size);
                    }
                },
            )
            .map_err(|e| e.to_string())?;
        if let Some(overlay) = &mut self.overlay
            && new_frame
        {
            overlay.counter.frame();
        }
        window.pre_present_notify();
        graphics.gpu.queue.present(frame);

        Ok(())
    }

    pub fn release_frame(&mut self) {
        self.image = None;
        // Keep one canvas for showing the window again, rather than all source
        // image versions. The disposable client-sized frame can be recreated.
        self.composed = false;
    }
    pub fn overlay_deadline(&self) -> Option<std::time::Instant> {
        self.overlay.as_ref().map(|overlay| overlay.counter.next)
    }
    pub fn invalidate_scene(&mut self) {
        self.scene_ready = false;
        self.composed = false;
    }
}

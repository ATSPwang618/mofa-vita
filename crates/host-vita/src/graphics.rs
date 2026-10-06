//! Ordered image commands and immutable scene versions on the EGL thread.
mod endpoints;
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[allow(unsafe_code)]
#[path = "../tests/graphics/pressure.rs"]
mod pressure_tests;
mod residency;
use krkr_protocol::{
    graphics::{
        Blend, Color, Command, DRAW_BATCH_CAPACITY, DrawFace, ImageId, ImageLifetime, ImageRef,
        PreparedDraws, ProvinceOperation, Scene,
    },
    image_cache::Cache,
    window::Response,
};
use krkr_render_gles2::{Gpu, Image};
use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

pub struct Graphics {
    pub gpu: Gpu,
    images: HashMap<ImageId, Image>,
    uploads: HashMap<ImageId, krkr_render_gles2::PendingUpload>,
    lifetimes: HashMap<ImageId, Weak<ImageLifetime>>,
    cache: Cache,
    draw_grant: Option<Weak<()>>,
    spilled: HashMap<ImageId, std::rc::Rc<krkr_render_gles2::SpilledImage>>,
    touched: HashMap<ImageId, u64>,
    serial: u64,
    physical_free: fn() -> Option<usize>,
    pressure: Option<(usize, usize)>,
    endpoints: [Option<endpoints::Endpoint>; 2],
}
pub struct Snapshot {
    pub scene: Scene,
    pub images: HashMap<ImageId, Image>,
}
type CaptureClip = ((i64, i64), Option<[i64; 4]>);
impl Snapshot {
    pub fn release_images(&mut self) {
        self.images.clear();
        for node in &mut self.scene.nodes {
            node.image = None;
        }
        self.scene.transitions.clear();
    }
}
impl Graphics {
    pub fn new(gpu: Gpu, cache: Cache) -> Self {
        Self {
            gpu,
            cache,
            images: HashMap::new(),
            uploads: HashMap::new(),
            lifetimes: HashMap::new(),
            draw_grant: None,
            spilled: HashMap::new(),
            touched: HashMap::new(),
            serial: 0,
            physical_free: crate::memory::graphics_free,
            pressure: None,
            endpoints: [None, None],
        }
    }
    /// The protocol closes this grant before any observable operation or scene
    /// fence. Only already writable regions qualify; untouched constant tiles
    /// must neither block a color batch nor acquire deferred write allocations.
    pub fn prepare_draws(&mut self, command: &Command) -> Option<PreparedDraws> {
        let reference = match command {
            Command::Fill { image, .. } | Command::Color { image, .. } => image.clone(),
            Command::PreparedDraw(batch) => batch.image().clone(),
            Command::Create {
                image, lifetime, ..
            } => ImageRef {
                id: *image,
                lifetime: lifetime.upgrade()?,
            },
            _ => return None,
        };
        let target = self.images.get(&reference.id)?;
        let colors = matches!(command, Command::Color { .. })
            || matches!(command, Command::PreparedDraw(batch) if matches!(batch.draws().first(), Some(krkr_protocol::graphics::PreparedDraw::Color(_))));
        let regions = if colors {
            Some(self.gpu.color_batch_regions(target)?)
        } else {
            None
        };
        if !target.has_main() || (!colors && self.gpu.canvas_write_bytes(target, false) != 0) {
            return None;
        }
        let mut batch = PreparedDraws::reserve(
            reference,
            target.size,
            None,
            false,
            DRAW_BATCH_CAPACITY * std::mem::size_of::<Color>(),
            &self.gpu.staging,
        )?;
        if let Some(regions) = regions {
            batch = batch.allow_colors(target.stored_size()?, regions)?;
        }
        self.draw_grant = Some(batch.lifetime());
        Some(batch)
    }
    pub fn capture(&mut self, mut scene: Scene) -> Result<Snapshot, String> {
        residency::prune_scene_images(&mut scene)?;
        self.capture_prepared(scene, None)
    }
    pub fn capture_scaled(
        &mut self,
        mut scene: Scene,
        logical: krkr_protocol::graphics::Size,
        physical: krkr_protocol::graphics::Size,
    ) -> Result<Snapshot, String> {
        self.prepare_scene_capture(physical, &mut scene)?;
        self.capture_prepared(scene, Some((logical, physical)))
    }
    pub(crate) fn capture_prepared(
        &mut self,
        mut scene: Scene,
        raster: Option<(krkr_protocol::graphics::Size, krkr_protocol::graphics::Size)>,
    ) -> Result<Snapshot, String> {
        let mut images = if let Some((logical, physical)) = raster {
            self.prepare_endpoints(&mut scene, logical, physical)?
        } else {
            self.endpoints = [None, None];
            HashMap::new()
        };
        let needed = residency::scene_images(&scene);
        self.ensure_images(&needed)?;
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
                entry.insert(self.image(reference)?.shared());
            }
        }
        Ok(Snapshot { scene, images })
    }
    fn image(&self, reference: &ImageRef) -> Result<&Image, String> {
        self.images
            .get(&reference.id)
            .ok_or_else(|| "image has been released".into())
    }
    fn put(&mut self, reference: &ImageRef, image: Image) {
        self.touched.insert(reference.id, self.serial);
        self.spilled.remove(&reference.id);
        self.uploads.remove(&reference.id);
        self.images.insert(reference.id, image);
        self.lifetimes
            .insert(reference.id, Arc::downgrade(&reference.lifetime));
    }
    fn write(
        &mut self,
        reference: &ImageRef,
        draw: impl FnOnce(&Gpu, &mut Image) -> krkr_render::Result<()>,
    ) -> Result<(), String> {
        let image = self
            .images
            .get_mut(&reference.id)
            .ok_or("image has been released")?;
        draw(&self.gpu, image).map_err(|e| e.to_string())?;
        self.gpu
            .compact_fragmented_canvas(image)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn reap(&mut self) {
        self.lifetimes.retain(|id, lease| {
            let alive = lease.strong_count() != 0;
            if !alive {
                self.images.remove(id);
                self.spilled.remove(id);
                self.touched.remove(id);
                self.uploads.remove(id);
            }
            alive
        });
    }
    pub fn maintain(&mut self) -> Result<(), String> {
        let _stage = crate::watchdog::scope(crate::watchdog::Stage::Maintain);
        self.reap();
        for image in self.images.values_mut() {
            self.gpu
                .compact_fragmented_canvas(image)
                .map_err(|e| e.to_string())?;
        }
        self.gpu.collect_mesh_textures();
        self.gpu.collect_warp_tables();
        self.gpu.collect_adjustment_tables();
        self.gpu.maintain().map_err(|e| e.to_string())
    }
    fn compact_canvases(&mut self, headroom: usize) -> Result<(), String> {
        self.gpu
            .compact_canvases(self.images.values_mut(), headroom)
            .map_err(|e| e.to_string())
    }
    /// Scene surfaces share the texture pool with cached assets. Reclaim the
    /// optional cache before rasterization, while no partial frame is visible.
    pub fn prepare_scene(&mut self, physical: krkr_protocol::graphics::Size) -> Result<(), String> {
        self.prepare_scene_inner(physical, None)
    }
    pub fn prepare_scene_capture(
        &mut self,
        physical: krkr_protocol::graphics::Size,
        scene: &mut Scene,
    ) -> Result<(), String> {
        residency::prune_scene_images(scene)?;
        self.prepare_scene_inner(physical, Some(scene))
    }
    fn prepare_scene_inner(
        &mut self,
        physical: krkr_protocol::graphics::Size,
        scene: Option<&Scene>,
    ) -> Result<(), String> {
        let headroom = physical
            .rgba_bytes()
            .unwrap_or(usize::MAX)
            .saturating_mul(8)
            .min(self.gpu.scratch.limit());
        let protected = scene.map(residency::scene_images).unwrap_or_default();
        self.reclaim_physical(headroom, &protected)?;
        // Retired scratch textures are reusable, even while their permits
        // remain charged. Ignoring them cleared subtree caches and forced a
        // global GPU finish precisely when the warmed surface pool was useful.
        if self.gpu.scratch_capacity() >= headroom {
            return Ok(());
        }
        self.maintain()?;
        if self.gpu.scratch_capacity() >= headroom {
            return Ok(());
        }
        self.gpu.collect().map_err(|e| e.to_string())?;
        self.compact_canvases(headroom)?;
        while self.gpu.scratch_capacity() < headroom && self.evict_cache() {
            self.reap();
        }
        if self.gpu.scratch.available() < headroom {
            self.gpu.collect().map_err(|e| e.to_string())?;
            // Eviction can remove the last alias which prevented compaction.
            self.compact_canvases(headroom)?;
        }
        // Before capture, protect its future inputs explicitly to avoid
        // spilling and immediately restoring them. Afterwards, snapshot
        // ownership itself prevents their eviction.
        self.make_room(headroom, &protected)?;
        Ok(())
    }
    pub fn execute(&mut self, command: &Command) -> Result<Response, String> {
        use crate::watchdog::{Stage, scope};
        let _stage = scope(match command {
            Command::UploadYuv { .. } => Stage::UploadYuv,
            Command::CopyYuv { .. } => Stage::CopyYuv,
            Command::Copy { .. } => Stage::Copy,
            Command::Transform { .. } => Stage::Transform,
            Command::ReadHitPlane { .. } => Stage::HitPlane,
            _ => Stage::Graphics,
        });
        self.invalidate_endpoints(command);
        let timer = krkr_protocol::diagnostics::Timer::start();
        let result = self.execute_inner(command);
        timer.report(|| format!("stage=graphics-work command={}", command_name(command)));
        result.map_err(|error| {
            self.report_failure(&format!("graphics {}", command_name(command)), error)
        })
    }
    pub(super) fn report_failure(&self, stage: &str, error: impl std::fmt::Display) -> String {
        let message = format!(
            "{stage}: {error}; resident={}/{} scratch={}/{} staging={}/{} bytes",
            self.gpu.resident.used(),
            self.gpu.resident.limit(),
            self.gpu.scratch.used(),
            self.gpu.scratch.limit(),
            self.gpu.staging.used(),
            self.gpu.staging.limit(),
        );
        // Capture before the failing scene snapshot and VM release their images.
        // Free memory reported at game END cannot explain the failed allocation.
        #[cfg(target_os = "vita")]
        let message = format!("{message}; {}", crate::memory::free_report());
        krkr_protocol::profile::marker("graphics.error", || message.clone());
        message
    }
    fn command_allocation(&self, command: &Command) -> Result<usize, String> {
        Ok(match command {
            Command::Create { size, color, .. } => self.gpu.create_image_bytes(*size, *color),
            Command::ComposeScene { size, .. } => self.gpu.canvas_allocation_bytes(*size, None),
            Command::Resize { image, size, color } => self.image(image).map_or(0, |source| {
                self.gpu.resize_image_bytes(source, *size, *color)
            }),
            Command::EnableImage {
                source,
                size,
                color,
                ..
            } => self.gpu.create_image_bytes(*size, *color).saturating_add(
                if source
                    .as_ref()
                    .and_then(|r| self.images.get(&r.id))
                    .is_some_and(|image| image.has_province() && image.size != *size)
                {
                    size.rgba_bytes().unwrap_or(usize::MAX)
                } else {
                    0
                },
            ),
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
            } => size
                .rgba_bytes()
                .unwrap_or(usize::MAX)
                .saturating_mul(usize::from(*main) + usize::from(*province)),
            Command::CreateProvince { size, .. } => size.rgba_bytes().unwrap_or(usize::MAX),
            Command::Meshes { image, size, batch } => {
                let upload = self
                    .gpu
                    .mesh_upload_bytes(batch)
                    .map_err(|e| e.to_string())?;
                let target = self
                    .images
                    .get(&image.id)
                    .filter(|target| target.size == *size)
                    .map_or_else(
                        || size.rgba_bytes().unwrap_or(usize::MAX),
                        |target| {
                            let alias = batch.draws.iter().any(|draw| {
                                matches!(&draw.texture,
                            krkr_protocol::mesh::Texture::Image(source) if source.id == image.id)
                            });
                            if alias {
                                target.resident_bytes().max(target.write_bytes(false))
                            } else {
                                target.write_bytes(false)
                            }
                        },
                    );
                upload.saturating_add(target)
            }
            Command::LoadCompressed { texture, .. } => {
                if self.gpu.supports_compressed(texture) {
                    texture.data().len()
                } else {
                    texture.size.rgba_bytes().unwrap_or(usize::MAX)
                }
            }
            Command::AssignBitmap { source, pixels, .. } => {
                let province = source
                    .as_ref()
                    .and_then(|source| self.images.get(&source.id))
                    .is_some_and(|source| source.has_province() && source.size != pixels.size);
                pixels
                    .size
                    .rgba_bytes()
                    .unwrap_or(usize::MAX)
                    .saturating_mul(1 + usize::from(province))
            }
            Command::UploadScaled { image, .. } | Command::Upload { image, .. }
                if self.uploads.contains_key(&image.id) =>
            {
                0
            }
            Command::UploadScaled { image, pixels, .. } => self.image(image).map_or(0, |target| {
                // PrepareUpload has already reserved this compact image.
                // Charging two more images here evicted caches and forced a
                // global GPU wait even when the upload changes unique storage.
                if target.size == pixels.size && target.has_main() && !target.has_province() {
                    target.write_bytes(false)
                } else {
                    pixels.size.rgba_bytes().unwrap_or(usize::MAX)
                }
            }),
            Command::PatchRegion {
                image,
                rectangle,
                pixels,
            } => {
                let temporary = pixels.size.rgba_bytes().unwrap_or(usize::MAX);
                temporary.saturating_add(self.image(image).map_or(0, |target| {
                    if *rectangle == target.size.rect() {
                        0
                    } else {
                        target.write_bytes(false)
                    }
                }))
            }
            Command::PatchPixels { pixels, .. } => pixels
                .size
                .rgba_bytes()
                .unwrap_or(usize::MAX)
                .saturating_mul(
                    usize::from(pixels.main.is_some()) + usize::from(pixels.province.is_some()),
                ),
            Command::UploadYuv { pixels, .. } => pixels
                .size
                .rgba_bytes()
                .unwrap_or(usize::MAX)
                .saturating_mul(2),
            Command::CopyYuv { image, pixels, .. } => pixels
                .size
                .rgba_bytes()
                .unwrap_or(usize::MAX)
                .saturating_mul(2)
                .saturating_add(
                    self.image(image)
                        .map_or(0, |image| image.write_bytes(false)),
                ),
            Command::Fill { image, fills } => self
                .image(image)
                .map_or(0, |image| self.gpu.fill_write_bytes(image, fills)),
            Command::Copy {
                image,
                face,
                source,
                rectangle,
                x,
                y,
                clip,
                hold_alpha,
            } => {
                self.image(image)
                    .ok()
                    .zip(self.image(source).ok())
                    .map_or(0, |(target, input)| {
                        krkr_render::blit::region(
                            input.size,
                            target.size,
                            *clip,
                            *rectangle,
                            *x,
                            *y,
                        )
                        .map_or(0, |(src, dst)| {
                            if (image.id == source.id && src == dst)
                                || (matches!(face, DrawFace::Alpha | DrawFace::AddAlpha)
                                    || (*face == DrawFace::Opaque && !hold_alpha))
                                    && self.gpu.copy_is_view(target, input, src, dst)
                            {
                                0
                            } else {
                                self.gpu.copy_write_bytes(
                                    target,
                                    input,
                                    dst,
                                    *face == DrawFace::Province,
                                    image.id == source.id,
                                )
                            }
                        })
                    })
            }
            Command::Color {
                image,
                rectangle,
                color,
                opacity,
                face,
            } => self.image(image).map_or(0, |target| {
                self.gpu
                    .color_write_bytes(target, *rectangle, *color, *opacity, *face)
            }),
            Command::Sprites {
                image,
                source,
                options,
                batch,
                ..
            } => self.image(image).map_or(0, |target| {
                let main = if source.id == image.id {
                    target.resident_bytes().max(target.write_bytes(false))
                } else {
                    target.write_bytes(false)
                };
                main + usize::from(options.face == DrawFace::Province && !batch.clear.is_empty())
                    * target.write_bytes(true)
            }),
            Command::Operate {
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                options,
            } => {
                self.image(image)
                    .ok()
                    .zip(self.image(source).ok())
                    .map_or(0, |(target, input)| {
                        if options.is_noop() {
                            return 0;
                        }
                        krkr_render::blit::region(
                            input.size,
                            target.size,
                            *clip,
                            *rectangle,
                            *x,
                            *y,
                        )
                        .map_or(0, |(src, dst)| {
                            if options.mode == Blend::Opaque
                                && options.opacity == 255
                                && options.face == DrawFace::Opaque
                                && !options.hold_alpha
                                && self.gpu.copy_is_view(target, input, src, dst)
                            {
                                return 0;
                            }
                            self.gpu
                                .operate_write_bytes(target, input, dst, image.id == source.id)
                        })
                    })
            }
            Command::Transform {
                image,
                source,
                rectangle,
                transform,
                clip,
                clear,
                operation,
                ..
            } => self.image(image).map_or(0, |target| {
                if operation.is_noop() {
                    return 0;
                }
                let Some(clip) = clip.intersection(target.size.rect()) else {
                    return 0;
                };
                krkr_render::transform::Mapping::new(*rectangle, *transform, clip)
                    .ok()
                    .flatten()
                    .map(|m| {
                        if !matches!(transform, krkr_protocol::transform::Transform::Affine(_)) {
                            let area = self
                                .gpu
                                .transform_write_area(target, m.bounds, clip, *operation, *clear);
                            return self.gpu.canvas_region_write_bytes(
                                target,
                                area,
                                false,
                                image.id == source.id,
                            );
                        }
                        self.gpu.transform_write_bytes(
                            target,
                            m.bounds,
                            clip,
                            *operation,
                            *clear,
                            image.id == source.id,
                        )
                    })
                    .unwrap_or_else(|| {
                        clear.map_or(0, |_| {
                            self.gpu.canvas_region_write_bytes(
                                target,
                                clip,
                                false,
                                image.id == source.id,
                            )
                        })
                    })
            }),
            Command::Scanlines {
                image,
                source,
                rows,
            } => self.image(image).map_or(0, |target| {
                let writable = if source.id == image.id {
                    target.resident_bytes().max(target.write_bytes(false))
                } else {
                    target.write_bytes(false)
                };
                writable.saturating_add(self.gpu.scanline_upload_bytes(target, rows))
            }),
            Command::WrappedCopy { image, source, .. }
            | Command::Perspective { image, source, .. } => self.image(image).map_or(0, |target| {
                if source.id == image.id {
                    target.resident_bytes().max(target.write_bytes(false))
                } else {
                    target.write_bytes(false)
                }
            }),
            Command::Warp {
                image,
                source,
                effect,
            } => self
                .image(image)
                .map_or(0, |target| {
                    if source.id == image.id {
                        target.resident_bytes().max(target.write_bytes(false))
                    } else {
                        target.write_bytes(false)
                    }
                })
                .saturating_add(self.gpu.warp_upload_bytes(effect)),
            Command::Adjust {
                image,
                rectangle,
                operation,
                ..
            } => self
                .image(image)
                .map_or(0, |target| {
                    self.gpu.adjust_write_bytes(target, *rectangle, operation)
                })
                .saturating_add(self.gpu.adjust_upload_bytes(operation)),
            Command::Text {
                image,
                run,
                style,
                clip,
            } => self.image(image).map_or(0, |target| {
                self.gpu.text_write_bytes(target, run, *style, *clip)
            }),
            Command::CopyPixels { image, .. } => self
                .image(image)
                .map_or(0, |target| target.write_bytes(false)),
            Command::Independ {
                image,
                province,
                copy,
            } => self
                .image(image)
                .map_or(0, |i| i.independ_bytes(*province, *copy)),
            Command::Upload { image, pixels } => self.image(image).map_or(0, |target| {
                usize::from(pixels.main.is_some()) * target.write_bytes(false)
                    + usize::from(pixels.province.is_some()) * target.write_bytes(true)
            }),
            _ => 0,
        })
    }
    fn execute_inner(&mut self, command: &Command) -> Result<Response, String> {
        if let Command::Assign { image, source } | Command::SnapshotMain { image, source } = command
            && let Some(saved) = self.spilled.get(&source.id).cloned()
        {
            // Both commands only share a version. Its parked pixels need not
            // be decompressed or uploaded until a later read or write uses it.
            self.serial = self.serial.wrapping_add(1);
            self.touched.insert(image.id, self.serial);
            self.images.remove(&image.id);
            self.uploads.remove(&image.id);
            self.spilled.insert(image.id, saved);
            self.lifetimes
                .insert(image.id, Arc::downgrade(&image.lifetime));
            return Ok(Response::Done);
        }
        if let Command::Fill { image, fills } = command
            && let Some(saved) = self.spilled.get(&image.id)
            && let Some(next) = self
                .gpu
                .fill_spilled_canvas(saved, fills)
                .map_err(|e| e.to_string())?
        {
            self.serial = self.serial.wrapping_add(1);
            self.put(image, next);
            return Ok(Response::Done);
        }
        if let Command::Transform {
            image,
            source,
            rectangle,
            transform: krkr_protocol::transform::Transform::Affine(points),
            operation: krkr_protocol::transform::ImageOperation::Copy { hold_alpha: false },
            clip,
            clear: Some(color),
            ..
        } = command
            && image.id != source.id
            && self.spilled.contains_key(&image.id)
        {
            self.ensure_images(&[source.id])?;
            // Restoring the source can also restore aliases of the target.
            if let Some(saved) = self.spilled.get(&image.id)
                && let Some(next) = self
                    .gpu
                    .clear_spilled_affine(
                        saved,
                        self.image(source)?,
                        *rectangle,
                        *points,
                        *clip,
                        *color,
                    )
                    .map_err(|e| e.to_string())?
            {
                self.put(image, next);
            }
        }
        let needed = match command {
            Command::Upload { image, .. } | Command::UploadScaled { image, .. }
                if self.uploads.contains_key(&image.id) =>
            {
                smallvec::SmallVec::new()
            }
            _ => residency::command_images(command),
        };
        self.ensure_images(&needed).map_err(|error| match command {
            Command::Transform { image, source, rectangle, operation, clip, clear, .. } =>
                format!("{error}; target={:?} source={:?} rectangle={rectangle:?} operation={operation:?} clip={clip:?} clear={clear:?}", image.id, source.id),
            _ => error,
        })?;
        if let Command::Adjust {
            image,
            rectangle,
            operation: krkr_protocol::graphics::Adjustment::BoxBlur { radius, .. },
        } = command
            && let Some(headroom) =
                self.gpu
                    .blur_preferred_headroom(self.image(image)?, *rectangle, *radius)
        {
            self.make_room(headroom, &needed)?;
        }
        // Reclaim optional aliases before durable allocation; do not retry a
        // potentially partially executed blend after an admission failure.
        let mut allocation = self.command_allocation(command)?;
        self.reclaim_physical(allocation, &needed)?;
        // Reclaiming aliases may remove the need for a copy-on-write canvas.
        allocation = self.command_allocation(command)?;
        if allocation > self.gpu.resident.available() {
            self.maintain()?;
            self.gpu.collect().map_err(|e| e.to_string())?;
            self.compact_canvases(allocation)?;
            allocation = self.command_allocation(command)?;
            // Drop enough optional aliases before waiting on the GPU. A cache
            // entry may share storage with a live layer, so count actual dead
            // allocations rather than its logical cache charge. Retiring one
            // image at a time caused a global finish for every LRU eviction.
            while allocation > self.gpu.capacity_after_collect(&self.gpu.resident)
                && self.evict_cache()
            {
                self.reap();
                allocation = self.command_allocation(command)?;
            }
            if allocation > self.gpu.resident.available() {
                self.gpu.collect().map_err(|e| e.to_string())?;
                self.compact_canvases(allocation)?;
                allocation = self.command_allocation(command)?;
                if allocation > self.gpu.resident.available() {
                    self.make_room(allocation, &needed)?;
                    allocation = self.command_allocation(command)?;
                }
                if allocation > self.gpu.resident.available() {
                    self.gpu
                        .reclaim_canvas_borders(self.images.values_mut(), allocation)
                        .map_err(|e| e.to_string())?;
                }
            }
            // Border compaction can change sharing and copy-on-write cost.
            allocation = self.command_allocation(command)?;
        }
        self.make_room(allocation, &needed)?;
        if let Command::BeginUpload { staging_bytes, .. } = command {
            self.make_staging_room(*staging_bytes, allocation)?;
        }
        match command {
            Command::Warp {
                image,
                source,
                effect,
            } => {
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| gpu.warp(target, &source, effect))?;
            }
            Command::LoadCompressed {
                image,
                texture,
                logical_size,
            } => {
                let next = if self.gpu.supports_compressed(texture) {
                    self.gpu
                        .load_compressed(texture)
                        .map_err(|e| e.to_string())?
                } else {
                    let pixels = krkr_image::compressed::decode(
                        texture,
                        &self.gpu.staging,
                        &std::sync::atomic::AtomicBool::new(false),
                    )
                    .map_err(|e| e.to_string())?;
                    self.gpu
                        .assign_bitmap(None, &pixels)
                        .map_err(|e| e.to_string())?
                };
                let next = self
                    .gpu
                    .logical_image(next, *logical_size)
                    .map_err(|e| e.to_string())?;
                let bytes = next.resident_bytes();
                self.put(image, next);
                return Ok(Response::ImageStorage(bytes));
            }
            Command::Meshes { image, size, batch } => {
                let prepared = self
                    .gpu
                    .prepare_meshes(batch, &self.images)
                    .map_err(|e| e.to_string())?;
                if !self
                    .images
                    .get(&image.id)
                    .is_some_and(|target| target.size == *size)
                {
                    let target = self.gpu.create_image(*size, 0).map_err(|e| e.to_string())?;
                    self.put(image, target);
                }
                self.gpu
                    .draw_meshes(
                        self.images.get_mut(&image.id).expect("mesh target"),
                        prepared,
                    )
                    .map_err(|e| e.to_string())?;
            }
            Command::Perspective {
                image,
                source,
                mapping,
                clip,
            } => {
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| {
                    gpu.perspective(target, &source, *mapping, *clip)
                })?;
            }
            Command::Sprites {
                image,
                source,
                batch,
                clip,
                options,
            } => {
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| {
                    gpu.draw_sprites(target, &source, batch, *clip, *options)
                })?;
            }
            Command::Scanlines {
                image,
                source,
                rows,
            } => {
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| {
                    gpu.copy_scanlines(target, &source, rows)
                })?;
            }
            Command::WrappedCopy {
                image,
                source,
                rectangle,
                destination,
                shift,
                clip,
            } => {
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| {
                    gpu.copy_wrapped(target, &source, *rectangle, *destination, *shift, *clip)
                })?;
            }
            Command::CopyPixels {
                image,
                pixels,
                split_alpha,
                size,
            } => self.write(image, |gpu, target| {
                gpu.copy_pixels(target, pixels, *split_alpha, *size)
            })?,
            Command::Create {
                image,
                lifetime,
                size,
                color,
            } => {
                if lifetime.strong_count() != 0 {
                    let next = self
                        .gpu
                        .create_image(*size, *color)
                        .map_err(|e| e.to_string())?;
                    self.uploads.remove(image);
                    self.images.insert(*image, next);
                    self.lifetimes.insert(*image, lifetime.clone());
                }
            }
            Command::Resize { image, size, color } => {
                let old = self.image(image)?;
                let next = self.gpu.resize(old, *size, *color).map_err(|e| {
                    format!(
                        "{e}; resize logical={:?} stored={:?} to={size:?}",
                        old.size,
                        old.stored_size()
                    )
                })?;
                self.put(image, next);
            }
            Command::Assign { image, source } | Command::SnapshotMain { image, source } => {
                let next = if matches!(command, Command::SnapshotMain { .. }) {
                    self.image(source)?.shared_main()
                } else {
                    self.image(source)?.shared()
                };
                self.put(image, next);
            }
            Command::EnableImage {
                image,
                source,
                size,
                color,
            } => {
                let source = source
                    .as_ref()
                    .map(|source| self.image(source))
                    .transpose()?;
                let next = self
                    .gpu
                    .enable_image(source, *size, *color)
                    .map_err(|e| e.to_string())?;
                self.put(image, next);
            }
            Command::CreateProvince {
                image,
                size,
                operation,
            } => {
                let mut next = self.gpu.create_province(*size).map_err(|e| e.to_string())?;
                match operation {
                    ProvinceOperation::Reserve => Ok(()),
                    ProvinceOperation::Fill(fill) => self.gpu.fill(&mut next, &[*fill]),
                    ProvinceOperation::Copy {
                        source,
                        rectangle,
                        x,
                        y,
                        clip,
                    } => self.gpu.copy_rect(
                        &mut next,
                        self.image(source)?,
                        *rectangle,
                        *x,
                        *y,
                        *clip,
                        DrawFace::Province,
                        false,
                    ),
                }
                .map_err(|e| e.to_string())?;
                self.put(image, next);
            }
            Command::AssignBitmap {
                image,
                source,
                pixels,
            } => {
                let source = source
                    .as_ref()
                    .map(|source| self.image(source))
                    .transpose()?;
                let next = self
                    .gpu
                    .assign_bitmap(source, pixels)
                    .map_err(|e| e.to_string())?;
                self.put(image, next);
            }
            Command::PrepareUpload {
                image,
                size,
                main,
                province,
                source,
            } => {
                let next = if let Some(source) = source {
                    self.gpu
                        .prepare_upload(self.image(source)?, *size, *main, *province)
                } else {
                    self.gpu.reserve_upload(*size, *main, *province)
                }
                .map_err(|e| e.to_string())?;
                self.put(image, next);
            }
            Command::BeginUpload {
                image,
                size,
                main,
                province,
                source,
                ..
            } => {
                let source = source.as_ref().map(|s| self.image(s)).transpose()?;
                let pending = self
                    .gpu
                    .begin_upload(source, *size, *main, *province)
                    .map_err(|e| e.to_string())?;
                self.uploads.insert(image.id, pending);
                self.lifetimes
                    .insert(image.id, Arc::downgrade(&image.lifetime));
            }
            Command::Upload { image, pixels } => {
                if let Some(pending) = self.uploads.remove(&image.id) {
                    let next = pending
                        .complete(&self.gpu, pixels, None)
                        .map_err(|e| e.to_string())?;
                    let bytes = next.resident_bytes();
                    self.put(image, next);
                    return Ok(Response::ImageStorage(bytes));
                }
                self.write(image, |gpu, target| gpu.upload(target, pixels))?
            }
            Command::UploadScaled {
                image,
                pixels,
                logical_size,
            } => {
                if let Some(pending) = self.uploads.remove(&image.id) {
                    let next = pending
                        .complete(&self.gpu, pixels, Some(*logical_size))
                        .map_err(|e| e.to_string())?;
                    let bytes = next.resident_bytes();
                    self.put(image, next);
                    return Ok(Response::ImageStorage(bytes));
                }
                self.write(image, |gpu, target| {
                    gpu.upload_scaled_into(target, pixels, *logical_size)
                })?;
            }
            Command::UploadYuv {
                image,
                pixels,
                logical_size,
            } => {
                self.image(image)?;
                let next = self
                    .gpu
                    .upload_yuv(pixels, *logical_size)
                    .map_err(|e| e.to_string())?;
                self.put(image, next);
            }
            Command::CopyYuv {
                image,
                pixels,
                logical_size,
                split_alpha,
                size,
            } => {
                self.write(image, |gpu, target| {
                    gpu.copy_yuv(target, pixels, *logical_size, *split_alpha, *size)
                })?;
            }
            Command::PatchPixels { image, pixels } => {
                self.write(image, |gpu, target| gpu.patch_pixels(target, pixels))?
            }
            Command::PatchRegion {
                image,
                rectangle,
                pixels,
            } => self.write(image, |gpu, target| {
                gpu.patch_region(target, *rectangle, pixels)
            })?,
            Command::Independ {
                image,
                province,
                copy,
            } => self.write(image, |gpu, target| gpu.independ(target, *province, *copy))?,
            Command::Fill { image, fills } => self
                .write(image, |gpu, target| gpu.fill(target, fills))
                .map_err(|error| {
                    let target = self.image(image).ok();
                    format!(
                        "{error}; fill target={:?}/{:?} count={} first={:?}",
                        target.map(|i| i.size),
                        target.and_then(Image::stored_size),
                        fills.len(),
                        &fills[..fills.len().min(4)],
                    )
                })?,
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
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| {
                    gpu.copy_rect(
                        target,
                        &source,
                        *rectangle,
                        *x,
                        *y,
                        *clip,
                        *face,
                        *hold_alpha,
                    )
                })
                .map_err(|error| {
                    let target = self.image(image).ok();
                    format!(
                        "{error}; copy source={:?}/{:?} target={:?}/{:?} rect={rectangle:?} destination=({x},{y}) clip={clip:?} face={face:?} hold_alpha={hold_alpha}",
                        source.size,
                        source.stored_size(),
                        target.map(|i| i.size),
                        target.and_then(Image::stored_size),
                    )
                })?;
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
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| {
                    gpu.operate(target, &source, *rectangle, *x, *y, *clip, *options)
                })?;
            }
            Command::Color {
                image,
                rectangle,
                color,
                opacity,
                face,
            } => self.write(image, |gpu, target| {
                gpu.color(target, *rectangle, *color, *opacity, *face)
            })?,
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
                let available = self.gpu.resident.available();
                let source = self.image(source)?.shared();
                self.write(image, |gpu, target| {
                    gpu.transform(
                        target, &source, *rectangle, *transform, *sampling, *operation, *clip,
                        *clear,
                    )
                })
                .map_err(|error| {
                    let target = self.images.get(&image.id);
                    format!(
                        "{error}; admission={allocation} available={available}; source={:?}/{:?} rectangle={rectangle:?} target={:?}/{:?} transform={transform:?} sampling={sampling:?} operation={operation:?} clip={clip:?} clear={clear:?}",
                        source.size,
                        source.stored_size(),
                        target.map(|i| i.size),
                        target.and_then(Image::stored_size),
                    )
                })?;
            }
            Command::Text {
                image,
                run,
                style,
                clip,
            } => self.write(image, |gpu, target| {
                gpu.draw_text(target, run, *style, *clip)
            })?,
            Command::ComposeScene { image, size, scene } => {
                let target = self
                    .gpu
                    .scene_image(*size, scene, &self.images, (0, 0))
                    .map_err(|e| e.to_string())?;
                self.put(image, target);
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
                let Some((source, destination)) = krkr_render::blit::region(
                    *size,
                    self.image(image)?.size,
                    *clip,
                    *rectangle,
                    *x,
                    *y,
                ) else {
                    return Ok(Response::Done);
                };
                if source == size.rect()
                    && destination == self.image(image)?.size.rect()
                    && let Some(next) = self
                        .gpu
                        .scene_canvas_replacement(self.image(image)?, scene, &self.images)
                        .map_err(|e| e.to_string())?
                {
                    self.put(image, next);
                    return Ok(Response::Done);
                }
                let size = krkr_protocol::graphics::Size {
                    width: source.width,
                    height: source.height,
                };
                let snapshot = self
                    .gpu
                    .scene_surface(size, scene, &self.images, (source.left, source.top))
                    .map_err(|e| e.to_string())?;
                self.write(image, |gpu, target| {
                    gpu.copy_rect(
                        target,
                        &snapshot,
                        size.rect(),
                        destination.left,
                        destination.top,
                        *clip,
                        DrawFace::Alpha,
                        false,
                    )
                })?;
            }
            Command::Pixel {
                image,
                x,
                y,
                province,
            } => {
                let target = self.image(image)?;
                if *province
                    && (!target.has_province()
                        || *x < 0
                        || *y < 0
                        || *x as u32 >= target.size.width
                        || *y as u32 >= target.size.height)
                {
                    return Ok(Response::Pixel(0));
                }
                return self
                    .gpu
                    .pixel(self.image(image)?, *x, *y, *province)
                    .map(Response::Pixel)
                    .map_err(|e| e.to_string());
            }
            Command::ReadHitPlane { image, province } => {
                return self
                    .gpu
                    .read_hit_plane(self.image(image)?, *province)
                    .map(Response::HitPlane)
                    .map_err(|e| e.to_string());
            }
            Command::ReadImage { image }
            | Command::ReadProvince { image }
            | Command::ReadRegion { image, .. } => {
                let target = self.image(image)?;
                let province = matches!(command, Command::ReadProvince { .. });
                if province && !target.has_province() {
                    return Ok(Response::Image(krkr_protocol::pixels::Pixels {
                        size: target.size,
                        main: None,
                        province: None,
                    }));
                }
                let read = self
                    .gpu
                    .readback(
                        target,
                        match command {
                            Command::ReadRegion { rectangle, .. } => *rectangle,
                            _ => target.size.rect(),
                        },
                        province,
                    )
                    .map_err(|e| e.to_string())?;
                let (main, province) = if province {
                    (None, Some(read.data))
                } else {
                    (Some(read.data), None)
                };
                return Ok(Response::Image(krkr_protocol::pixels::Pixels {
                    size: read.size,
                    main,
                    province,
                }));
            }
            Command::PreparedDraw(batch) => {
                if !self
                    .draw_grant
                    .take()
                    .is_some_and(|lease| Weak::ptr_eq(&lease, &batch.lifetime()))
                    || batch.source().is_some()
                    || batch.draws().len() > DRAW_BATCH_CAPACITY
                {
                    return Err("GLES host received a draw grant it did not issue".into());
                }
                let target = self.image(batch.image())?;
                if target.size != batch.size()
                    || !target.has_main()
                    || batch.draws().iter().any(|draw| match draw {
                        krkr_protocol::graphics::PreparedDraw::Color(c) => {
                            self.gpu.color_write_bytes(
                                target,
                                c.rectangle,
                                c.color,
                                c.opacity,
                                c.face,
                            ) != 0
                        }
                        krkr_protocol::graphics::PreparedDraw::Fill(_) => {
                            self.gpu.canvas_write_bytes(target, false) != 0
                        }
                        _ => true,
                    })
                {
                    return Err("prepared draw target changed before its fence".into());
                }
                self.write(batch.image(), |gpu, target| {
                    gpu.prepared_draws(target, batch.draws())
                })?;
            }
            Command::Adjust {
                image,
                rectangle,
                operation,
            } => {
                self.write(image, |gpu, target| {
                    gpu.adjust(target, *rectangle, operation)
                })
                .map_err(|error| {
                    let target = self.images.get(&image.id);
                    let kind = match operation {
                        krkr_protocol::graphics::Adjustment::BoxBlur { radius, alpha } => {
                            format!("BoxBlur radius={radius:?} alpha={alpha}")
                        }
                        krkr_protocol::graphics::Adjustment::Filter(filter) => {
                            format!("Filter {:?}", filter.kind)
                        }
                        other => format!("{:?}", std::mem::discriminant(other)),
                    };
                    format!("{error}; adjustment={kind} target={target:?} rect={rectangle:?}")
                })?;
            }
        }
        Ok(Response::Done)
    }
}

fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Sprites { .. } => "Sprites",
        Command::Scanlines { .. } => "Scanlines",
        Command::Warp { .. } => "Warp",
        Command::PreparedDraw(..) => "PreparedDraw",
        Command::SnapshotMain { .. } => "SnapshotMain",
        Command::ComposeScene { .. } => "ComposeScene",
        Command::Meshes { .. } => "Meshes",
        Command::CopyPixels { .. } => "CopyPixels",
        Command::Perspective { .. } => "Perspective",
        Command::WrappedCopy { .. } => "WrappedCopy",
        Command::PiledCopy { .. } => "PiledCopy",
        Command::Adjust { .. } => "Adjust",
        Command::Text { .. } => "Text",
        Command::Transform { .. } => "Transform",
        Command::Color { .. } => "Color",
        Command::Operate { .. } => "Operate",
        Command::EnableImage { .. } => "EnableImage",
        Command::CreateProvince { .. } => "CreateProvince",
        Command::Assign { .. } => "Assign",
        Command::AssignBitmap { .. } => "AssignBitmap",
        Command::LoadCompressed { .. } => "LoadCompressed",
        Command::Independ { .. } => "Independ",
        Command::PrepareUpload { .. } => "PrepareUpload",
        Command::BeginUpload { .. } => "BeginUpload",
        Command::Upload { .. } => "Upload",
        Command::UploadScaled { .. } => "UploadScaled",
        Command::UploadYuv { .. } => "UploadYuv",
        Command::CopyYuv { .. } => "CopyYuv",
        Command::PatchPixels { .. } => "PatchPixels",
        Command::PatchRegion { .. } => "PatchRegion",
        Command::Create { .. } => "Create",
        Command::Resize { .. } => "Resize",
        Command::Fill { .. } => "Fill",
        Command::Copy { .. } => "Copy",
        Command::Pixel { .. } => "Pixel",
        Command::ReadImage { .. } => "ReadImage",
        Command::ReadRegion { .. } => "ReadRegion",
        Command::ReadProvince { .. } => "ReadProvince",
        Command::ReadHitPlane { .. } => "ReadHitPlane",
    }
}

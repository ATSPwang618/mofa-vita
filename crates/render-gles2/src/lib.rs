//! OpenGL ES 2.0 GPU rendering. Images retain textures, never shadow RGBA copies.
//! Logical images may exceed the device's texture limit: storage is tiled and
//! compact uploads and display-density canvases retain their script coordinates.
mod adjust;
mod adjust_cache;
mod box_blur;
mod box_blur_stream;
mod canvas_reclaim;
mod canvas_spill;
pub use canvas_spill::SpilledImage;
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/internal/cache_reuse.rs"]
mod cache_reuse_tests;
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/internal/canvas_strips.rs"]
mod canvas_strip_tests;
mod canvas_tiles;
mod color_batch;
mod compressed;
mod copies;
mod copy_cache;
mod device;
mod draw_bounds;
mod draw_programs;
mod draw_source;
mod drawing;
mod fills;
mod image;
mod image_filter;
mod lines;
mod mesh;
mod pending_upload;
mod perspective;
mod pixel_cache;
mod pixels;
mod resample;
mod resample_source;
mod scanlines;
mod scene;
mod scene_batch;
mod scene_batch_source;
mod scene_batch_tiles;
mod scene_cache;
mod scene_damage;
mod scene_flatten;
mod solid_cache;
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/support/mod.rs"]
mod test_support;
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/support/traffic.rs"]
mod test_traffic;
pub use scene_damage::SceneState;
mod shader;
mod shader_binary;
mod text;
mod text_tile;
mod tiles;
mod transform;
mod transition;
mod transition_source;
mod video;
mod warp;
pub use image::Image;
use krkr_protocol::{budget::Budget, graphics::Size};
pub use krkr_render::{Error, Result};
pub use mesh::PreparedMeshes;
pub use pending_upload::PendingUpload;
pub use pixels::ReadPixels;
use std::rc::Rc;

pub struct Config {
    pub resident: Budget,
    pub scratch: Budget,
    pub staging: Budget,
    /// At most one tile is copied for a local edit. Ordinary converted Vita
    /// images fit in one 1024x1024 tile; long strips span several textures.
    pub tile_edge: u32,
    /// Use an independent work surface for destination-reading passes. Reused
    /// canvases may also use the bounded texture FBO cache below. Box blur owns
    /// two small texture-backed FBOs for its alternating intermediate sums.
    pub work_framebuffer: bool,
    /// Cache native texture FBOs alongside the work surface for reused canvases.
    /// The byte cap covers attached image payloads (already in their image
    /// budgets); driver render-surface allocations consume additional memory.
    /// Slots remain charged until their texture allocations are destroyed.
    /// With two or more slots, half the slots and bytes are reserved for
    /// scratch targets so persistent canvases cannot crowd out composition.
    pub render_target_cache_entries: usize,
    pub render_target_cache_bytes: usize,
    /// Script canvases use this display's pixel density. Text and its bitmap
    /// copies keep logical pixels until composition; uploads keep their supplied
    /// dimensions, and province data retains exact logical pixels.
    pub canvas_limit: Option<Size>,
    /// Compose the final scene at canvas density as well. Leave disabled to
    /// retain screen-density text and uploaded UI while reducing effect canvases.
    pub compact_scene: bool,
    /// Sharpen reduced effect canvases during display upsampling.
    pub effect_sharpen: bool,
    /// Keep small script canvases at logical resolution to avoid resampling
    /// glyph layers twice. The area limit is edge squared, with either side
    /// at most four edges, so narrow buttons get the same bounded protection.
    /// Zero disables this size-based exception; text retains logical pixels.
    pub small_canvas_edge: u32,
}
impl Default for Config {
    fn default() -> Self {
        let textures = Budget::new(96 * 1024 * 1024);
        Self {
            resident: textures.child(80 * 1024 * 1024),
            scratch: textures.child(16 * 1024 * 1024),
            staging: Budget::new(32 * 1024 * 1024),
            tile_edge: 1024,
            work_framebuffer: cfg!(target_os = "vita"),
            render_target_cache_entries: 0,
            render_target_cache_bytes: 0,
            canvas_limit: cfg!(target_os = "vita").then_some(Size {
                width: 960,
                height: 544,
            }),
            small_canvas_edge: if cfg!(target_os = "vita") { 64 } else { 0 },
            compact_scene: false,
            effect_sharpen: false,
        }
    }
}
pub struct Gpu {
    pub(crate) device: Rc<device::Device>,
    pub(crate) program: draw_programs::Programs,
    pub(crate) scene_batch_programs: std::cell::RefCell<scene_batch::Programs>,
    pub(crate) fill_program: std::cell::RefCell<Option<shader::Program>>,
    pub(crate) glyph_batch_program: std::cell::RefCell<Option<Rc<shader::Program>>>,
    pub(crate) glyph_pair_program: std::cell::RefCell<Option<Rc<shader::Program>>>,
    pub(crate) filter_program: std::cell::RefCell<Option<shader::Program>>,
    pub(crate) filter_axes: std::cell::RefCell<krkr_render::resample::Cache>,
    pub(crate) direct_filter_program:
        std::cell::RefCell<[Option<shader::Program>; resample_source::TAPS.len()]>,
    pub(crate) scanline_program: std::cell::RefCell<Option<shader::Program>>,
    pub(crate) perspective_program: std::cell::RefCell<Option<shader::Program>>,
    pub(crate) meshes: std::cell::RefCell<Option<mesh::Renderer>>,
    pub(crate) warps: std::cell::RefCell<warp::Renderer>,
    pub(crate) adjustments: std::cell::RefCell<adjust::Renderer>,
    pub(crate) adjusted_images: std::cell::RefCell<adjust_cache::Cache>,
    pub(crate) solid_images: std::cell::RefCell<solid_cache::Cache>,
    pub(crate) canvas_solids: std::cell::RefCell<canvas_tiles::Solids>,
    pub(crate) copied_images: std::cell::RefCell<copy_cache::Cache>,
    pub(crate) pixel_reads: std::cell::RefCell<pixel_cache::Cache>,
    pub(crate) line_program: std::cell::RefCell<Option<shader::Program>>,
    pub(crate) box_blur_programs: std::cell::RefCell<Option<[shader::Program; 2]>>,
    pub(crate) small_box_blur_program: std::cell::RefCell<Option<shader::Program>>,
    pub(crate) packed_box_blur_programs: std::cell::RefCell<Option<[shader::Program; 2]>>,
    pub(crate) box_blur_targets: std::cell::RefCell<Option<[Rc<device::Texture>; 2]>>,
    pub(crate) box_blur_native_disabled: std::cell::Cell<bool>,
    pub(crate) image_filters: std::cell::RefCell<image_filter::Renderer>,
    pub(crate) video: std::cell::RefCell<video::Renderer>,
    pub(crate) atlas: std::cell::RefCell<text::Atlas>,
    pub(crate) text_tiles: std::cell::RefCell<text_tile::Cache>,
    pub(crate) transitions: std::cell::RefCell<transition::Transitions>,
    pub(crate) scene_cache: std::cell::RefCell<scene_cache::Cache>,
    pub(crate) flattened_images: std::cell::RefCell<scene_flatten::Cache>,
    pub(crate) lookup: Rc<device::Texture>,
    pub resident: Budget,
    pub scratch: Budget,
    pub staging: Budget,
    pub(crate) tile_edge: u32,
    pub(crate) canvas_limit: Option<Size>,
    compact_scene: bool,
    pub(crate) effect_sharpen: bool,
    pub(crate) display_scene_blend: std::cell::Cell<bool>,
    pub(crate) small_canvas_edge: u32,
    pub(crate) canvas_scale: std::cell::Cell<f64>,
}
impl Gpu {
    /// # Safety
    /// The supplied GLES context must be current on this thread for every call
    /// and for destruction of this renderer and all its images. The native EGL
    /// context must outlive them. No other renderer may change its state during
    /// a call. Images and this renderer deliberately implement neither Send nor Sync.
    pub unsafe fn new(gl: glow::Context, config: Config) -> Result<Self> {
        config.resident.set_profile_name("memory.resident_bytes");
        config.scratch.set_profile_name("memory.scratch_bytes");
        config.staging.set_profile_name("memory.staging_bytes");
        let device = device::Device::new(
            gl,
            config.work_framebuffer.then_some(config.tile_edge.max(1)),
            config.scratch.clone(),
            config.staging.clone(),
            config.render_target_cache_entries,
            config.render_target_cache_bytes,
        )?;
        if config.tile_edge == 0 {
            return Err(Error::Message("GLES tile size must be positive"));
        }
        let program = draw_programs::Programs::new(device.clone())?;
        let lookup = device.sample_texture(
            Size {
                width: 256,
                height: 321,
            },
            &config.resident,
        )?;
        let staging = config.staging.reserve(256 * 321 * 4)?;
        device.upload(&lookup, &krkr_render::blend::lookup_table())?;
        drop(staging);
        let tile_edge = config.tile_edge.min(device.max_texture);
        Ok(Self {
            device,
            program,
            scene_batch_programs: std::cell::RefCell::new(scene_batch::Programs::default()),
            fill_program: std::cell::RefCell::new(None),
            glyph_batch_program: std::cell::RefCell::new(None),
            glyph_pair_program: std::cell::RefCell::new(None),
            filter_program: std::cell::RefCell::new(None),
            filter_axes: Default::default(),
            direct_filter_program: std::cell::RefCell::new(std::array::from_fn(|_| None)),
            scanline_program: std::cell::RefCell::new(None),
            perspective_program: std::cell::RefCell::new(None),
            meshes: std::cell::RefCell::new(None),
            warps: std::cell::RefCell::new(warp::Renderer::default()),
            adjustments: std::cell::RefCell::new(adjust::Renderer::default()),
            adjusted_images: std::cell::RefCell::new(adjust_cache::Cache::default()),
            solid_images: std::cell::RefCell::new(solid_cache::Cache::default()),
            canvas_solids: std::cell::RefCell::new(canvas_tiles::Solids::default()),
            copied_images: std::cell::RefCell::new(copy_cache::Cache::default()),
            pixel_reads: std::cell::RefCell::new(pixel_cache::Cache::default()),
            line_program: std::cell::RefCell::new(None),
            box_blur_programs: std::cell::RefCell::new(None),
            small_box_blur_program: std::cell::RefCell::new(None),
            packed_box_blur_programs: std::cell::RefCell::new(None),
            box_blur_targets: std::cell::RefCell::new(None),
            box_blur_native_disabled: std::cell::Cell::new(false),
            image_filters: std::cell::RefCell::new(image_filter::Renderer::default()),
            video: std::cell::RefCell::new(video::Renderer::default()),
            atlas: std::cell::RefCell::new(text::Atlas::default()),
            text_tiles: std::cell::RefCell::new(text_tile::Cache::default()),
            transitions: std::cell::RefCell::new(transition::Transitions::default()),
            scene_cache: std::cell::RefCell::new(scene_cache::Cache::default()),
            flattened_images: Default::default(),
            lookup,
            resident: config.resident,
            scratch: config.scratch,
            staging: config.staging,
            tile_edge,
            canvas_limit: config.canvas_limit,
            compact_scene: config.compact_scene,
            effect_sharpen: config.effect_sharpen,
            display_scene_blend: std::cell::Cell::new(false),
            small_canvas_edge: config.small_canvas_edge,
            canvas_scale: std::cell::Cell::new(1.0),
        })
    }
    /// Flush ordered drawing before publishing a scene. This does not perform a readback.
    pub fn flush(&self) -> Result<()> {
        self.device.flush()
    }
    /// Resolve the work surface before the host's EGL swap submits the frame.
    /// Avoid a separate glFlush/render kick for each internal composition.
    pub fn resolve(&self) -> Result<()> {
        self.device.resolve()
    }
    /// Reclaim retired texture permits only after GPU completion. Called after
    /// swap or under admission pressure; never on each drawing request.
    pub fn collect(&self) -> Result<()> {
        self.clear_caches();
        self.device.collect()
    }
    /// Release retired surfaces and wait for in-flight driver allocations,
    /// including backing copies not charged to the texture budgets.
    pub fn collect_under_pressure(&self) -> Result<()> {
        self.clear_caches();
        self.device.collect_under_pressure()
    }
    fn clear_caches(&self) {
        self.filter_axes.borrow_mut().clear();
        self.canvas_solids.borrow_mut().trim();
        self.scene_cache.borrow_mut().clear();
        self.flattened_images.borrow_mut().clear();
        self.solid_images.borrow_mut().trim();
        self.adjusted_images.borrow_mut().trim();
        self.copied_images.borrow_mut().trim();
        self.text_tiles.borrow_mut().trim();
        self.box_blur_targets.borrow_mut().take();
    }
    /// Reuse dead texture allocations between frames within their original budgets.
    pub fn maintain(&self) -> Result<()> {
        self.scene_cache.borrow_mut().trim();
        self.flattened_images.borrow_mut().trim();
        self.solid_images.borrow_mut().trim();
        self.adjusted_images.borrow_mut().trim();
        self.copied_images.borrow_mut().trim();
        self.text_tiles.borrow_mut().trim();
        self.device.maintain()
    }
    /// Scratch storage available after reusing/reclaiming retired surfaces.
    /// Live images and cached subtrees remain charged. This is admission
    /// capacity, not permission to release storage before GPU completion.
    pub fn scratch_capacity(&self) -> usize {
        self.capacity_after_collect(&self.scratch)
    }
    /// Estimate admission after retiring dead resources in all shared pools.
    /// No GPU wait, deletion or permit release happens during this query.
    pub fn capacity_after_collect(&self, budget: &Budget) -> usize {
        self.device.capacity_after_collect(budget)
    }
    /// Called for the primary script window, never for panel/fullscreen size.
    pub fn set_canvas_size(&self, logical: Size) {
        if let Some(limit) = self.canvas_limit
            && logical.width != 0
            && logical.height != 0
        {
            let scale = (f64::from(limit.width) / f64::from(logical.width))
                .min(f64::from(limit.height) / f64::from(logical.height))
                .min(1.0);
            self.canvas_scale.set(scale);
        }
    }
    pub fn max_texture_size(&self) -> u32 {
        self.device.max_texture
    }
    /// Keep scene composition on the same grid as script canvases. The window
    /// viewport and input coordinates still use the original display geometry.
    pub fn scene_storage_size(&self, logical: Size, display: Size) -> Size {
        if !self.compact_scene {
            return display;
        }
        let stored = self.canvas_storage(logical, None);
        Size {
            width: stored.width.min(display.width),
            height: stored.height.min(display.height),
        }
    }
    pub(crate) fn check_image(&self, image: &Image) -> Result<()> {
        if !Rc::ptr_eq(&self.device, &image.device) {
            return Err(Error::Message("image belongs to another GLES context"));
        }
        Ok(())
    }
}

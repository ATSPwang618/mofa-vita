#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/device/copies.rs"]
mod copy_tests;
mod draw_state;
mod targets;
#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../tests/device/upload_lifetime.rs"]
mod upload_tests;
mod work;
use crate::{Error, Result};
use glow::HasContext;
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::{Rect, Size},
};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

pub(crate) struct Device {
    pub gl: glow::Context,
    pub max_texture: u32,
    pub quad: glow::NativeBuffer,
    pub draw_state: draw_state::DrawState,
    // Cache the compiled vertex stage for linking subsequent programs.
    pub quad_vertex: Cell<Option<glow::NativeShader>>,
    pub shader_binaries: crate::shader_binary::Loader,
    retired: RefCell<Vec<Retired>>,
    recycled: RefCell<Vec<Allocation>>,
    work: RefCell<Option<work::Work>>,
    targets: RefCell<targets::Targets>,
    staging: Budget,
}
fn tile_shapes(size: Size, edge: u32) -> smallvec::SmallVec<[(Size, usize); 4]> {
    let mut shapes = smallvec::SmallVec::new();
    // A regular tile grid has at most four shapes, including its right and
    // bottom edges. Count them without visiting every tile in a large canvas.
    for (width, columns) in [(edge, size.width / edge), (size.width % edge, 1)] {
        for (height, rows) in [(edge, size.height / edge), (size.height % edge, 1)] {
            if width != 0 && height != 0 && columns != 0 && rows != 0 {
                shapes.push((Size { width, height }, columns as usize * rows as usize));
            }
        }
    }
    shapes
}
fn storage_bytes(size: Size, format: u32) -> Option<usize> {
    match format {
        0x8d64 => krkr_protocol::texture::Format::Etc1.byte_len(size),
        0x8c02 => krkr_protocol::texture::Format::Pvrtc1Rgba4.byte_len(size),
        0x83f0 => krkr_protocol::texture::Format::Bc1Rgb.byte_len(size),
        0x83f3 => krkr_protocol::texture::Format::Bc3Rgba.byte_len(size),
        0x6000_0001 => krkr_protocol::texture::Format::Bc1RgbVita.byte_len(size),
        0x6000_0002 => krkr_protocol::texture::Format::Bc3RgbaVita.byte_len(size),
        glow::LUMINANCE => size.rgba_bytes().map(|bytes| bytes / 4),
        glow::LUMINANCE_ALPHA => size.rgba_bytes().map(|bytes| bytes / 2),
        _ => size.rgba_bytes(),
    }
}
enum Retired {
    Texture(Allocation),
    Evicted(Allocation),
    Buffer(glow::NativeBuffer, Permit, usize),
}
#[derive(Clone, Copy)]
enum TextureData<'a> {
    Pixels(&'a [u8]),
    Compressed(&'a [u8]),
}
struct Allocation {
    texture: glow::NativeTexture,
    framebuffer: Option<glow::NativeFramebuffer>,
    size: Size,
    format: u32,
    permit: Permit,
}
impl Allocation {
    fn bytes(&self) -> usize {
        storage_bytes(self.size, self.format).unwrap()
    }
    fn matches(&self, size: Size, format: u32, budget: &Budget) -> bool {
        // Native scratch targets have a separate, bounded owner. Never reuse
        // their attachments as ordinary images in the shared work surface.
        self.framebuffer.is_none()
            && self.size == size
            && self.format == format
            && self.permit.belongs_to(budget)
    }
}
pub(crate) struct Buffer {
    device: Rc<Device>,
    pub name: glow::NativeBuffer,
    permit: Option<Permit>,
    bytes: usize,
}
impl Drop for Buffer {
    fn drop(&mut self) {
        self.device.retired.borrow_mut().push(Retired::Buffer(
            self.name,
            self.permit.take().expect("buffer permit"),
            self.bytes,
        ));
    }
}
pub(crate) struct Texture {
    pub device: Rc<Device>,
    texture: glow::NativeTexture,
    framebuffer: Cell<Option<glow::NativeFramebuffer>>,
    weak: Weak<Texture>,
    format: u32,
    pub size: Size,
    pub generation: Cell<u64>,
    // A newly allocated, unpublished texture can be filled in place. Recycled
    // textures may still have GPU readers even when their generation is zero.
    unused_storage: Cell<bool>,
    pub(crate) border_scan_generation: Cell<Option<u64>>,
    // A uniform background plus conservative bounds of subsequent writes.
    // Clearing those bounds restores a solid tile without a GPU readback.
    solid: Cell<Option<(u32, Option<Rect>)>>,
    alpha: Cell<Option<u8>>,
    writes: RefCell<Rc<[(u64, Rect); 16]>>,
    copies: RefCell<[Option<CopiedFrom>; 4]>,
    permit: Option<Permit>,
}
struct CopiedFrom {
    texture: Weak<Texture>,
    source_generation: u64,
    source_writes: Rc<[(u64, Rect); 16]>,
    generation: u64,
    damage: Option<Rect>,
}
impl Texture {
    pub fn renderable(&self) -> bool {
        self.format == glow::RGBA
    }
    pub fn allocation_bytes(&self) -> usize {
        storage_bytes(self.size, self.format).unwrap()
    }
    pub fn belongs_to(&self, budget: &Budget) -> bool {
        self.permit.as_ref().is_some_and(|p| p.belongs_to(budget))
    }
    fn changed(&self, area: Rect) {
        self.unused_storage.set(false);
        self.alpha.set(None);
        if let Some((color, damage)) = self.solid.get() {
            self.solid.set(Some((
                color,
                Some(crate::scene_damage::union(damage, area)),
            )));
        }
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        Rc::make_mut(&mut self.writes.borrow_mut())[generation as usize % 16] = (generation, area);
    }
    pub(crate) fn solid_color(&self) -> Option<u32> {
        self.solid
            .get()
            .and_then(|(color, damage)| damage.is_none().then_some(color))
    }
    pub(crate) fn uniform_alpha(&self) -> Option<u8> {
        self.alpha.get()
    }
    pub(crate) fn set_uniform_alpha(&self, alpha: u8) {
        self.alpha.set(Some(alpha));
    }
    pub(crate) fn constant_background(&self) -> Option<(u32, Option<Rect>)> {
        self.solid.get()
    }
    /// After a whole-tile point kernel, only the previous nonuniform pixels
    /// can differ from its transformed background. The caller has already
    /// submitted the draw and recorded its full write generation.
    pub(crate) fn point_background(&self, color: u32, damage: Option<Rect>) {
        self.solid.set(Some((color, damage)));
    }
    pub(crate) fn solid_region(&self, area: Rect) -> Option<u32> {
        self.solid.get().and_then(|(color, damage)| {
            damage
                .is_none_or(|damage| damage.intersection(area).is_none())
                .then_some(color)
        })
    }
    pub(crate) fn cleared(&self, area: Rect, color: u32) {
        if area == self.size.rect()
            || self.solid.get().is_some_and(|(old, damage)| {
                old == color && damage.is_none_or(|d| d.intersection(area) == Some(d))
            })
        {
            self.solid.set(Some((color, None)));
        }
    }
    pub(crate) fn damage_since(&self, generation: u64) -> Option<Rect> {
        Self::written_damage(
            self.size,
            &self.writes.borrow(),
            self.generation.get(),
            generation,
        )
    }
    fn written_damage(
        size: Size,
        writes: &[(u64, Rect); 16],
        current: u64,
        generation: u64,
    ) -> Option<Rect> {
        if generation == current {
            return None;
        }
        let Some(distance) = current.checked_sub(generation).filter(|n| *n <= 16) else {
            return Some(size.rect());
        };
        let mut damage = None;
        for offset in 1..=distance {
            let version = generation + offset;
            let (stored, area) = writes[version as usize % 16];
            if stored != version {
                return Some(size.rect());
            }
            damage = Some(crate::scene_damage::union(damage, area));
        }
        damage
    }
    /// Compare pixels across a full-copy replacement as well as in-place writes.
    /// Weak origins retain no GPU storage; missing/expired history stays conservative.
    pub(crate) fn damage_from(&self, previous: &Weak<Texture>, generation: u64) -> Option<Rect> {
        if Weak::ptr_eq(&self.weak, previous) {
            return self.damage_since(generation);
        }
        let copies = self.copies.borrow();
        let Some(origin) = copies
            .iter()
            .flatten()
            .find(|origin| Weak::ptr_eq(&origin.texture, previous))
        else {
            return Some(self.size.rect());
        };
        // The last displayed version may predate in-place edits made before
        // this copy. Preserve that bounded history even after the source dies;
        // retaining only its generation turned several glyphs into a full-page
        // repaint as soon as a second scene was submitted in the same frame.
        [
            Self::written_damage(
                self.size,
                &origin.source_writes,
                origin.source_generation,
                generation,
            ),
            origin.damage,
            self.damage_since(origin.generation),
        ]
        .into_iter()
        .flatten()
        .reduce(|a, b| crate::scene_damage::union(Some(a), b))
    }
    fn copied_from(&self, source: &Texture) {
        self.solid.set(source.solid.get());
        let generation = self.generation.get();
        let mut copies = [const { None }; 4];
        copies[0] = Some(CopiedFrom {
            texture: source.weak.clone(),
            source_generation: source.generation.get(),
            source_writes: source.writes.borrow().clone(),
            generation,
            damage: None,
        });
        for (slot, origin) in copies[1..]
            .iter_mut()
            .zip(source.copies.borrow().iter().flatten())
        {
            let damage = match source.damage_since(origin.generation) {
                Some(area) => Some(crate::scene_damage::union(origin.damage, area)),
                None => origin.damage,
            };
            *slot = Some(CopiedFrom {
                texture: origin.texture.clone(),
                source_generation: origin.source_generation,
                source_writes: origin.source_writes.clone(),
                generation,
                damage,
            });
        }
        *self.copies.borrow_mut() = copies;
    }
    pub fn name(&self) -> glow::NativeTexture {
        self.texture
    }
    pub fn framebuffer(&self) -> Result<glow::NativeFramebuffer> {
        self.framebuffer_region(self.size.rect())
    }
    pub fn framebuffer_region(&self, area: Rect) -> Result<glow::NativeFramebuffer> {
        self.device.framebuffer(self, Some(area), false, false)
    }
    /// Every channel in this rectangle will be replaced, without discard or
    /// destination sampling. Pixels outside it must still survive.
    pub fn overwrite_framebuffer(&self, area: Rect) -> Result<glow::NativeFramebuffer> {
        self.device.framebuffer(self, Some(area), true, false)
    }
    /// Video replaces a complete frame on every write. Admit it to the same
    /// bounded FBO cache immediately instead of resolving through the work FBO.
    pub fn video_framebuffer(&self) -> Result<glow::NativeFramebuffer> {
        self.device
            .framebuffer(self, Some(self.size.rect()), true, true)
    }
    pub fn read_framebuffer(&self) -> Result<glow::NativeFramebuffer> {
        self.device.framebuffer(self, None, false, false)
    }
    /// Load only the requested read region, without recording a pixel write.
    pub fn read_framebuffer_region(&self, area: Rect) -> Result<glow::NativeFramebuffer> {
        if let Some(framebuffer) = self.framebuffer.get() {
            return Ok(framebuffer);
        }
        if let Some(work) = self.device.work.borrow_mut().as_mut() {
            if !work.contains(self)
                && let Some(framebuffer) = self.device.targets.borrow_mut().get(self.name())
            {
                return Ok(framebuffer);
            }
            return work.prepare(&self.device, self, area);
        }
        self.device.native_framebuffer(self)
    }
    fn bytes(&self, size: Size) -> Option<usize> {
        storage_bytes(size, self.format)
    }
}
impl Drop for Texture {
    fn drop(&mut self) {
        self.device
            .retired
            .borrow_mut()
            .push(Retired::Texture(Allocation {
                texture: self.texture,
                framebuffer: self.framebuffer.get(),
                size: self.size,
                format: self.format,
                permit: self.permit.take().expect("texture permit"),
            }));
    }
}
impl Device {
    pub fn capacity_after_collect(&self, budget: &Budget) -> usize {
        let retired = self.retired.borrow();
        let pool = self.recycled.borrow();
        budget.available_after_releasing(pool.iter().map(|a| &a.permit).chain(retired.iter().map(
            |r| match r {
                Retired::Texture(a) | Retired::Evicted(a) => &a.permit,
                Retired::Buffer(_, permit, _) => permit,
            },
        )))
    }
    /// Count only storage not already retained in the two reuse queues. Each
    /// cached allocation can satisfy one tile, and must belong to this pool.
    pub fn plane_allocation_bytes(&self, size: Size, edge: u32, budget: &Budget) -> usize {
        let mut required = size.rgba_bytes().unwrap();
        if !self.streamed_uploads() {
            return required;
        }
        let mut shapes = tile_shapes(size, edge);
        let retired = self.retired.borrow();
        let pool = self.recycled.borrow();
        for allocation in pool.iter().chain(retired.iter().filter_map(|r| match r {
            Retired::Texture(a) | Retired::Evicted(a) => Some(a),
            _ => None,
        })) {
            if let Some((size, count)) = shapes
                .iter_mut()
                .find(|(s, n)| *n != 0 && allocation.matches(*s, glow::RGBA, budget))
            {
                required -= size.rgba_bytes().unwrap();
                *count -= 1;
            }
        }
        required
    }
    pub fn streamed_uploads(&self) -> bool {
        self.work.borrow().is_some()
    }
    pub fn new(
        gl: glow::Context,
        work_edge: Option<u32>,
        scratch: Budget,
        staging: Budget,
        target_entries: usize,
        target_bytes: usize,
    ) -> Result<Rc<Self>> {
        unsafe {
            let max_texture = gl
                .get_parameter_i32(glow::MAX_TEXTURE_SIZE)
                .min(gl.get_parameter_i32(glow::MAX_RENDERBUFFER_SIZE));
            if max_texture < 512
                || gl.get_parameter_i32(glow::MAX_TEXTURE_IMAGE_UNITS)
                    < if work_edge.is_some() { 8 } else { 6 }
            {
                return Err(Error::Message(
                    "GLES device does not support required texture limits",
                ));
            }
            let quad = gl.create_buffer().map_err(Error::Backend)?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(quad));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&[0_f32, 0., 1., 0., 0., 1., 1., 1.]),
                glow::STATIC_DRAW,
            );
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.disable(glow::DEPTH_TEST);
            gl.disable(glow::STENCIL_TEST);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::DITHER);
            gl.disable(glow::BLEND);
            let shader_binaries = crate::shader_binary::Loader::new(&gl);
            let device = Rc::new(Self {
                gl,
                quad,
                draw_state: draw_state::DrawState::default(),
                quad_vertex: Cell::new(None),
                shader_binaries,
                max_texture: max_texture as u32,
                retired: RefCell::new(Vec::new()),
                recycled: RefCell::new(Vec::new()),
                work: RefCell::new(None),
                targets: RefCell::new(targets::Targets::new(
                    target_entries,
                    target_bytes,
                    scratch.clone(),
                )),
                staging,
            });
            device.check()?;
            if let Some(edge) = work_edge {
                let edge = edge.min(device.max_texture);
                *device.work.borrow_mut() = Some(work::Work::new(
                    &device,
                    Size {
                        width: edge,
                        height: edge,
                    },
                    &scratch,
                )?);
            }
            Ok(device)
        }
    }
    #[track_caller]
    pub fn check(&self) -> Result<()> {
        let error = unsafe { self.gl.get_error() };
        if error != glow::NO_ERROR {
            let caller = std::panic::Location::caller();
            let message = format!(
                "GLES error 0x{error:04x} at {}:{}",
                caller.file(),
                caller.line()
            );
            return Err(Error::Backend(message));
        }
        Ok(())
    }
    pub fn buffer(self: &Rc<Self>, kind: u32, bytes: &[u8], budget: &Budget) -> Result<Buffer> {
        if bytes.len() > i32::MAX as usize {
            return Err(Error::Message("GLES buffer exceeds address limits"));
        }
        let permit = match budget.reserve(bytes.len().max(4)) {
            Ok(permit) => permit,
            Err(_) => {
                self.collect()?;
                budget.reserve(bytes.len().max(4))?
            }
        };
        unsafe {
            let name = self.gl.create_buffer().map_err(Error::Backend)?;
            let buffer = Buffer {
                device: self.clone(),
                name,
                permit: Some(permit),
                bytes: bytes.len().max(4),
            };
            self.check()?;
            for attempt in 0..2 {
                if kind == glow::ARRAY_BUFFER {
                    self.draw_state.invalidate_vertices();
                }
                self.gl.bind_buffer(kind, Some(name));
                self.gl.buffer_data_u8_slice(
                    kind,
                    if bytes.is_empty() { &[0; 4] } else { bytes },
                    glow::STREAM_DRAW,
                );
                let error = self.gl.get_error();
                if error == glow::NO_ERROR {
                    break;
                }
                if error != glow::OUT_OF_MEMORY || attempt != 0 {
                    return Err(Error::Backend(format!(
                        "GLES buffer allocation target=0x{kind:04x} bytes={}: 0x{error:04x}",
                        buffer.bytes,
                    )));
                }
                // Small retired buffers/textures can exhaust native resource
                // slots while byte budgets still have plenty of room. Finish
                // and release them once, before this buffer is used by a draw.
                self.collect_under_pressure()?;
            }
            Ok(buffer)
        }
    }
    pub fn texture(self: &Rc<Self>, size: Size, budget: &Budget) -> Result<Rc<Texture>> {
        self.allocate(size, budget, glow::RGBA, true, None)
    }
    /// Read-only masks use core ES2 LUMINANCE, with no framebuffer or renderable
    /// R8 extension. Set the filter for each allocation because a recycled
    /// LUMINANCE texture may have belonged to a different mask user.
    pub fn mask_texture(
        self: &Rc<Self>,
        size: Size,
        budget: &Budget,
        linear: bool,
    ) -> Result<Rc<Texture>> {
        let texture = self.allocate(size, budget, glow::LUMINANCE, false, None)?;
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture.name()));
            let filter = if linear { glow::LINEAR } else { glow::NEAREST };
            for name in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                self.gl
                    .tex_parameter_i32(glow::TEXTURE_2D, name, filter as i32);
            }
        }
        self.check()?;
        Ok(texture)
    }
    /// The caller fully uploads or copies before publishing this texture.
    pub fn sample_texture(self: &Rc<Self>, size: Size, budget: &Budget) -> Result<Rc<Texture>> {
        self.allocate(size, budget, glow::RGBA, false, None)
    }
    /// NV12 already stores adjacent U/V bytes. ES2 luminance/alpha preserves
    /// those two channels without a CPU repack or two chroma texture reads.
    pub fn nv12_chroma_texture(
        self: &Rc<Self>,
        size: Size,
        budget: &Budget,
    ) -> Result<Rc<Texture>> {
        self.allocate(size, budget, glow::LUMINANCE_ALPHA, false, None)
    }
    pub fn uploaded_texture(
        self: &Rc<Self>,
        size: Size,
        data: &[u8],
        budget: &Budget,
    ) -> Result<Rc<Texture>> {
        if size.rgba_bytes() != Some(data.len()) {
            return Err(Error::Message("RGBA texture size mismatch"));
        }
        self.allocate(
            size,
            budget,
            glow::RGBA,
            false,
            Some(TextureData::Pixels(data)),
        )
    }
    pub fn supports_compressed_format(&self, format: krkr_protocol::texture::Format) -> bool {
        use krkr_protocol::texture::Format;
        let extensions = self.gl.supported_extensions();
        if format.is_vita() && extensions.contains("GL_KRKR_texture_compression_bc") {
            return true;
        }
        match format.linear() {
            Format::Etc1 => extensions.contains("GL_OES_compressed_ETC1_RGB8_texture"),
            Format::Pvrtc1Rgba4 => extensions.contains("GL_IMG_texture_compression_pvrtc"),
            Format::Bc1Rgb => {
                extensions.contains("GL_EXT_texture_compression_s3tc")
                    || extensions.contains("GL_EXT_texture_compression_dxt1")
            }
            Format::Bc3Rgba => {
                extensions.contains("GL_EXT_texture_compression_s3tc")
                    || extensions.contains("GL_ANGLE_texture_compression_dxt5")
            }
            _ => false,
        }
    }
    pub fn compressed_texture(
        self: &Rc<Self>,
        size: Size,
        format: krkr_protocol::texture::Format,
        data: &[u8],
        budget: &Budget,
    ) -> Result<Rc<Texture>> {
        if format.byte_len(size) != Some(data.len()) {
            return Err(Error::Message("compressed texture size mismatch"));
        }
        if format.is_vita()
            && !self
                .gl
                .supported_extensions()
                .contains("GL_KRKR_texture_compression_bc")
        {
            // Desktop GPUs also sample BC directly. Only the block order differs;
            // keep the temporary allocation compressed and inside staging limits.
            let mut linear = krkr_protocol::pixels::Bytes::zeroed(data.len(), &self.staging)?;
            krkr_protocol::texture::reorder_bc(size, format, data, linear.as_mut_slice(), false)
                .map_err(Error::Message)?;
            return self.allocate(
                size,
                budget,
                format.linear().gl_internal(),
                false,
                Some(TextureData::Compressed(linear.as_slice())),
            );
        }
        self.allocate(
            size,
            budget,
            format.gl_internal(),
            false,
            Some(TextureData::Compressed(data)),
        )
    }
    /// Bounded blur and seam-gather targets. Callers initialize their contents
    /// before sampling, with no CPU upload.
    pub fn render_texture(self: &Rc<Self>, size: Size, budget: &Budget) -> Result<Rc<Texture>> {
        // Keep native attachments in their own reuse path. Ordinary images
        // must still use the shared work surface rather than inherit an FBO.
        let recycled = {
            let mut retired = self.retired.borrow_mut();
            retired
                .iter()
                .position(|resource| {
                    matches!(resource, Retired::Texture(allocation) | Retired::Evicted(allocation)
                        if allocation.framebuffer.is_some()
                            && allocation.size == size
                            && allocation.format == glow::RGBA
                            && allocation.permit.belongs_to(budget))
                })
                .map(|index| retired.swap_remove(index))
        };
        if let Some(Retired::Texture(allocation) | Retired::Evicted(allocation)) = recycled {
            // Sampling of the old contents is already ordered before the next
            // draw. Reuse its storage and attachment without a global finish.
            return Ok(self.reuse_allocation(allocation));
        }
        let texture = self.allocate(size, budget, glow::RGBA, false, None)?;
        self.native_framebuffer(&texture)?;
        Ok(texture)
    }
    fn reuse_allocation(self: &Rc<Self>, old: Allocation) -> Rc<Texture> {
        Rc::new_cyclic(|weak| Texture {
            device: self.clone(),
            texture: old.texture,
            framebuffer: Cell::new(old.framebuffer),
            weak: weak.clone(),
            format: old.format,
            size: old.size,
            generation: Cell::new(0),
            unused_storage: Cell::new(false),
            border_scan_generation: Cell::new(None),
            solid: Cell::new(None),
            alpha: Cell::new(None),
            writes: RefCell::new(Rc::new([(0, Rect::default()); 16])),
            copies: RefCell::new([const { None }; 4]),
            permit: Some(old.permit),
        })
    }
    fn allocate(
        self: &Rc<Self>,
        size: Size,
        budget: &Budget,
        format: u32,
        clear: bool,
        data: Option<TextureData<'_>>,
    ) -> Result<Rc<Texture>> {
        if size.width == 0
            || size.height == 0
            || size.width > self.max_texture
            || size.height > self.max_texture
        {
            return Err(Error::Message("invalid GLES texture dimensions"));
        }
        // ES2 guarantees RGBA8 uploads and RGBA readback. Province planes use
        // the red byte in RGBA storage; no renderable R8 extension is assumed.
        let bytes =
            storage_bytes(size, format).ok_or(Error::Message("texture byte size overflow"))?;
        if self.streamed_uploads() && !matches!(data, Some(TextureData::Compressed(_))) {
            let recycled = {
                let mut pool = self.recycled.borrow_mut();
                pool.iter()
                    .position(|a| a.matches(size, format, budget))
                    .map(|index| pool.swap_remove(index))
            }
            .or_else(|| {
                let mut retired = self.retired.borrow_mut();
                let index = retired.iter().position(|r| {
                    matches!(r,
                    Retired::Texture(a) | Retired::Evicted(a) if a.matches(size, format, budget))
                })?;
                let (Retired::Texture(allocation) | Retired::Evicted(allocation)) =
                    retired.swap_remove(index)
                else {
                    unreachable!()
                };
                Some(allocation)
            });
            if let Some(old) = recycled {
                let allocation = self.reuse_allocation(old);
                // All previous sampling precedes this clear in the GL stream.
                // Keep the resident allocation and its permit; no TexImage,
                // host-side zero upload, deletion, or global finish is needed.
                if let Some(TextureData::Pixels(pixels)) = data {
                    self.upload(&allocation, pixels)?;
                } else if clear {
                    if let Some(framebuffer) = self.targets.borrow_mut().get(allocation.name()) {
                        unsafe {
                            self.gl
                                .bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                            self.gl.disable(glow::SCISSOR_TEST);
                            self.gl.color_mask(true, true, true, true);
                            self.gl.clear_color(0., 0., 0., 0.);
                            self.gl.clear(glow::COLOR_BUFFER_BIT);
                        }
                        self.check()?;
                    } else {
                        self.work
                            .borrow_mut()
                            .as_mut()
                            .unwrap()
                            .clear(self, &allocation)?;
                    }
                }
                return Ok(allocation);
            }
        }
        let permit = match budget.reserve(bytes) {
            Ok(permit) => permit,
            Err(_) => {
                self.collect()?;
                budget.reserve(bytes)?
            }
        };
        let zeros = if clear && self.work.borrow().is_some() {
            let stride = size.width as usize * 4;
            let capacity = if bytes <= self.staging.available() {
                bytes
            } else {
                (self.staging.available().min(64 * 1024) / stride).max(1) * stride
            };
            Some(krkr_protocol::pixels::Bytes::zeroed(
                capacity,
                &self.staging,
            )?)
        } else {
            None
        };
        unsafe {
            let gl = &self.gl;
            let texture = gl.create_texture().map_err(Error::Backend)?;
            let allocation = Rc::new_cyclic(|weak| Texture {
                device: self.clone(),
                texture,
                framebuffer: Cell::new(None),
                weak: weak.clone(),
                format,
                size,
                generation: Cell::new(0),
                unused_storage: Cell::new(!clear && data.is_none()),
                border_scan_generation: Cell::new(None),
                solid: Cell::new(None),
                alpha: Cell::new(None),
                writes: RefCell::new(Rc::new([(0, Rect::default()); 16])),
                copies: RefCell::new([const { None }; 4]),
                permit: Some(permit),
            });
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            for name in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, glow::NEAREST as i32);
            }
            for name in [glow::TEXTURE_WRAP_S, glow::TEXTURE_WRAP_T] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, name, glow::CLAMP_TO_EDGE as i32);
            }
            self.check()?;
            for attempt in 0..2 {
                if let Some(TextureData::Compressed(data)) = data {
                    gl.compressed_tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        format as i32,
                        size.width as i32,
                        size.height as i32,
                        0,
                        data.len() as i32,
                        data,
                    );
                } else {
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        format as i32,
                        size.width as i32,
                        size.height as i32,
                        0,
                        format,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(match data {
                            Some(TextureData::Pixels(pixels)) => Some(pixels),
                            _ => zeros
                                .as_ref()
                                .filter(|z| z.as_slice().len() == bytes)
                                .map(|z| z.as_slice()),
                        }),
                    );
                }
                let error = gl.get_error();
                if error == glow::NO_ERROR {
                    break;
                }
                if error != glow::OUT_OF_MEMORY || attempt != 0 {
                    return Err(Error::Backend(format!(
                        "GLES texture allocation {}x{} format=0x{format:04x} bytes={bytes}: 0x{error:04x}",
                        size.width, size.height
                    )));
                }
                // Software byte permits cannot account for PVR's transient
                // backing copies or heap fragmentation. Release retired and
                // recycled storage before retrying this unpublished allocation
                // once. Never replay a draw/blend that may have partly run.
                self.collect_under_pressure()?;
                gl.active_texture(glow::TEXTURE0);
                gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            }
            if let Some(zeros) = zeros.as_ref().filter(|z| z.as_slice().len() < bytes) {
                let stride = size.width as usize * 4;
                let rows = zeros.as_slice().len() / stride;
                for top in (0..size.height as usize).step_by(rows) {
                    let height = rows.min(size.height as usize - top);
                    self.upload_region(
                        &allocation,
                        Rect {
                            left: 0,
                            top: top as i32,
                            width: size.width,
                            height: height as u32,
                        },
                        &zeros.as_slice()[..height * stride],
                    )?;
                }
            } else if clear && zeros.is_none() {
                let framebuffer = allocation.framebuffer()?;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                gl.disable(glow::SCISSOR_TEST);
                gl.color_mask(true, true, true, true);
                gl.clear_color(0., 0., 0., 0.);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
            self.check()?;
            Ok(allocation)
        }
    }
    fn framebuffer(
        &self,
        texture: &Texture,
        dirty: Option<Rect>,
        overwrite: bool,
        streaming: bool,
    ) -> Result<glow::NativeFramebuffer> {
        if let Some(area) = dirty {
            texture.changed(area);
        }
        if texture.format != glow::RGBA {
            return Err(Error::Message(
                "sample-only texture cannot be a framebuffer",
            ));
        }
        if let Some(name) = texture.framebuffer.get() {
            return Ok(name);
        }
        // A backdrop or prepared batch pins this texture to the independent
        // work surface until the batch changes targets. Never promote it while
        // its own backing is still being sampled by those draws.
        let direct = self
            .work
            .borrow()
            .as_ref()
            .is_some_and(|work| !work.contains(texture));
        if direct {
            let selected = self.targets.borrow_mut().select(self, texture, streaming);
            match selected {
                Ok(Some(name)) => return Ok(name),
                Err(Error::Backend(ref message))
                    if message.starts_with("GLES error 0x0505 at ") =>
                {
                    self.collect_under_pressure()?;
                    if let Some(name) =
                        self.targets.borrow_mut().select(self, texture, streaming)?
                    {
                        return Ok(name);
                    }
                }
                Err(error) => return Err(error),
                Ok(None) => {}
            }
        }
        let selected = self
            .work
            .borrow_mut()
            .as_mut()
            .map(|work| work.target(self, texture, dirty, overwrite));
        if let Some(selected) = selected {
            // Selecting the work target can promote its backing texture on
            // PVR. That driver allocation may fail although its software
            // permit fits. No caller draw has run yet: reclaim idle storage
            // and completed ghosts, then retry selection once. Never replay
            // the caller's blend or swallow errors from an earlier store.
            if matches!(&selected, Err(Error::Backend(message))
                if message.starts_with("GLES error 0x0505 at "))
            {
                self.collect_under_pressure()?;
                return self
                    .work
                    .borrow_mut()
                    .as_mut()
                    .unwrap()
                    .target(self, texture, dirty, overwrite);
            }
            return selected;
        }
        self.native_framebuffer(texture)
    }
    fn native_framebuffer(&self, texture: &Texture) -> Result<glow::NativeFramebuffer> {
        if let Some(name) = texture.framebuffer.get() {
            return Ok(name);
        }
        let name = self.create_framebuffer(texture)?;
        texture.framebuffer.set(Some(name));
        Ok(name)
    }
    fn create_framebuffer(&self, texture: &Texture) -> Result<glow::NativeFramebuffer> {
        self.check()?;
        unsafe {
            let gl = &self.gl;
            let name = gl.create_framebuffer().map_err(Error::Backend)?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(name));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture.name()),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            if let Err(error) = self.check().and_then(|()| {
                if status == glow::FRAMEBUFFER_COMPLETE {
                    Ok(())
                } else {
                    Err(Error::Backend(format!(
                        "GLES framebuffer {status:#x}, texture {:?}",
                        texture.size
                    )))
                }
            }) {
                gl.delete_framebuffer(name);
                return Err(error);
            }
            Ok(name)
        }
    }
    /// Freeze the previous pixels for a destination-reading pass. The fixed
    /// work surface and the backing texture are distinct GPU resources.
    pub fn backdrop(&self, texture: &Texture, area: Rect) -> Result<Option<glow::NativeTexture>> {
        if texture.framebuffer.get().is_some() {
            return Ok(None);
        }
        let mut cell = self.work.borrow_mut();
        let Some(work) = cell.as_mut() else {
            return Ok(None);
        };
        work.prepare(self, texture, area)?;
        work.backdrop(self, area)?;
        Ok(Some(texture.name()))
    }
    /// Prepare a destination-reading batch on the shared surface. Dedicated
    /// targets (such as box blur) use separate backdrops; cached image FBOs can
    /// still use the work surface for a batch that samples its own old pixels.
    pub fn supports_work_draw(&self, texture: &Texture) -> bool {
        texture.framebuffer.get().is_none() && self.work.borrow().is_some()
    }
    pub fn prepare_work_draw(
        &self,
        texture: &Texture,
        area: Rect,
    ) -> Result<Option<glow::NativeFramebuffer>> {
        if texture.framebuffer.get().is_some() {
            return Ok(None);
        }
        let mut cell = self.work.borrow_mut();
        let Some(work) = cell.as_mut() else {
            return Ok(None);
        };
        let framebuffer = work.prepare(self, texture, area)?;
        // Record the batch once: a long text run must not exhaust the bounded
        // damage history merely by touching many glyphs in the same region.
        texture.changed(area);
        Ok(Some(framebuffer))
    }
    /// Only called inside a prepared batch, after resolving this draw's
    /// backdrop. Keep its actual writes separate from the preloaded bounds.
    pub fn work_draw_region(&self, texture: &Texture, area: Rect) -> Result<()> {
        self.work
            .borrow_mut()
            .as_mut()
            .ok_or(Error::Message("draw batch has no work surface"))?
            .target(self, texture, Some(area), false)?;
        Ok(())
    }
    pub fn before_sample(&self, texture: &Texture) -> Result<()> {
        texture.unused_storage.set(false);
        if let Some(work) = self.work.borrow_mut().as_mut()
            && work.contains(texture)
        {
            work.sample(self)?;
        }
        Ok(())
    }
    fn before_upload(&self, texture: &Texture, area: Rect) -> Result<()> {
        texture.changed(area);
        if let Some(work) = self.work.borrow_mut().as_mut()
            && work.contains(texture)
        {
            if area != texture.size.rect() {
                work.store(self, false)?;
            }
            work.invalidate();
        }
        Ok(())
    }
    fn store(&self) -> Result<()> {
        if let Some(work) = self.work.borrow_mut().as_mut() {
            work.store(self, false)?;
        }
        Ok(())
    }
    pub fn upload(&self, texture: &Texture, bytes: &[u8]) -> Result<()> {
        self.upload_region(texture, texture.size.rect(), bytes)
    }
    /// Release an owned upload buffer as soon as GLES consumes it.
    pub fn upload_owned_region(
        &self,
        texture: &Texture,
        rectangle: Rect,
        bytes: krkr_protocol::pixels::Bytes,
    ) -> Result<()> {
        self.upload_region(texture, rectangle, bytes.as_slice())
    }
    pub fn upload_region(&self, texture: &Texture, rectangle: Rect, pixels: &[u8]) -> Result<()> {
        let length = pixels.len();
        let size = Size {
            width: rectangle.width,
            height: rectangle.height,
        };
        if texture.size.rect().intersection(rectangle) != Some(rectangle)
            || texture.bytes(size) != Some(length)
        {
            return Err(Error::Message(
                "GLES upload byte count differs from texture",
            ));
        }
        let streamed = self.streamed_uploads();
        let full = rectangle == texture.size.rect();
        let unused = texture.unused_storage.get();
        self.before_upload(texture, rectangle)?;
        unsafe {
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture.name()));
            self.gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            if full && streamed && !unused {
                // Full replacement needs no old pixels. Re-specifying storage
                // lets PVR retain an in-flight version instead of preserving
                // and synchronizing a live texture for a subimage update.
                self.gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    texture.format as i32,
                    texture.size.width as i32,
                    texture.size.height as i32,
                    0,
                    texture.format,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(pixels)),
                );
            } else {
                // PVR can upload into idle STRIDE storage directly. In
                // particular, do not redefine a fresh NULL allocation: that
                // would send the decoder's pixels through a host staging copy.
                self.gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    rectangle.left,
                    rectangle.top,
                    rectangle.width as i32,
                    rectangle.height as i32,
                    texture.format,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(pixels)),
                );
            }
        }
        self.check()
    }
    pub(crate) fn make_textures_resident(
        &self,
        sources: &[(glow::NativeTexture, Size)],
    ) -> Result<()> {
        self.work
            .borrow_mut()
            .as_mut()
            .expect("streamed uploads require work surface")
            .make_textures_resident(self, sources)
    }
    pub fn copy(&self, source: &Texture, target: &Texture) -> Result<()> {
        if source.size != target.size {
            return Err(Error::Message("GLES texture copy dimensions differ"));
        }
        if std::ptr::eq(source, target) {
            return Ok(());
        }
        self.copy_region(source, source.size.rect(), target)?;
        target.copied_from(source);
        Ok(())
    }
    pub fn copy_region(&self, source: &Texture, rectangle: Rect, target: &Texture) -> Result<()> {
        if source.size.rect().intersection(rectangle) != Some(rectangle)
            || rectangle.width != target.size.width
            || rectangle.height != target.size.height
        {
            return Err(Error::Message(
                "GLES texture copy region differs from target",
            ));
        }
        self.copy_region_at(source, rectangle, target, 0, 0)
    }
    pub fn copy_region_at(
        &self,
        source: &Texture,
        rectangle: Rect,
        target: &Texture,
        x: i32,
        y: i32,
    ) -> Result<()> {
        let destination = Rect {
            left: x,
            top: y,
            ..rectangle
        };
        if source.size.rect().intersection(rectangle) != Some(rectangle)
            || target.size.rect().intersection(destination) != Some(destination)
        {
            return Err(Error::Message("GLES texture copy lies outside image"));
        }
        if target.framebuffer.get().is_none()
            && let Some(work) = self.work.borrow_mut().as_mut()
        {
            target.changed(destination);
            return work.copy(self, source, rectangle, target, destination);
        }
        self.before_upload(target, destination)?;
        unsafe {
            self.gl
                .bind_framebuffer(glow::FRAMEBUFFER, Some(source.read_framebuffer()?));
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(target.name()));
            self.gl.copy_tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                x,
                y,
                rectangle.left,
                rectangle.top,
                rectangle.width as i32,
                rectangle.height as i32,
            );
        }
        self.check()
    }
    pub fn retire_program(&self, program: glow::NativeProgram) {
        // PVR's shader KRM ghosts in-flight USE variants. No host allocation
        // backs a linked program, and GL deletion needs no global finish.
        self.draw_state.invalidate_program();
        unsafe {
            if self.gl.get_parameter_i32(glow::CURRENT_PROGRAM) as u32 == program.0.get() {
                self.gl.use_program(None);
            }
            self.gl.delete_program(program);
        }
    }
    pub fn flush(&self) -> Result<()> {
        self.store()?;
        unsafe {
            self.gl.flush();
        }
        self.check()
    }
    pub fn resolve(&self) -> Result<()> {
        self.store()?;
        self.check()
    }
    pub fn collect(&self) -> Result<()> {
        self.collect_inner(false)
    }
    pub fn collect_under_pressure(&self) -> Result<()> {
        self.collect_inner(true)
    }
    fn collect_inner(&self, pressure: bool) -> Result<()> {
        let _profile = krkr_protocol::profile::span("gpu.collect");
        self.store()?;
        // The driver may retain a texture-owned surface after FBO deletion.
        // Drain dead texture allocations below; live ones keep their slots.
        self.retired
            .borrow_mut()
            .extend(self.recycled.borrow_mut().drain(..).map(Retired::Texture));
        // A driver can retain in-flight backing copies even when no Rust
        // textures have retired. An OOM retry must wait for those as well.
        if !pressure && self.retired.borrow().is_empty() {
            return self.check();
        }
        {
            let _profile = krkr_protocol::profile::span("gpu.finish");
            unsafe {
                self.gl.finish();
            }
        }
        self.drain();
        self.check()
    }
    /// Frame maintenance keeps a bounded cache of resident texture storage.
    /// Explicit collection and admission pressure still return every permit.
    pub fn maintain(&self) -> Result<()> {
        if !self.streamed_uploads() {
            return self.collect();
        }
        {
            let mut pool = self.recycled.borrow_mut();
            let mut retired = self.retired.borrow_mut();
            let pending = std::mem::take(&mut *retired);
            for resource in pending {
                match resource {
                    Retired::Texture(allocation)
                        if allocation.format == glow::RGBA && allocation.framebuffer.is_none() =>
                    {
                        pool.push(allocation)
                    }
                    resource => retired.push(resource),
                }
            }
            let mut bytes: usize = pool.iter().map(Allocation::bytes).sum();
            // Recently retired sizes displace cold ones. Keeping the first 32
            // allocations forever prevented a new page's sizes from entering.
            while pool.len() > 32 || bytes > 16 * 1024 * 1024 {
                let old = pool.remove(0);
                bytes -= old.bytes();
                retired.push(Retired::Evicted(old));
            }
        }
        let reclaim = {
            let retired = self.retired.borrow();
            retired.len() >= 64
                || retired
                    .iter()
                    .map(|r| match r {
                        Retired::Texture(a) | Retired::Evicted(a) => a.bytes(),
                        Retired::Buffer(_, _, bytes) => *bytes,
                    })
                    .sum::<usize>()
                    >= 4 * 1024 * 1024
        };
        // Tiny glyph atlases and transient vertex buffers must not force a 3D
        // completion on each maintenance pass. Keep both storage and permits
        // until this bounded batch completes; pressure/teardown still drains
        // immediately.
        if reclaim {
            self.store()?;
            unsafe {
                self.gl.finish();
            }
            self.drain();
        }
        self.check()
    }
    fn drain(&self) {
        for retired in self.retired.borrow_mut().drain(..) {
            unsafe {
                match retired {
                    Retired::Texture(allocation) | Retired::Evicted(allocation) => {
                        self.targets
                            .borrow_mut()
                            .remove(&self.gl, allocation.texture);
                        if let Some(framebuffer) = allocation.framebuffer {
                            self.gl.delete_framebuffer(framebuffer);
                        }
                        self.gl.delete_texture(allocation.texture);
                        drop(allocation.permit);
                    }
                    Retired::Buffer(buffer, permit, _) => {
                        self.gl.delete_buffer(buffer);
                        drop(permit);
                    }
                }
            }
        }
    }
}
impl Drop for Device {
    fn drop(&mut self) {
        // The currently bound engine program may have been delete-marked by
        // Gpu's field destructors; a barrier must not try to restore that name.
        unsafe {
            self.gl.use_program(None);
        }
        self.retired
            .get_mut()
            .extend(self.recycled.get_mut().drain(..).map(Retired::Texture));
        unsafe {
            self.gl.finish();
            self.targets.get_mut().clear(&self.gl);
            if let Some(work) = self.work.get_mut().take() {
                work.destroy(&self.gl);
            }
            self.gl.delete_buffer(self.quad);
            if let Some(shader) = self.quad_vertex.take() {
                self.gl.delete_shader(shader);
            }
        }
        self.drain();
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    #[test]
    fn algebraic_grid_counts_match_edge_tiles() {
        for edge in [1, 16, 64, 256] {
            for width in [0, 1, 17, 64, 257, 960] {
                for height in [0, 1, 31, 128, 544] {
                    let mut expected = std::collections::BTreeMap::new();
                    for top in (0..height).step_by(edge as usize) {
                        for left in (0..width).step_by(edge as usize) {
                            *expected
                                .entry((edge.min(width - left), edge.min(height - top)))
                                .or_insert(0) += 1;
                        }
                    }
                    let actual = tile_shapes(Size { width, height }, edge)
                        .into_iter()
                        .map(|(size, count)| ((size.width, size.height), count))
                        .collect::<std::collections::BTreeMap<_, _>>();
                    assert_eq!(actual, expected);
                }
            }
        }
    }
}

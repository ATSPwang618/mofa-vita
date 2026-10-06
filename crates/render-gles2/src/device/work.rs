//! One persistent, linear RGBA8 renderbuffer. Image textures retain stable
//! names and receive only dirty pixels when another image needs the surface.
use super::{Device, Texture};
use crate::{Error, Result};
use glow::HasContext;
use krkr_protocol::{
    budget::{Budget, Permit},
    graphics::{Rect, Size},
};
use std::{cell::Cell, num::NonZeroU32, rc::Weak};
#[cfg(test)]
#[path = "../../tests/device/dirty.rs"]
mod dirty_tests;

struct Surface {
    color: glow::NativeRenderbuffer,
    framebuffer: glow::NativeFramebuffer,
    size: Size,
    _permit: Permit,
}
struct Pending {
    owner: Weak<Texture>,
    name: glow::NativeTexture,
    // Only this rectangle has current pixels in the shared renderbuffer.
    // Loading a tiny glyph must not copy a whole 960x540 backing texture.
    valid: Rect,
    dirty: Dirty,
}
#[derive(Default)]
struct Dirty {
    bounds: Option<Rect>,
    // Keep disjoint writes separate for dependency checks. The bounding box
    // remains valid for a single eventual transfer (gaps are loaded by select).
    regions: [Rect; 16],
    len: usize,
}
impl Dirty {
    fn add(&mut self, mut area: Rect) {
        let bounds = crate::scene_damage::union(self.bounds, area);
        self.bounds = Some(bounds);
        if self.regions[..self.len]
            .iter()
            .any(|old| old.intersection(area) == Some(area))
        {
            return;
        }
        // Adjacent spans on one row/column and contained writes can be folded
        // without introducing holes. This avoids the quadratic closest-pair
        // search when a run contains many small contiguous updates.
        let mut index = 0;
        while index < self.len {
            let old = self.regions[index];
            let merged = crate::scene_damage::union(Some(old), area);
            let pixels = |r: Rect| u64::from(r.width) * u64::from(r.height);
            if pixels(merged)
                == pixels(old) + pixels(area) - old.intersection(area).map_or(0, pixels)
            {
                area = merged;
                self.len -= 1;
                self.regions[index] = self.regions[self.len];
                index = 0;
            } else {
                index += 1;
            }
        }
        if self.len == self.regions.len() {
            // Keep the same bounded metadata, but merge the closest pair.
            // Collapsing everything into `bounds` revives already-resolved
            // holes (e.g. a text line inside a freshly filled background),
            // introducing a render/transfer barrier for every later glyph.
            let pixels = |r: Rect| u64::from(r.width) * u64::from(r.height);
            let mut best = (u64::MAX, 0, self.len, area);
            for i in 0..self.len {
                let a = self.regions[i];
                for j in i + 1..=self.len {
                    let b = if j == self.len { area } else { self.regions[j] };
                    let merged = crate::scene_damage::union(Some(a), b);
                    let overlap = a.intersection(b).map_or(0, pixels);
                    let extra = pixels(merged) + overlap - pixels(a) - pixels(b);
                    if extra < best.0 {
                        best = (extra, i, j, merged);
                    }
                }
            }
            self.regions[best.1] = best.3;
            if best.2 < self.len {
                self.regions[best.2] = area;
            }
        } else {
            self.regions[self.len] = area;
            self.len += 1;
        }
    }
    fn intersects(&self, area: Rect) -> bool {
        self.regions[..self.len]
            .iter()
            .any(|old| old.intersection(area).is_some())
    }
    fn remove(&mut self, area: Rect) {
        let mut remaining = Self::default();
        for old in &self.regions[..self.len] {
            for part in subtract(*old, area).into_iter().flatten() {
                // Too much fragmentation: retaining already-stored pixels is
                // safe and bounded. Never forget an outstanding write.
                if remaining.len == remaining.regions.len() {
                    return;
                }
                remaining.add(part);
            }
        }
        *self = remaining;
    }
    fn transfers(&self) -> &[Rect] {
        let Some(bounds) = self.bounds.as_ref() else {
            return &[];
        };
        let regions = &self.regions[..self.len];
        let pixels = |area: &Rect| u64::from(area.width) * u64::from(area.height);
        // Prefer one transfer for dense text and ordinary adjacent writes.
        // Widely separated updates must not copy a whole screen of untouched
        // pixels. Charge extra calls conservatively before splitting a store.
        let separate: u64 = regions.iter().map(|r| pixels(r) + 256).sum();
        if self.len > 1 && separate * 2 < pixels(bounds) {
            regions
        } else {
            std::slice::from_ref(bounds)
        }
    }
}
pub(super) struct Work {
    surface: Surface,
    program: glow::NativeProgram,
    source_uniform: Option<glow::NativeUniformLocation>,
    region_uniform: Option<glow::NativeUniformLocation>,
    region: Cell<Option<[f32; 4]>>,
    destination_uniform: Option<glow::NativeUniformLocation>,
    destination: Cell<Option<[f32; 4]>>,
    budget: Budget,
    current: Option<Pending>,
}
// Unit 7 and attribute 2 are reserved for this internal copy. Renderer passes
// use units 0..5 and attributes 0..1; their bindings and pointers stay intact.
struct Bindings {
    active: u32,
    texture: Option<glow::NativeTexture>,
    framebuffer: Option<glow::NativeFramebuffer>,
}
impl Bindings {
    unsafe fn save(gl: &glow::Context) -> Self {
        unsafe {
            let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
            if active != glow::TEXTURE7 {
                gl.active_texture(glow::TEXTURE7);
            }
            Self {
                active,
                texture: NonZeroU32::new(gl.get_parameter_i32(glow::TEXTURE_BINDING_2D) as u32)
                    .map(glow::NativeTexture),
                framebuffer:
                    NonZeroU32::new(gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32)
                        .map(glow::NativeFramebuffer),
            }
        }
    }
    unsafe fn restore(self, gl: &glow::Context, current: glow::NativeFramebuffer) {
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, self.texture);
            if self.active != glow::TEXTURE7 {
                gl.active_texture(self.active);
            }
            // PVR submits the active surface even for a same-name bind.
            if self.framebuffer != Some(current) {
                gl.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            }
        }
    }
}
impl Surface {
    fn new(device: &Device, size: Size, budget: &Budget) -> Result<Self> {
        // This is an ES2 extension, not the core RGBA4 format. Never silently
        // reduce alpha/color precision for a work surface.
        if !device
            .gl
            .supported_extensions()
            .contains("GL_OES_rgb8_rgba8")
            && device.gl.version().major < 3
        {
            return Err(Error::Message("work surface requires GL_OES_rgb8_rgba8"));
        }
        let permit = budget.reserve(
            size.rgba_bytes()
                .ok_or(Error::Message("work surface size overflow"))?,
        )?;
        let gl = &device.gl;
        unsafe {
            let color = gl.create_renderbuffer().map_err(Error::Backend)?;
            let framebuffer = match gl.create_framebuffer() {
                Ok(name) => name,
                Err(error) => {
                    gl.delete_renderbuffer(color);
                    return Err(Error::Backend(error));
                }
            };
            let bindings = Bindings::save(gl);
            let previous = NonZeroU32::new(gl.get_parameter_i32(glow::RENDERBUFFER_BINDING) as u32)
                .map(glow::NativeRenderbuffer);
            gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
            // PVR fbo.c stores renderbuffers linearly. Texture attachments use
            // a texture layout; pixelop.c's software CopyTex fallback then
            // allocates and untwiddles the ENTIRE work surface, even for 1px.
            // The work surface is never sampled, so a renderbuffer avoids that
            // allocation and conversion without adding native render surfaces.
            gl.renderbuffer_storage(
                glow::RENDERBUFFER,
                glow::RGBA8,
                size.width as i32,
                size.height as i32,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.framebuffer_renderbuffer(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::RENDERBUFFER,
                Some(color),
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            let result = device.check().and_then(|()| {
                if status == glow::FRAMEBUFFER_COMPLETE {
                    Ok(())
                } else {
                    Err(Error::Backend(format!(
                        "GLES work framebuffer {status:#x}, size {size:?}"
                    )))
                }
            });
            gl.bind_renderbuffer(glow::RENDERBUFFER, previous);
            bindings.restore(gl, framebuffer);
            if let Err(error) = result {
                gl.delete_framebuffer(framebuffer);
                gl.delete_renderbuffer(color);
                return Err(error);
            }
            Ok(Self {
                color,
                framebuffer,
                size,
                _permit: permit,
            })
        }
    }
    unsafe fn destroy(self, gl: &glow::Context) {
        unsafe {
            gl.delete_framebuffer(self.framebuffer);
            gl.delete_renderbuffer(self.color);
        }
    }
}
impl Work {
    pub fn new(device: &Device, size: Size, budget: &Budget) -> Result<Self> {
        let surface = Surface::new(device, size, budget)?;
        match copy_program(&device.gl) {
            Ok(program) => Ok(Self {
                surface,
                source_uniform: unsafe { device.gl.get_uniform_location(program, "u_source") },
                region_uniform: unsafe { device.gl.get_uniform_location(program, "u_region") },
                region: Cell::new(None),
                destination_uniform: unsafe {
                    device.gl.get_uniform_location(program, "u_destination")
                },
                destination: Cell::new(None),
                program,
                budget: budget.clone(),
                current: None,
            }),
            Err(error) => {
                unsafe {
                    surface.destroy(&device.gl);
                }
                Err(error)
            }
        }
    }
    pub fn contains(&self, texture: &Texture) -> bool {
        self.current
            .as_ref()
            .is_some_and(|pending| Weak::ptr_eq(&pending.owner, &texture.weak))
    }
    pub fn invalidate(&mut self) {
        self.current = None;
    }
    pub fn target(
        &mut self,
        device: &Device,
        texture: &Texture,
        dirty: Option<Rect>,
        overwrite: bool,
    ) -> Result<glow::NativeFramebuffer> {
        self.select(device, texture, dirty, overwrite)
    }
    pub fn clear(&mut self, device: &Device, texture: &Texture) -> Result<()> {
        let framebuffer = self.select(device, texture, Some(texture.size.rect()), true)?;
        unsafe {
            let gl = &device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, texture.size.width as i32, texture.size.height as i32);
            gl.color_mask(true, true, true, true);
            gl.clear_color(0., 0., 0., 0.);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        device.check()
    }
    fn select(
        &mut self,
        device: &Device,
        texture: &Texture,
        dirty: Option<Rect>,
        discard: bool,
    ) -> Result<glow::NativeFramebuffer> {
        let requested = dirty.unwrap_or(texture.size.rect());
        self.select_region(device, texture, requested, dirty, discard)
    }
    /// Load a whole draw batch once, without introducing dependencies between
    /// its disjoint writes. Actual writes are registered separately by target.
    pub fn prepare(
        &mut self,
        device: &Device,
        texture: &Texture,
        requested: Rect,
    ) -> Result<glow::NativeFramebuffer> {
        self.select_region(device, texture, requested, None, false)
    }
    fn select_region(
        &mut self,
        device: &Device,
        texture: &Texture,
        requested: Rect,
        dirty: Option<Rect>,
        discard: bool,
    ) -> Result<glow::NativeFramebuffer> {
        if !self.contains(texture) {
            self.store(device, false)?;
            self.current = None;
            if texture.size.width > self.surface.size.width
                || texture.size.height > self.surface.size.height
            {
                let size = Size {
                    width: texture.size.width.max(self.surface.size.width),
                    height: texture.size.height.max(self.surface.size.height),
                };
                let next = Surface::new(device, size, &self.budget)?;
                // Growth is exceptional. Ordinary target switches never wait,
                // allocate a texture, or create/delete a native render surface.
                unsafe {
                    device.gl.finish();
                    std::mem::replace(&mut self.surface, next).destroy(&device.gl);
                }
            }
            if !discard {
                self.load(device, texture, requested)?;
            } else {
                // PVR tex.c only takes the framebuffer-to-texture HWTQ path
                // for GLES2_LOADED_LEVEL. A fresh/redefined texture otherwise
                // causes synchronous CPU readback. Sample once to make the
                // whole allocation resident; this one pixel is then replaced.
                // Repeat on target switches: uploads and driver reclamation can
                // invalidate residency without changing the GL texture name.
                self.load(
                    device,
                    texture,
                    Rect {
                        left: requested.left,
                        top: requested.top,
                        width: 1,
                        height: 1,
                    },
                )?;
            }
            self.current = Some(Pending {
                owner: texture.weak.clone(),
                name: texture.name(),
                valid: requested,
                dirty: Dirty::default(),
            });
        } else {
            let valid = self.current.as_ref().unwrap().valid;
            let expanded = crate::scene_damage::union(Some(valid), requested);
            // Fill the uncovered strips, including any gap between two writes.
            // Never reload pixels already modified in the working surface.
            for area in uncovered(expanded, valid).into_iter().flatten() {
                if discard {
                    // The pending surface may contain earlier writes outside
                    // this overwrite. Load only gaps between the two regions;
                    // neither the old writes nor the new pixels need a load.
                    for gap in subtract(area, requested).into_iter().flatten() {
                        self.load(device, texture, gap)?;
                    }
                } else {
                    self.load(device, texture, area)?;
                }
            }
            self.current.as_mut().unwrap().valid = expanded;
        }
        let pending = self.current.as_mut().unwrap();
        if let Some(area) = dirty {
            pending.dirty.add(area);
        }
        Ok(self.surface.framebuffer)
    }
    pub fn sample(&mut self, device: &Device) -> Result<()> {
        // Presentation samples with the default FBO bound: commit once. A
        // pass sampling its pending target must retain the upcoming write.
        let rendering = unsafe { device.gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32 }
            == self.surface.framebuffer.0.get();
        self.store(device, rendering)
    }
    pub fn backdrop(&mut self, device: &Device, area: Rect) -> Result<()> {
        // The backing still contains correct pixels outside pending writes.
        // Adjacent glyphs can share it without a render/transfer barrier between
        // every character. Overlaps (including shadows) must see earlier writes.
        if self
            .current
            .as_ref()
            .is_some_and(|p| p.dirty.intersects(area))
        {
            self.store_region(device, false, Some(area))?;
        }
        Ok(())
    }
    pub fn store(&mut self, device: &Device, keep_dirty: bool) -> Result<()> {
        self.store_region(device, keep_dirty, None)
    }
    fn store_region(
        &mut self,
        device: &Device,
        keep_dirty: bool,
        clip: Option<Rect>,
    ) -> Result<()> {
        let Some(pending) = self.current.as_mut() else {
            return Ok(());
        };
        let Some(owner) = pending.owner.upgrade() else {
            self.current = None;
            return Ok(());
        };
        let Some(area) = pending.dirty.bounds else {
            return Ok(());
        };
        let _profile =
            krkr_protocol::profile::span_detail("gpu.work.store", || format!("area={area:?}"));
        if owner.size.rect().intersection(area) != Some(area)
            || self.surface.size.rect().intersection(area) != Some(area)
        {
            return Err(Error::Backend(format!(
                "work store outside bounds: surface={:?} target={:?} dirty={area:?}",
                self.surface.size, owner.size
            )));
        }
        // A local blend only samples its own rectangle. Preserve the other
        // dirty regions on-chip until presentation or another pass needs them.
        // Resolving the entire dirty screen for each portrait/text overlap
        // wastes transfer bandwidth and forces additional tile-store work.
        let transfers = || {
            pending
                .dirty
                .transfers()
                .iter()
                .filter_map(|area| clip.map_or(Some(*area), |clip| area.intersection(clip)))
        };
        let gl = &device.gl;
        // Attribute deferred draw errors to the preceding pass, not to a copy
        // that hasn't run. Restore bindings even when the copy itself fails.
        device
            .check()
            .map_err(|error| Error::Backend(format!("before work store: {error}")))?;
        unsafe {
            let bindings = Bindings::save(gl);
            if bindings.framebuffer != Some(self.surface.framebuffer) {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(self.surface.framebuffer));
            }
            gl.bind_texture(glow::TEXTURE_2D, Some(pending.name));
            // Resident backing permits the transfer path, but PVR may still
            // fall back for unsupported layouts/regions. The linear source
            // avoids its full-surface untwiddle allocation in that case.
            for area in transfers() {
                gl.copy_tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    area.left,
                    area.top,
                    area.left,
                    area.top,
                    area.width as i32,
                    area.height as i32,
                );
            }
            let result = device.check();
            bindings.restore(gl, self.surface.framebuffer);
            result.map_err(|error| {
                let message = format!(
                    "work store CopyTexSubImage2D: RGBA8 renderbuffer={:?}, target={:?} {:?}, dirty={area:?}: {error}",
                    self.surface.size, pending.name, owner.size
                );
                Error::Backend(message)
            })?;
        }
        device.check()?;
        if !keep_dirty {
            if let Some(clip) = clip {
                pending.dirty.remove(clip);
            } else {
                pending.dirty = Dirty::default();
            }
        }
        Ok(())
    }
    pub fn copy(
        &mut self,
        device: &Device,
        source: &Texture,
        region: Rect,
        target: &Texture,
        destination: Rect,
    ) -> Result<()> {
        // Resolve before selecting the target, including overlapping self-copy.
        if self.contains(source) {
            self.store(device, false)?;
        }
        // Reused canvases can receive this copy directly. Keep self-copies on
        // the independent surface, where the source remains a stable snapshot.
        if source.name() != target.name()
            && !self.contains(target)
            && let Some(framebuffer) = device.targets.borrow_mut().get(target.name())
        {
            return self.blit_to(
                device,
                source.name(),
                source.size,
                region,
                destination,
                framebuffer,
            );
        }
        self.target(device, target, Some(destination), true)?;
        // A shader write keeps fresh destinations on GPU. CopyTexSubImage2D
        // into a never-sampled texture makes PVR read the framebuffer on CPU.
        self.blit(device, source, region, destination)
    }
    fn load(&self, device: &Device, texture: &Texture, area: Rect) -> Result<()> {
        self.blit(device, texture, area, area)
    }
    fn blit(
        &self,
        device: &Device,
        texture: &Texture,
        region: Rect,
        destination: Rect,
    ) -> Result<()> {
        self.blit_name(device, texture.name(), texture.size, region, destination)
    }
    // Sampling materializes compressed levels that would otherwise remain
    // in the driver's host cache until the image first appears in a scene.
    pub fn make_textures_resident(
        &mut self,
        device: &Device,
        sources: &[(glow::NativeTexture, Size)],
    ) -> Result<()> {
        self.store(device, false)?;
        self.current = None;
        let pixel = Rect {
            width: 1,
            height: 1,
            ..Default::default()
        };
        for &(name, size) in sources {
            self.blit_name(device, name, size, pixel, pixel)?;
        }
        unsafe {
            device.gl.finish();
        }
        device.check()
    }
    fn blit_name(
        &self,
        device: &Device,
        name: glow::NativeTexture,
        size: Size,
        region: Rect,
        destination: Rect,
    ) -> Result<()> {
        self.blit_to(
            device,
            name,
            size,
            region,
            destination,
            self.surface.framebuffer,
        )
    }
    fn blit_to(
        &self,
        device: &Device,
        name: glow::NativeTexture,
        size: Size,
        region: Rect,
        destination: Rect,
        framebuffer: glow::NativeFramebuffer,
    ) -> Result<()> {
        let gl = &device.gl;
        unsafe {
            let bindings = Bindings::save(gl);
            let program = NonZeroU32::new(gl.get_parameter_i32(glow::CURRENT_PROGRAM) as u32)
                .map(glow::NativeProgram);
            let buffer = NonZeroU32::new(gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING) as u32)
                .map(glow::NativeBuffer);
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut mask = [0; 4];
            gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut mask);
            let blend = gl.is_enabled(glow::BLEND);
            let scissor = gl.is_enabled(glow::SCISSOR_TEST);
            gl.bind_texture(glow::TEXTURE_2D, Some(name));
            let min = gl.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER);
            let mag = gl.get_tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER);
            if min != glow::NEAREST as i32 {
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MIN_FILTER,
                    glow::NEAREST as i32,
                );
            }
            if mag != glow::NEAREST as i32 {
                gl.tex_parameter_i32(
                    glow::TEXTURE_2D,
                    glow::TEXTURE_MAG_FILTER,
                    glow::NEAREST as i32,
                );
            }
            if bindings.framebuffer != Some(framebuffer) {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            }
            // Express an in-bounds copy as quad geometry. Shrinking and then
            // restoring the viewport invalidates PVR's drawmask twice per load.
            // An empty or smaller caller viewport still uses the original path.
            let keep_viewport = viewport[2] > 0
                && viewport[3] > 0
                && destination.left >= viewport[0]
                && destination.top >= viewport[1]
                && i64::from(destination.left) + i64::from(destination.width)
                    <= i64::from(viewport[0]) + i64::from(viewport[2])
                && i64::from(destination.top) + i64::from(destination.height)
                    <= i64::from(viewport[1]) + i64::from(viewport[3]);
            let placement = if keep_viewport {
                [
                    (destination.left as f32 - viewport[0] as f32) / viewport[2] as f32,
                    (destination.top as f32 - viewport[1] as f32) / viewport[3] as f32,
                    destination.width as f32 / viewport[2] as f32,
                    destination.height as f32 / viewport[3] as f32,
                ]
            } else {
                gl.viewport(
                    destination.left,
                    destination.top,
                    destination.width as i32,
                    destination.height as i32,
                );
                [0., 0., 1., 1.]
            };
            if blend {
                gl.disable(glow::BLEND);
            }
            if scissor {
                gl.disable(glow::SCISSOR_TEST);
            }
            let masked = mask.contains(&0);
            if masked {
                gl.color_mask(true, true, true, true);
            }
            // This temporary pass preserves the caller's program and buffer
            // below and uses attribute 2, leaving DrawState's quad layout on
            // attributes 0/1 intact.
            gl.use_program(Some(self.program));
            if self.region.get().is_none() {
                gl.uniform_1_i32(self.source_uniform.as_ref(), 7);
            }
            let region = [
                region.left as f32 / size.width as f32,
                region.top as f32 / size.height as f32,
                region.width as f32 / size.width as f32,
                region.height as f32 / size.height as f32,
            ];
            if self.region.get() != Some(region) {
                gl.uniform_4_f32(
                    self.region_uniform.as_ref(),
                    region[0],
                    region[1],
                    region[2],
                    region[3],
                );
                self.region.set(Some(region));
            }
            if self.destination.get() != Some(placement) {
                gl.uniform_4_f32(
                    self.destination_uniform.as_ref(),
                    placement[0],
                    placement[1],
                    placement[2],
                    placement[3],
                );
                self.destination.set(Some(placement));
            }
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(device.quad));
            gl.vertex_attrib_pointer_f32(2, 2, glow::FLOAT, false, 8, 0);
            gl.enable_vertex_attrib_array(2);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.disable_vertex_attrib_array(2);
            if min != glow::NEAREST as i32 {
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, min);
            }
            if mag != glow::NEAREST as i32 {
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, mag);
            }
            gl.use_program(program);
            gl.bind_buffer(glow::ARRAY_BUFFER, buffer);
            if !keep_viewport {
                gl.viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
            }
            if masked {
                gl.color_mask(mask[0] != 0, mask[1] != 0, mask[2] != 0, mask[3] != 0);
            }
            if blend {
                gl.enable(glow::BLEND);
            }
            if scissor {
                gl.enable(glow::SCISSOR_TEST);
            }
            bindings.restore(gl, framebuffer);
        }
        device.check()
    }
    pub unsafe fn destroy(self, gl: &glow::Context) {
        unsafe {
            self.surface.destroy(gl);
            gl.delete_program(self.program);
        }
    }
}
// Both rectangles are inside the texture, and `outer` contains `inner`.
// The four non-overlapping strips preserve all previous pending writes.
fn uncovered(outer: Rect, inner: Rect) -> [Option<Rect>; 4] {
    let right = inner.left + inner.width as i32;
    let bottom = inner.top + inner.height as i32;
    let strip = |left, top, width: i32, height: i32| {
        (width > 0 && height > 0).then_some(Rect {
            left,
            top,
            width: width as u32,
            height: height as u32,
        })
    };
    [
        strip(
            outer.left,
            outer.top,
            outer.width as i32,
            inner.top - outer.top,
        ),
        strip(
            outer.left,
            bottom,
            outer.width as i32,
            outer.top + outer.height as i32 - bottom,
        ),
        strip(
            outer.left,
            inner.top,
            inner.left - outer.left,
            inner.height as i32,
        ),
        strip(
            right,
            inner.top,
            outer.left + outer.width as i32 - right,
            inner.height as i32,
        ),
    ]
}
fn subtract(area: Rect, overwrite: Rect) -> [Option<Rect>; 4] {
    match area.intersection(overwrite) {
        Some(overlap) => uncovered(area, overlap),
        None => [Some(area), None, None, None],
    }
}
fn copy_program(gl: &glow::Context) -> Result<glow::NativeProgram> {
    unsafe {
        let program = gl.create_program().map_err(Error::Backend)?;
        let mut shaders = Vec::new();
        let result = (|| {
            for (stage, source) in [
                (
                    glow::VERTEX_SHADER,
                    "#version 100\nattribute vec2 a_unit; uniform highp vec4 u_region; uniform highp vec4 u_destination; varying highp vec2 v_uv; void main() { v_uv=u_region.xy+a_unit*u_region.zw; gl_Position=vec4((u_destination.xy+a_unit*u_destination.zw)*2.0-1.0,0.0,1.0); }",
                ),
                (
                    glow::FRAGMENT_SHADER,
                    "#version 100\nprecision mediump float; varying highp vec2 v_uv; uniform sampler2D u_source; void main() { gl_FragColor=texture2D(u_source,v_uv); }",
                ),
            ] {
                let shader = gl.create_shader(stage).map_err(Error::Backend)?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let error = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    return Err(Error::Backend(error));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.bind_attrib_location(program, 2, "a_unit");
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(Error::Backend(gl.get_program_info_log(program)));
            }
            Ok(program)
        })();
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        if result.is_err() {
            gl.delete_program(program);
        }
        result
    }
}

//! Indexed textured geometry stays on GPU. Immutable assets are cached by
//! lifetime; mask and alpha-fallback surfaces are reused between ordered draws.
use crate::{
    Error, Gpu, Image, Result,
    device::{Buffer, Device, Texture as Surface},
    drawing::rect,
    image::Tile,
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::{
    budget::Permit,
    graphics::{ImageId, Rect, Size},
    mesh::{Batch, Blend, Draw, Texture},
    pixels::Pixels,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    rc::Rc,
    sync::{Arc, Weak},
};

struct Cached {
    owner: Weak<Pixels>,
    image: Image,
    _permit: Permit,
}
pub(crate) struct Renderer {
    program: Program,
    blend_max: bool,
    textures: BTreeMap<usize, Cached>,
    mask: Option<Rc<Surface>>,
    previous: Option<Rc<Surface>>,
}
struct Range {
    vertex: i32,
    index: i32,
    count: i32,
}
struct LinearSampler<'a> {
    gpu: &'a Gpu,
    texture: Rc<Surface>,
}
impl LinearSampler<'_> {
    fn set(&self, filter: u32) {
        unsafe {
            self.gpu.device.gl.active_texture(glow::TEXTURE0);
            self.gpu
                .device
                .gl
                .bind_texture(glow::TEXTURE_2D, Some(self.texture.name()));
        }
        unsafe {
            for name in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                self.gpu
                    .device
                    .gl
                    .tex_parameter_i32(glow::TEXTURE_2D, name, filter as i32);
            }
        }
    }
}
impl Drop for LinearSampler<'_> {
    fn drop(&mut self) {
        self.set(glow::NEAREST);
    }
}
/// Borrows the validated batch so its geometry, order and masks cannot change
/// after admission. Image sources are fixed before any target clear or write.
pub struct PreparedMeshes<'a> {
    device: Rc<Device>,
    batch: &'a Batch,
    textures: Vec<Image>,
    vertices: Buffer,
    indices: Buffer,
    ranges: Vec<Range>,
    _permit: Permit,
}
impl Renderer {
    fn new(gpu: &Gpu) -> Result<Self> {
        Ok(Self {
            program: Program::new(
                gpu.device.clone(),
                include_str!("mesh.vert"),
                &format!(
                    "{}\n{}",
                    include_str!("tiles.glsl"),
                    include_str!("mesh.frag")
                ),
            )?,
            blend_max: gpu
                .device
                .gl
                .supported_extensions()
                .contains("GL_EXT_blend_minmax"),
            textures: BTreeMap::new(),
            mask: None,
            previous: None,
        })
    }
    fn scratch(slot: &mut Option<Rc<Surface>>, gpu: &Gpu, size: Size, needed: bool) -> Result<()> {
        if !needed {
            slot.take();
            return Ok(());
        }
        if slot.as_ref().is_some_and(|s| s.size == size) {
            return Ok(());
        }
        slot.take();
        *slot = Some(gpu.device.texture(size, &gpu.scratch)?);
        Ok(())
    }
}
fn bytes(count: usize, stride: usize) -> Result<usize> {
    count
        .checked_mul(stride)
        .ok_or(Error::Message("mesh storage overflow"))
}
fn masked(batch: &Batch, draw: &Draw) -> bool {
    draw.masks.iter().any(|&m| batch.draws[m].opacity > 0.)
}
fn validate(batch: &Batch) -> Result<()> {
    if batch.order.iter().any(|&i| i >= batch.draws.len())
        || batch
            .clear
            .is_some_and(|c| c.iter().any(|n| !n.is_finite()))
    {
        return Err(Error::Message("invalid mesh order or clear color"));
    }
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
    }
    Ok(())
}
impl Gpu {
    /// Count only new immutable assets; warm animation frames must not evict
    /// unrelated caches to reserve textures that are already resident.
    pub fn mesh_upload_bytes(&self, batch: &Batch) -> Result<usize> {
        let _metadata = self
            .staging
            .reserve(bytes(batch.draws.len(), 32)?.saturating_add(64))?;
        let cell = self.meshes.borrow();
        let mut seen = HashSet::with_capacity(batch.draws.len());
        let mut total = 0usize;
        for draw in &batch.draws {
            let Texture::Pixels(pixels) = &draw.texture else {
                continue;
            };
            let key = Arc::as_ptr(pixels) as usize;
            if !seen.insert(key)
                || cell.as_ref().is_some_and(|r| {
                    r.textures.get(&key).is_some_and(|entry| {
                        entry
                            .owner
                            .upgrade()
                            .is_some_and(|p| Arc::ptr_eq(&p, pixels))
                    })
                })
            {
                continue;
            }
            total = total.saturating_add(pixels.size.rgba_bytes().unwrap_or(usize::MAX));
        }
        Ok(total)
    }
    pub fn collect_mesh_textures(&self) {
        if let Some(renderer) = self.meshes.borrow_mut().as_mut() {
            renderer
                .textures
                .retain(|_, entry| entry.owner.strong_count() != 0);
        }
    }
    pub fn prepare_meshes<'a>(
        &self,
        batch: &'a Batch,
        images: &HashMap<ImageId, Image>,
    ) -> Result<PreparedMeshes<'a>> {
        validate(batch)?;
        let mut vertex_count = 0usize;
        let mut index_count = 0usize;
        for draw in &batch.draws {
            vertex_count = vertex_count
                .checked_add(draw.geometry.vertices.len())
                .ok_or(Error::Message("mesh vertex count overflow"))?;
            index_count = index_count
                .checked_add(draw.geometry.indices.len())
                .ok_or(Error::Message("mesh index count overflow"))?;
        }
        let vertex_bytes = bytes(vertex_count, 16)?;
        let index_bytes = bytes(index_count, 2)?;
        if vertex_bytes > i32::MAX as usize || index_bytes > i32::MAX as usize {
            return Err(Error::Message("mesh buffers exceed GLES address limits"));
        }
        let permit = self.staging.reserve(bytes(
            batch.draws.len(),
            std::mem::size_of::<Range>() + std::mem::size_of::<Image>(),
        )?)?;
        let _cpu = self
            .staging
            .reserve(vertex_bytes.saturating_add(index_bytes))?;
        let mut vertices = Vec::<[f32; 4]>::with_capacity(vertex_count);
        let mut indices = Vec::<u16>::with_capacity(index_count);
        let mut ranges = Vec::with_capacity(batch.draws.len());
        for draw in &batch.draws {
            ranges.push(Range {
                vertex: (vertices.len() * 16) as i32,
                index: (indices.len() * 2) as i32,
                count: draw.geometry.indices.len() as i32,
            });
            vertices.extend(
                draw.geometry
                    .vertices
                    .iter()
                    .map(|v| [v.position[0], v.position[1], v.uv[0], v.uv[1]]),
            );
            indices.extend_from_slice(&draw.geometry.indices);
        }
        let vertices = self.device.buffer(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(&vertices),
            &self.staging,
        )?;
        let indices = self.device.buffer(
            glow::ELEMENT_ARRAY_BUFFER,
            bytemuck::cast_slice(&indices),
            &self.staging,
        )?;
        let mut cell = self.meshes.borrow_mut();
        if cell.is_none() {
            *cell = Some(Renderer::new(self)?);
        }
        let renderer = cell.as_mut().unwrap();
        renderer
            .textures
            .retain(|_, entry| entry.owner.strong_count() != 0);
        let mut textures = Vec::with_capacity(batch.draws.len());
        for draw in &batch.draws {
            let source = match &draw.texture {
                Texture::Image(reference) => images
                    .get(&reference.id)
                    .ok_or(Error::Message("mesh texture has been released"))?
                    .shared_main(),
                Texture::Pixels(pixels) => {
                    let key = Arc::as_ptr(pixels) as usize;
                    if !renderer.textures.get(&key).is_some_and(|entry| {
                        entry
                            .owner
                            .upgrade()
                            .is_some_and(|p| Arc::ptr_eq(&p, pixels))
                    }) {
                        let permit = self
                            .staging
                            .reserve(std::mem::size_of::<Cached>() * 2 + 64)?;
                        let image = self.assign_bitmap(None, pixels)?;
                        renderer.textures.insert(
                            key,
                            Cached {
                                owner: Arc::downgrade(pixels),
                                image,
                                _permit: permit,
                            },
                        );
                    }
                    renderer.textures[&key].image.shared_main()
                }
            };
            self.check_image(&source)?;
            textures.push(self.compact_source(&source)?.unwrap_or(source));
        }
        Ok(PreparedMeshes {
            device: self.device.clone(),
            batch,
            textures,
            vertices,
            indices,
            ranges,
            _permit: permit,
        })
    }
    pub fn draw_meshes(&self, target: &mut Image, prepared: PreparedMeshes<'_>) -> Result<()> {
        self.check_image(target)?;
        if !Rc::ptr_eq(&self.device, &prepared.device) {
            return Err(Error::Message(
                "mesh resources belong to another GLES context",
            ));
        }
        self.writable_compact(target, target.size.rect(), false)?;
        let plane = target.plane(false)?;
        let canvas = plane.size;
        let size = Size {
            width: plane.tiles.iter().map(|t| t.rectangle.width).max().unwrap(),
            height: plane
                .tiles
                .iter()
                .map(|t| t.rectangle.height)
                .max()
                .unwrap(),
        };
        let mut cell = self.meshes.borrow_mut();
        let renderer = cell.as_mut().expect("prepared mesh renderer");
        let batch = prepared.batch;
        let has_mask = batch
            .order
            .iter()
            .any(|&i| batch.draws[i].visible && masked(batch, &batch.draws[i]));
        let needs_previous = !renderer.blend_max
            && batch.order.iter().any(|&i| {
                let draw = &batch.draws[i];
                draw.visible
                    && (matches!(draw.blend, Blend::AlphaMax)
                        || draw.masks.iter().any(|&m| {
                            batch.draws[m].visible
                                && batch.draws[m].opacity > 0.
                                && matches!(batch.draws[m].blend, Blend::AlphaMax)
                        }))
            });
        Renderer::scratch(&mut renderer.mask, self, size, has_mask)?;
        Renderer::scratch(&mut renderer.previous, self, size, needs_previous)?;
        // Consecutive draws commonly share an atlas. Keep its filter until a
        // different source is needed, restoring nearest on exit or any error.
        let mut linear = None;
        for tile in &target.plane(false)?.tiles {
            if let Some(color) = batch.clear {
                clear(self, &tile.texture, color)?;
            }
            let mut masks: Option<&[usize]> = None;
            for &index in &batch.order {
                let draw = &batch.draws[index];
                if !draw.visible {
                    continue;
                }
                let has_mask = masked(batch, draw);
                if has_mask && masks != Some(draw.masks.as_slice()) {
                    let texture = renderer.mask.as_ref().expect("allocated mesh mask");
                    clear(self, texture, [0.; 4])?;
                    let mask_tile = Tile {
                        backing: None,
                        rectangle: tile.rectangle,
                        texture: texture.clone(),
                    };
                    for &m in &draw.masks {
                        if batch.draws[m].visible && batch.draws[m].opacity > 0. {
                            renderer.issue(
                                self,
                                &mask_tile,
                                canvas,
                                &prepared,
                                m,
                                None,
                                &mut linear,
                            )?;
                        }
                    }
                    masks = Some(&draw.masks);
                }
                renderer.issue(
                    self,
                    tile,
                    canvas,
                    &prepared,
                    index,
                    if has_mask {
                        renderer.mask.as_deref()
                    } else {
                        None
                    },
                    &mut linear,
                )?;
            }
        }
        drop(linear);
        self.device.check()
    }
}
fn clear(gpu: &Gpu, texture: &Surface, color: [f32; 4]) -> Result<()> {
    unsafe {
        let gl = &gpu.device.gl;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(texture.framebuffer()?));
        gl.enable(glow::SCISSOR_TEST);
        gl.scissor(0, 0, texture.size.width as i32, texture.size.height as i32);
        gl.color_mask(true, true, true, true);
        gl.clear_color(color[0], color[1], color[2], color[3]);
        gl.clear(glow::COLOR_BUFFER_BIT);
    }
    Ok(())
}
impl Renderer {
    fn target(&self, gpu: &Gpu, tile: &Tile) -> Result<()> {
        unsafe {
            let gl = &gpu.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(tile.texture.framebuffer()?));
            gl.viewport(
                0,
                0,
                tile.rectangle.width as i32,
                tile.rectangle.height as i32,
            );
            gl.disable(glow::SCISSOR_TEST);
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    fn issue<'a>(
        &self,
        gpu: &'a Gpu,
        target: &Tile,
        canvas: Size,
        prepared: &PreparedMeshes<'_>,
        index: usize,
        mask: Option<&Surface>,
        linear: &mut Option<LinearSampler<'a>>,
    ) -> Result<()> {
        let draw = &prepared.batch.draws[index];
        let range = &prepared.ranges[index];
        if range.count == 0 {
            return Ok(());
        }
        let source = &prepared.textures[index];
        let plane = source.plane(false)?;
        let fallback = matches!(draw.blend, Blend::AlphaMax) && !self.blend_max;
        // A normal one-texture atlas uses the hardware bilinear sampler.
        // Compact logical assets and tile borders retain the four-tap path.
        gpu.device.before_sample(&plane.tiles[0].texture)?;
        let use_linear = plane.tiles.len() == 1 && plane.size == source.size;
        let same_sampler = use_linear
            && linear
                .as_ref()
                .is_some_and(|s| Rc::ptr_eq(&s.texture, &plane.tiles[0].texture));
        if !same_sampler {
            // Reset before replacing the source, including transitions to a
            // tiled/compact path that requires nearest sampling.
            drop(linear.take());
        }
        if use_linear && !same_sampler {
            let sampler = LinearSampler {
                gpu,
                texture: plane.tiles[0].texture.clone(),
            };
            sampler.set(glow::LINEAR);
            *linear = Some(sampler);
        }
        let program = &self.program;
        // Mesh vertices immediately replace the quad layout; bind only the
        // program so no discarded quad pointer/buffer state is submitted.
        program.bind_program();
        gpu.device.draw_state.invalidate_vertices();
        unsafe {
            let gl = &gpu.device.gl;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(prepared.vertices.name));
            gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(prepared.indices.name));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, range.vertex);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, range.vertex + 8);
        }
        program.four("u_target", rect(target.rectangle));
        program.two("u_canvas", canvas.width as f32, canvas.height as f32);
        program.four("u_color", draw.color);
        program.one("u_channel", f32::from(use_linear));
        program.four(
            "u_operation",
            [
                draw.opacity,
                f32::from(draw.solid_color),
                f32::from(mask.is_some()),
                f32::from(matches!(draw.blend, Blend::LayerAlpha)),
            ],
        );
        program.four(
            "u_extent",
            [
                source.size.width as f32,
                source.size.height as f32,
                plane.size.width as f32 / source.size.width as f32,
                plane.size.height as f32 / source.size.height as f32,
            ],
        );
        let mask = mask.unwrap_or(&gpu.lookup);
        program.two(
            "u_mask_size",
            mask.size.width as f32,
            mask.size.height as f32,
        );
        gpu.bind_texture(4, mask)?;
        let previous = self.previous.as_deref().unwrap_or(&gpu.lookup);
        program.two(
            "u_previous_size",
            previous.size.width as f32,
            previous.size.height as f32,
        );
        gpu.bind_texture(5, previous)?;
        // Multi-tile UVs can select different tiles for overlapping triangles.
        // Keep triangles outside the tile loop, otherwise painter order flips.
        let step = if plane.tiles.len() == 1 && !fallback {
            range.count
        } else {
            3
        };
        for offset in (0..range.count).step_by(step as usize) {
            if step == 3 {
                let Some(bounds) = triangle_bounds(draw, offset as usize, canvas, target.rectangle)
                else {
                    continue;
                };
                if fallback {
                    let local = Rect {
                        left: bounds.left - target.rectangle.left,
                        top: bounds.top - target.rectangle.top,
                        ..bounds
                    };
                    gpu.device.copy_region_at(
                        &target.texture,
                        local,
                        previous,
                        local.left,
                        local.top,
                    )?;
                }
            }
            self.target(gpu, target)?;
            program.one("u_kind", 0.);
            unsafe {
                let gl = &gpu.device.gl;
                gl.enable(glow::BLEND);
                gl.color_mask(true, true, true, !fallback);
                gl.blend_equation(glow::FUNC_ADD);
                match draw.blend {
                    Blend::AlphaMax => {
                        if !fallback {
                            gl.blend_equation_separate(glow::FUNC_ADD, glow::MAX);
                        }
                        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                    }
                    Blend::MultiplyAdd => {
                        gl.blend_func_separate(glow::DST_COLOR, glow::ONE, glow::ZERO, glow::ONE)
                    }
                    Blend::Alpha | Blend::LayerAlpha => {
                        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA)
                    }
                }
            }
            for tile in &plane.tiles {
                gpu.bind_neighbours(program, plane, tile)?;
                unsafe {
                    gpu.device.gl.draw_elements(
                        glow::TRIANGLES,
                        step,
                        glow::UNSIGNED_SHORT,
                        range.index + offset * 2,
                    );
                }
            }
            if fallback {
                program.one("u_kind", 1.);
                unsafe {
                    gpu.device.gl.disable(glow::BLEND);
                    gpu.device.gl.color_mask(false, false, false, true);
                }
                for tile in &plane.tiles {
                    gpu.bind_neighbours(program, plane, tile)?;
                    unsafe {
                        gpu.device.gl.draw_elements(
                            glow::TRIANGLES,
                            step,
                            glow::UNSIGNED_SHORT,
                            range.index + offset * 2,
                        );
                    }
                }
            }
        }
        gpu.device.check()
    }
}
fn triangle_bounds(draw: &Draw, first: usize, canvas: Size, clip: Rect) -> Option<Rect> {
    let vertices = &draw.geometry.vertices;
    let points = draw.geometry.indices[first..first + 3]
        .iter()
        .map(|&i| vertices[usize::from(i)].position);
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in points {
        for (axis, length) in [canvas.width, canvas.height].into_iter().enumerate() {
            let q = (f64::from(p[axis]) + 1.) * 0.5 * f64::from(length);
            lo[axis] = lo[axis].min(q.floor() - 1.);
            hi[axis] = hi[axis].max(q.ceil() + 1.);
        }
    }
    let left = lo[0].max(f64::from(clip.left));
    let top = lo[1].max(f64::from(clip.top));
    let right = hi[0].min(f64::from(clip.left) + f64::from(clip.width));
    let bottom = hi[1].min(f64::from(clip.top) + f64::from(clip.height));
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect {
        left: left as i32,
        top: top as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

#[cfg(all(test, target_os = "linux"))]
#[path = "../tests/mesh/internal.rs"]
mod tests;

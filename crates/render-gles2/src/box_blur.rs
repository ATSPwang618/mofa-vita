//! Separable integer sums with one final average. Scratch storage is reused
//! across channels and output blocks; source pixels always stay on the GPU.
//! The work-surface backend keeps two small native FBOs for the alternating
//! sums, avoiding a renderbuffer load/store around every accumulation pass.
use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{Draw, rect},
    image::{Plane, Tile},
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::graphics::{Rect, Size};
use std::{collections::HashMap, rc::Rc};

struct Plan {
    block: Size,
    sums: Size,
    pool: Size,
    horizontal: bool,
    bytes: usize,
}
struct SparseOutput {
    logical: Rect,
    physical: Rect,
    xs: Vec<u32>,
    ys: Vec<u32>,
}
impl SparseOutput {
    fn bytes(&self) -> usize {
        self.physical.width as usize * self.physical.height as usize * 4 + 4
    }
}
impl Plan {
    fn new(block: Size, image: Size, radius: [u32; 2], edge: u32, alpha: bool) -> Self {
        let horizontal = Size {
            width: block.width,
            height: image.height.min(block.height.saturating_add(radius[1] * 2)),
        };
        let vertical = Size {
            width: image.width.min(block.width.saturating_add(radius[0] * 2)),
            height: block.height,
        };
        let make = |sums: Size, horizontal| {
            let pool = Size {
                width: block.width.max(sums.width.min(edge)),
                height: block.height.max(sums.height.min(edge)),
            };
            Self {
                block,
                sums,
                pool,
                horizontal,
                bytes: sums
                    .rgba_bytes()
                    .unwrap()
                    .saturating_add(pool.rgba_bytes().unwrap() * 2)
                    .saturating_add(usize::from(alpha) * block.rgba_bytes().unwrap()),
            }
        };
        let a = make(horizontal, true);
        let b = make(vertical, false);
        if a.bytes <= b.bytes { a } else { b }
    }
}

fn expand(area: Rect, radius: u32, horizontal: bool, size: Size) -> Rect {
    let (origin, count, limit) = if horizontal {
        (area.left as u32, area.width, size.width)
    } else {
        (area.top as u32, area.height, size.height)
    };
    let start = origin.saturating_sub(radius);
    let end = origin
        .saturating_add(count)
        .saturating_add(radius)
        .min(limit);
    if horizontal {
        Rect {
            left: start as i32,
            width: end - start,
            ..area
        }
    } else {
        Rect {
            top: start as i32,
            height: end - start,
            ..area
        }
    }
}

struct BoxSource<'a> {
    texture: &'a Texture,
    rectangle: Rect,
    scale: [f32; 2],
}

/// A streamed blur keeps its seam gather alive between strips. Charging it
/// again would shrink the next strip or collect it after every output band.
#[derive(Default)]
pub(crate) struct BoxInput(Option<Rc<Texture>>);
impl BoxInput {
    fn required(&self, size: Size) -> usize {
        if self
            .0
            .as_ref()
            .is_some_and(|t| t.size.width >= size.width && t.size.height >= size.height)
        {
            0
        } else {
            size.rgba_bytes().unwrap()
        }
    }
}

fn box_source_tile(source: &Plane, canvas: Size, footprint: Rect) -> Option<BoxSource<'_>> {
    let scale = [
        source.size.width as f32 / canvas.width as f32,
        source.size.height as f32 / canvas.height as f32,
    ];
    let physical = if source.size == canvas {
        footprint
    } else {
        // The kernel still visits logical pixel centers. Bound their stored
        // nearest samples, with a guard for shader coordinate rounding.
        let axis = |start: i32, length: u32, scale: f32, limit: u32| {
            let low =
                (((start as f32 + 0.5) * scale).floor() as i64 - 1).clamp(0, i64::from(limit));
            let high = (((start as f32 + length as f32 - 0.5) * scale).floor() as i64 + 2)
                .clamp(0, i64::from(limit));
            (low as i32, (high - low) as u32)
        };
        let (left, width) = axis(footprint.left, footprint.width, scale[0], source.size.width);
        let (top, height) = axis(
            footprint.top,
            footprint.height,
            scale[1],
            source.size.height,
        );
        Rect {
            left,
            top,
            width,
            height,
        }
    };
    let tile = source.tiles.iter().find(|tile| {
        let sampling = tile.sample_rectangle();
        tile.texture.size.width == sampling.width
            && tile.texture.size.height == sampling.height
            && tile.rectangle.intersection(physical) == Some(physical)
    })?;
    Some(BoxSource {
        texture: &tile.texture,
        rectangle: tile.sample_rectangle(),
        scale,
    })
}

// A small kernel needs a gather only at source-tile seams. Merge adjacent
// blocks whose complete halo fits one texture; the existing small blocks
// still bound scratch allocation wherever pixels must be gathered.
fn small_box_bands(
    source: &Plane,
    canvas: Size,
    area: Rect,
    radius: [u32; 2],
    block: Size,
) -> Vec<Rect> {
    let direct = |area| {
        box_source_tile(
            source,
            canvas,
            expand(
                expand(area, radius[0], true, canvas),
                radius[1],
                false,
                canvas,
            ),
        )
        .is_some()
    };
    if direct(area) {
        return vec![area];
    }
    let mut bands: Vec<Rect> = Vec::new();
    let mut above = HashMap::<(i32, u32), usize>::new();
    for y in (0..area.height).step_by(block.height as usize) {
        let mut row: Vec<Rect> = Vec::new();
        for x in (0..area.width).step_by(block.width as usize) {
            let band = Rect {
                left: area.left + x as i32,
                top: area.top + y as i32,
                width: block.width.min(area.width - x),
                height: block.height.min(area.height - y),
            };
            if let Some(previous) = row.last_mut() {
                let merged = Rect {
                    width: previous.width + band.width,
                    ..*previous
                };
                if direct(merged) {
                    *previous = merged;
                    continue;
                }
            }
            row.push(band);
        }
        let mut below = HashMap::new();
        for band in row {
            let key = (band.left, band.width);
            if let Some(&index) = above.get(&key) {
                let previous = bands[index];
                let merged = Rect {
                    height: previous.height + band.height,
                    ..previous
                };
                if direct(merged) {
                    bands[index] = merged;
                    below.insert(key, index);
                    continue;
                }
            }
            below.insert(key, bands.len());
            bands.push(band);
        }
        above = below;
    }
    bands
}

impl Gpu {
    pub(crate) fn box_output_size(&self, image: &Image, area: Rect) -> Size {
        if area == image.size.rect() && image.canvas {
            self.canvas_storage(image.size, Some(image))
        } else {
            image.size
        }
    }
    fn sparse_box_output(
        &self,
        image: &Image,
        area: Rect,
        radius: [u32; 2],
    ) -> Option<SparseOutput> {
        if !image.canvas || !self.device.streamed_uploads() || area != image.size.rect() {
            return None;
        }
        let input = image.main.as_ref()?;
        let stored = self.box_output_size(image, area);
        if stored.rgba_bytes()? < 256 * 1024 {
            return None;
        }
        // Only metadata-proven transparent black is omitted. Unknown alpha,
        // compressed pixels and cropped views conservatively contribute their
        // entire tile. All source aliases stay immutable throughout the blur.
        let active = input
            .tiles
            .iter()
            .filter_map(|tile| {
                if tile.texture.solid_color() == Some(0) {
                    return None;
                }
                if tile.renderable()
                    && let Some((0, Some(damage))) = tile.texture.constant_background()
                {
                    return tile.rectangle.intersection(Rect {
                        left: tile.rectangle.left + damage.left,
                        top: tile.rectangle.top + damage.top,
                        ..damage
                    });
                }
                Some(tile.rectangle)
            })
            .reduce(|a, b| crate::scene_damage::union(Some(a), b))?;
        let axis = |start: i32, length: u32, logical: u32, physical: u32, radius: u32| {
            let guard = logical.div_ceil(physical).saturating_add(1);
            let margin = radius.saturating_add(guard);
            let lo = (u64::from(start as u32) * u64::from(logical) / u64::from(physical)) as u32;
            let hi = ((u64::from(start as u32) + u64::from(length)) * u64::from(logical))
                .div_ceil(u64::from(physical)) as u32;
            (
                lo.saturating_sub(margin),
                hi.saturating_add(margin).min(logical),
            )
        };
        let (left, right) = axis(
            active.left,
            active.width,
            image.size.width,
            input.size.width,
            radius[0],
        );
        let (top, bottom) = axis(
            active.top,
            active.height,
            image.size.height,
            input.size.height,
            radius[1],
        );
        let logical = Rect {
            left: left as i32,
            top: top as i32,
            width: right - left,
            height: bottom - top,
        };
        let physical = crate::scene::raster::Raster::new(image.size, stored, (0, 0))
            .ok()?
            .rect(logical)?;
        if u64::from(physical.width) * u64::from(physical.height) * 8
            >= u64::from(stored.width) * u64::from(stored.height) * 7
        {
            return None;
        }
        let boundaries = |start: u32, count: u32, end: u32| {
            let mut points = vec![0, start + count, end];
            points.extend((start..start + count).step_by(self.tile_edge as usize));
            points.sort_unstable();
            points.dedup();
            points
        };
        let xs = boundaries(physical.left as u32, physical.width, stored.width);
        let ys = boundaries(physical.top as u32, physical.height, stored.height);
        ((xs.len() - 1) * (ys.len() - 1) <= 128).then_some(SparseOutput {
            logical,
            physical,
            xs,
            ys,
        })
    }
    pub(crate) fn box_output_bytes(&self, image: &Image, area: Rect, radius: [u32; 2]) -> usize {
        self.sparse_box_output(image, area, radius).map_or_else(
            || {
                self.box_output_size(image, area)
                    .rgba_bytes()
                    .unwrap_or(usize::MAX)
            },
            |plan| plan.bytes(),
        )
    }
    fn box_output(&self, image: &Image, area: Rect, radius: [u32; 2]) -> Result<(Image, Rect)> {
        let mut next = image.shared();
        if let Some(plan) = self.sparse_box_output(image, area, radius) {
            drop(self.resident.reserve(plan.bytes())?);
            let solid = self.canvas_solid_tile(
                Size {
                    width: 1,
                    height: 1,
                },
                0,
                true,
            )?;
            let mut tiles = Vec::with_capacity((plan.xs.len() - 1) * (plan.ys.len() - 1));
            for y in plan.ys.windows(2) {
                for x in plan.xs.windows(2) {
                    let rectangle = Rect {
                        left: x[0] as i32,
                        top: y[0] as i32,
                        width: x[1] - x[0],
                        height: y[1] - y[0],
                    };
                    let texture = if rectangle.intersection(plan.physical).is_some() {
                        self.device.sample_texture(
                            Size {
                                width: rectangle.width,
                                height: rectangle.height,
                            },
                            &self.resident,
                        )?
                    } else {
                        solid.clone()
                    };
                    tiles.push(Tile {
                        rectangle,
                        backing: None,
                        texture,
                    });
                }
            }
            next.main = Some(Rc::new(Plane {
                size: self.box_output_size(image, area),
                budget: self.resident.clone(),
                tiles,
            }));
            return Ok((next, plan.logical));
        }
        if area == image.size.rect() {
            // Every output pixel is replaced. Keep the immutable input for
            // halo samples, without copying it into the output first.
            next.main =
                Some(self.overwrite_plane(self.box_output_size(image, area), &self.resident)?);
        } else {
            self.writable(&mut next, area, false)?;
        }
        Ok((next, area))
    }
    fn box_cached_bytes(&self, size: Size) -> usize {
        self.box_blur_targets
            .borrow()
            .as_ref()
            .filter(|pool| pool[0].size == size && pool[0].belongs_to(&self.scratch))
            .map_or(0, |_| size.rgba_bytes().unwrap() * 2)
    }

    fn box_plan(&self, block: Size, image: Size, radius: [u32; 2], edge: u32, alpha: bool) -> Plan {
        let mut plan = Plan::new(block, image, radius, edge, alpha);
        let pool = self.box_pool_size(plan.pool);
        // Charge actual cached dimensions even when only a smaller clip is used.
        plan.bytes -= plan.pool.rgba_bytes().unwrap() * 2;
        plan.pool = pool;
        plan.bytes += plan.pool.rgba_bytes().unwrap() * 2;
        plan
    }

    fn box_pool_size(&self, requested: Size) -> Size {
        if let Some(pool) = self.box_blur_targets.borrow().as_ref()
            && pool[0].belongs_to(&self.scratch)
            && pool[0].size.width >= requested.width
            && pool[0].size.height >= requested.height
        {
            pool[0].size
        } else {
            requested
        }
    }

    fn box_pool(&self, size: Size) -> Result<[Rc<Texture>; 2]> {
        if !self.device.streamed_uploads() {
            return Ok([
                self.device.texture(size, &self.scratch)?,
                self.device.texture(size, &self.scratch)?,
            ]);
        }
        if self.box_cached_bytes(size) != 0 {
            return Ok(self.box_blur_targets.borrow().as_ref().unwrap().clone());
        }
        // Release old native render surfaces before creating their replacements.
        // Ordinary reuse does not allocate, reattach, or wait for the GPU.
        self.box_blur_targets.borrow_mut().take();
        self.device.collect()?;
        let allocate_work = || -> Result<[Rc<Texture>; 2]> {
            Ok([
                self.device.texture(size, &self.scratch)?,
                self.device.texture(size, &self.scratch)?,
            ])
        };
        let pool = if self.box_blur_native_disabled.get() {
            allocate_work()?
        } else {
            let native: Result<_> = (|| {
                Ok([
                    self.device.render_texture(size, &self.scratch)?,
                    self.device.render_texture(size, &self.scratch)?,
                ])
            })();
            match native {
                Ok(pool) => pool,
                Err(Error::Backend(message)) if message.starts_with("GLES error 0x0505 at ") => {
                    // Native attachments need driver memory outside texture permits.
                    // No blur draw has run: retire partial allocations and use the
                    // existing work surface without another native attachment.
                    self.box_blur_native_disabled.set(true);
                    self.device.collect()?;
                    allocate_work()?
                }
                Err(error) => return Err(error),
            }
        };
        *self.box_blur_targets.borrow_mut() = Some(pool.clone());
        Ok(pool)
    }

    pub(crate) fn box_blur(
        &self,
        image: &mut Image,
        area: Rect,
        radius: [u32; 2],
        alpha: bool,
    ) -> Result<()> {
        let kernel = (u64::from(radius[0]) * 2 + 1)
            .checked_mul(u64::from(radius[1]) * 2 + 1)
            .filter(|area| *area < 1 << 24)
            .ok_or(Error::Message(
                "box blur area must be smaller than 16 million pixels",
            ))?;
        if radius == [0, 0] {
            return Ok(());
        }
        if let Some(plan) = self.stream_blur_plan(image, area, radius) {
            return self.stream_box_blur(image, radius, alpha, plan);
        }
        let radius = [
            radius[0].min(image.size.width - 1),
            radius[1].min(image.size.height - 1),
        ];
        if kernel <= 81 {
            return self.small_box_blur(image, area, radius, alpha);
        }
        if radius.iter().all(|&r| r <= 64) {
            return self.packed_box_blur(image, area, radius, alpha, kernel < 256);
        }
        // Resident and scratch may share a parent budget. Reserve the durable
        // result first so adaptive scratch blocks use only the remaining room.
        // Planning scratch first can admit both allocations independently yet
        // leave too little for the full output when they coexist.
        let source = image.shared_main();
        let (next, area) = self.box_output(image, area, radius)?;
        let edge = self.tile_edge.min(256);
        let mut block = Size {
            width: area.width.min(edge),
            height: area.height.min(edge),
        };
        let mut plan = self.box_plan(block, image.size, radius, edge, alpha);
        if plan.bytes - self.box_cached_bytes(plan.pool) > self.scratch.available() {
            self.collect()?;
            plan = self.box_plan(block, image.size, radius, edge, alpha);
        }
        while plan.bytes - self.box_cached_bytes(plan.pool) > self.scratch.available()
            && (block.width > 1 || block.height > 1)
        {
            if block.width >= block.height && block.width > 1 {
                block.width = block.width.div_ceil(2);
            } else {
                block.height = block.height.div_ceil(2);
            }
            plan = self.box_plan(block, image.size, radius, edge, alpha);
        }
        drop(
            self.scratch
                .reserve(plan.bytes - self.box_cached_bytes(plan.pool))?,
        );
        let mut programs = self.box_blur_programs.borrow_mut();
        if programs.is_none() {
            let create = |mode| {
                Program::new(
                    self.device.clone(),
                    include_str!("quad.vert"),
                    &format!(
                        "#define BOX_STAGE {mode}\n{}\n{}",
                        include_str!("integer.glsl"),
                        include_str!("box_blur.frag")
                    ),
                )
            };
            *programs = Some([create(0)?, create(1)?]);
        }
        let [sum_program, finish_program] = programs.as_ref().unwrap();
        let sums = self.plane_with_edge(plan.sums, &self.scratch, edge)?;
        let pool = self.box_pool(plan.pool)?;
        let saved_alpha = alpha
            .then(|| self.device.texture(plan.block, &self.scratch))
            .transpose()?;
        // Allocate all resources before changing pixels. Keep the original main
        // plane until every block completes, then publish the replacement.
        let original = source.plane(false)?;
        let output = next.plane(false)?;
        let raster = crate::scene::raster::Raster::new(image.size, output.size, (0, 0))?;
        let output_scale = [
            image.size.width as f32 / output.size.width as f32,
            image.size.height as f32 / output.size.height as f32,
        ];
        let ratio = [
            original.size.width as f32 / source.size.width as f32,
            original.size.height as f32 / source.size.height as f32,
        ];
        let first = usize::from(!plan.horizontal);
        let second = 1 - first;
        for y in (0..area.height).step_by(plan.block.height as usize) {
            for x in (0..area.width).step_by(plan.block.width as usize) {
                let band = Rect {
                    left: area.left + x as i32,
                    top: area.top + y as i32,
                    width: plan.block.width.min(area.width - x),
                    height: plan.block.height.min(area.height - y),
                };
                let footprint = expand(band, radius[second], !plan.horizontal, image.size);
                let local = Size {
                    width: footprint.width,
                    height: footprint.height,
                }
                .rect();
                for channel in [3, 0, 1, 2] {
                    for tile in &sums.tiles {
                        let Some(part) = tile.rectangle.intersection(local) else {
                            continue;
                        };
                        let world = Rect {
                            left: footprint.left + part.left,
                            top: footprint.top + part.top,
                            ..part
                        };
                        let value = self.box_sum(
                            sum_program,
                            &pool,
                            original,
                            [0, 0],
                            ratio,
                            image.size,
                            world,
                            radius[first],
                            plan.horizontal,
                            Some((channel, alpha)),
                        )?;
                        self.device.copy_region_at(
                            &value,
                            Size {
                                width: part.width,
                                height: part.height,
                            }
                            .rect(),
                            &tile.texture,
                            0,
                            0,
                        )?;
                    }
                    let total = self.box_sum(
                        sum_program,
                        &pool,
                        &sums,
                        [footprint.left, footprint.top],
                        [1., 1.],
                        image.size,
                        band,
                        radius[second],
                        !plan.horizontal,
                        None,
                    )?;
                    if let Some(saved) = &saved_alpha
                        && channel == 3
                    {
                        self.box_finish(
                            finish_program,
                            saved,
                            band,
                            band,
                            &total,
                            [band.left, band.top],
                            None,
                            image.size,
                            radius,
                            kernel < 256,
                            false,
                            [true; 4],
                            [1., 1.],
                        )?;
                    }
                    for tile in &next.plane(false)?.tiles {
                        let Some(part) = raster
                            .rect(band)
                            .and_then(|r| r.intersection(tile.rectangle))
                        else {
                            continue;
                        };
                        let mut mask = [false; 4];
                        mask[channel] = true;
                        if channel == 3 {
                            // RGB follows in this same band; its previous
                            // contents will never be sampled or published.
                            mask = [true; 4];
                        }
                        self.box_finish(
                            finish_program,
                            &tile.texture,
                            tile.rectangle,
                            part,
                            &total,
                            [band.left, band.top],
                            saved_alpha.as_deref(),
                            image.size,
                            radius,
                            kernel < 256,
                            alpha && channel != 3,
                            mask,
                            output_scale,
                        )?;
                    }
                }
            }
        }
        *image = next;
        Ok(())
    }

    /// Up to 129 horizontal samples fit in 16 bits per channel. Two RGBA8
    /// surfaces retain all four sums; the final vertical pass averages once.
    /// The largest complete sum is 129 * 129 * 255, exact in highp's 24 bits.
    fn packed_box_blur(
        &self,
        image: &mut Image,
        area: Rect,
        radius: [u32; 2],
        alpha: bool,
        reciprocal: bool,
    ) -> Result<()> {
        let source = image.shared_main();
        let (next, area) = self.box_output(image, area, radius)?;
        self.packed_box_blur_into(
            &source,
            &next,
            area,
            radius,
            alpha,
            reciprocal,
            &mut BoxInput::default(),
        )?;
        *image = next;
        Ok(())
    }

    fn box_input<'a>(
        &self,
        source: &'a Plane,
        canvas: Size,
        footprint: Rect,
        input: &'a mut BoxInput,
        input_size: Size,
    ) -> Result<BoxSource<'a>> {
        if let Some(source) = box_source_tile(source, canvas, footprint) {
            // The entire kernel neighborhood already lives in one texture.
            // Preserve logical kernel samples in the shader; compact storage
            // alone does not require an expanded temporary texture.
            return Ok(source);
        }
        // Most blocks already fit one source tile. Allocate a seam gather
        // only when needed, and let the complete footprint overwrite it.
        if input.required(input_size) != 0 {
            input.0.take();
            input.0 = Some(self.device.sample_texture(input_size, &self.scratch)?);
        }
        let input = input.0.as_ref().unwrap();
        let rectangle = Rect {
            left: footprint.left,
            top: footprint.top,
            ..input.size.rect()
        };
        let gathered = Plane {
            size: canvas,
            budget: self.scratch.clone(),
            tiles: vec![Tile {
                backing: None,
                texture: input.clone(),
                rectangle,
            }],
        };
        let sx = source.size.width as f32 / canvas.width as f32;
        let sy = source.size.height as f32 / canvas.height as f32;
        self.draw(
            &gathered,
            Some(source),
            footprint,
            &Draw::copy(
                [sx, 0., (sx - 1.) * 0.5, 0., sy, (sy - 1.) * 0.5],
                [true; 4],
            ),
        )?;
        Ok(BoxSource {
            texture: input,
            rectangle,
            scale: [1., 1.],
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn packed_box_blur_into(
        &self,
        source: &Image,
        next: &Image,
        area: Rect,
        radius: [u32; 2],
        alpha: bool,
        reciprocal: bool,
        input: &mut BoxInput,
    ) -> Result<()> {
        let _profile = krkr_protocol::profile::span("gpu.blur.packed");
        let image = source;
        let direct = box_source_tile(
            source.plane(false)?,
            image.size,
            expand(
                expand(area, radius[0], true, image.size),
                radius[1],
                false,
                image.size,
            ),
        )
        .is_some();
        let edge = self.tile_edge.min(256);
        let mut block = Size {
            width: area
                .width
                .min(edge.min(self.tile_edge.saturating_sub(radius[0] * 2).max(1))),
            height: area
                .height
                .min(edge.min(self.tile_edge.saturating_sub(radius[1] * 2).max(1))),
        };
        let sizes = |b: Size| {
            let input = Size {
                width: image.size.width.min(b.width + radius[0] * 2),
                height: image.size.height.min(b.height + radius[1] * 2),
            };
            let sums = self.box_pool_size(Size {
                width: b.width,
                height: input.height,
            });
            (input, sums, sums.rgba_bytes().unwrap() * 2)
        };
        let required = |block| {
            let (size, sums, bytes) = sizes(block);
            bytes - self.box_cached_bytes(sums) + if direct { 0 } else { input.required(size) }
        };
        if required(block) > self.scratch.available() {
            self.collect()?;
        }
        while required(block) > self.scratch.available() && (block.width > 1 || block.height > 1) {
            if block.width >= block.height && block.width > 1 {
                block.width = block.width.div_ceil(2);
            } else {
                block.height = block.height.div_ceil(2);
            }
        }
        let (input_size, sum_size, _) = sizes(block);
        drop(self.scratch.reserve(required(block))?);
        let sums = self.box_pool(sum_size)?;
        let mut programs = self.packed_box_blur_programs.borrow_mut();
        if programs.is_none() {
            let create = |stage| {
                Program::new(
                    self.device.clone(),
                    include_str!("quad.vert"),
                    &format!(
                        "#define PACKED_STAGE {stage}\n{}",
                        include_str!("box_blur_packed.frag")
                    ),
                )
            };
            *programs = Some([create(0)?, create(1)?]);
        }
        let [horizontal, vertical] = programs.as_ref().unwrap();
        let original = source.plane(false)?;
        let output = next.plane(false)?;
        let raster = crate::scene::raster::Raster::new(image.size, output.size, (0, 0))?;
        let scale = [
            image.size.width as f32 / output.size.width as f32,
            image.size.height as f32 / output.size.height as f32,
        ];
        for y in (0..area.height).step_by(block.height as usize) {
            for x in (0..area.width).step_by(block.width as usize) {
                let band = Rect {
                    left: area.left + x as i32,
                    top: area.top + y as i32,
                    width: block.width.min(area.width - x),
                    height: block.height.min(area.height - y),
                };
                let footprint = expand(
                    expand(band, radius[0], true, image.size),
                    radius[1],
                    false,
                    image.size,
                );
                let sample = self.box_input(original, image.size, footprint, input, input_size)?;
                let middle = Rect {
                    left: band.left,
                    top: footprint.top,
                    width: band.width,
                    height: footprint.height,
                };
                for (high, sum) in sums.iter().enumerate() {
                    self.box_target(horizontal, sum, middle, middle, [true; 4])?;
                    self.bind_texture(0, sample.texture)?;
                    horizontal.two("u_source_scale", sample.scale[0], sample.scale[1]);
                    horizontal.two(
                        "u_source_origin",
                        sample.rectangle.left as f32,
                        sample.rectangle.top as f32,
                    );
                    horizontal.two(
                        "u_source_size",
                        sample.rectangle.width as f32,
                        sample.rectangle.height as f32,
                    );
                    horizontal.two(
                        "u_canvas",
                        image.size.width as f32,
                        image.size.height as f32,
                    );
                    horizontal.four(
                        "u_operation",
                        [radius[0] as f32, radius[1] as f32, f32::from(alpha), 0.],
                    );
                    horizontal.one("u_kind", high as f32);
                    unsafe {
                        self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    }
                    self.device.check()?;
                }
                for tile in &output.tiles {
                    let Some(part) = raster
                        .rect(band)
                        .and_then(|r| r.intersection(tile.rectangle))
                    else {
                        continue;
                    };
                    self.box_target(vertical, &tile.texture, tile.rectangle, part, [true; 4])?;
                    self.bind_texture(0, &sums[0])?;
                    self.bind_texture(1, &sums[1])?;
                    vertical.two("u_source_origin", middle.left as f32, middle.top as f32);
                    vertical.two(
                        "u_source_size",
                        sum_size.width as f32,
                        sum_size.height as f32,
                    );
                    vertical.two(
                        "u_canvas",
                        image.size.width as f32,
                        image.size.height as f32,
                    );
                    vertical.two("u_output_scale", scale[0], scale[1]);
                    vertical.four(
                        "u_operation",
                        [
                            radius[0] as f32,
                            radius[1] as f32,
                            f32::from(alpha),
                            f32::from(reciprocal),
                        ],
                    );
                    unsafe {
                        self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    }
                    self.device.check()?;
                }
            }
        }
        Ok(())
    }

    /// Small kernels fit exact RGBA sums and the legacy reciprocal product in
    /// highp's 24-bit integer range. Average all channels together, without
    /// per-channel packed sums, ping-pong passes or an alpha scratch texture.
    fn small_box_blur(
        &self,
        image: &mut Image,
        area: Rect,
        radius: [u32; 2],
        alpha: bool,
    ) -> Result<()> {
        let source = image.shared_main();
        let (next, area) = self.box_output(image, area, radius)?;
        self.small_box_blur_into(
            &source,
            &next,
            area,
            radius,
            alpha,
            &mut BoxInput::default(),
        )?;
        *image = next;
        Ok(())
    }

    pub(crate) fn small_box_blur_into(
        &self,
        source: &Image,
        next: &Image,
        area: Rect,
        radius: [u32; 2],
        alpha: bool,
        input: &mut BoxInput,
    ) -> Result<()> {
        let _profile = krkr_protocol::profile::span("gpu.blur.small");
        let image = source;
        let direct = box_source_tile(
            source.plane(false)?,
            image.size,
            expand(
                expand(area, radius[0], true, image.size),
                radius[1],
                false,
                image.size,
            ),
        )
        .is_some();
        let edge = self.tile_edge.min(256);
        // Leave space for the halo inside the work surface's tile extent.
        // Otherwise gathering border pixels grows the persistent surface
        // precisely when streaming was chosen to relieve memory pressure.
        let mut block = Size {
            width: area
                .width
                .min(edge.min(self.tile_edge.saturating_sub(radius[0] * 2).max(1))),
            height: area
                .height
                .min(edge.min(self.tile_edge.saturating_sub(radius[1] * 2).max(1))),
        };
        let pool_size = |block: Size| Size {
            width: image.size.width.min(block.width + radius[0] * 2),
            height: image.size.height.min(block.height + radius[1] * 2),
        };
        if !direct && input.required(pool_size(block)) > self.scratch.available() {
            self.collect()?;
        }
        while !direct
            && input.required(pool_size(block)) > self.scratch.available()
            && (block.width > 1 || block.height > 1)
        {
            if block.width >= block.height && block.width > 1 {
                block.width = block.width.div_ceil(2);
            } else {
                block.height = block.height.div_ceil(2);
            }
        }
        let input_size = pool_size(block);
        let mut cached = self.small_box_blur_program.borrow_mut();
        if cached.is_none() {
            *cached = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                include_str!("box_blur_small.frag"),
            )?);
        }
        let program = cached.as_ref().unwrap();
        let original = source.plane(false)?;
        let output = next.plane(false)?;
        let raster = crate::scene::raster::Raster::new(image.size, output.size, (0, 0))?;
        let output_scale = [
            image.size.width as f32 / output.size.width as f32,
            image.size.height as f32 / output.size.height as f32,
        ];
        for band in small_box_bands(original, image.size, area, radius, block) {
            let footprint = expand(
                expand(band, radius[0], true, image.size),
                radius[1],
                false,
                image.size,
            );
            // Reuse one bounded halo texture. Gather logical nearest pixels
            // across source tiles before filtering; never clamp at a tile seam.
            let sample = self.box_input(original, image.size, footprint, input, input_size)?;
            for tile in &next.plane(false)?.tiles {
                let Some(part) = raster
                    .rect(band)
                    .and_then(|r| r.intersection(tile.rectangle))
                else {
                    continue;
                };
                self.box_target(program, &tile.texture, tile.rectangle, part, [true; 4])?;
                program.two("u_output_scale", output_scale[0], output_scale[1]);
                self.bind_texture(0, sample.texture)?;
                program.two("u_source_scale", sample.scale[0], sample.scale[1]);
                program.two(
                    "u_source_origin",
                    sample.rectangle.left as f32,
                    sample.rectangle.top as f32,
                );
                program.two(
                    "u_source_size",
                    sample.rectangle.width as f32,
                    sample.rectangle.height as f32,
                );
                program.two(
                    "u_canvas",
                    image.size.width as f32,
                    image.size.height as f32,
                );
                program.four(
                    "u_operation",
                    [radius[0] as f32, radius[1] as f32, f32::from(alpha), 0.],
                );
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
                self.device.check()?;
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn box_sum(
        &self,
        program: &Program,
        pool: &[Rc<Texture>; 2],
        source: &Plane,
        origin: [i32; 2],
        ratio: [f32; 2],
        image: Size,
        area: Rect,
        radius: u32,
        horizontal: bool,
        channel: Option<(usize, bool)>,
    ) -> Result<Rc<Texture>> {
        let mut previous = 0;
        let local = Size {
            width: area.width,
            height: area.height,
        }
        .rect();
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(
                glow::FRAMEBUFFER,
                Some(pool[previous].overwrite_framebuffer(local)?),
            );
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, local.width as i32, local.height as i32);
            gl.color_mask(true, true, true, true);
            gl.clear_color(0., 0., 0., 0.);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        for base in (0..radius * 2 + 1).step_by(64) {
            let offset = base as i32 - radius as i32;
            let count = (radius * 2 + 1 - base).min(64);
            let mut bounds = area;
            if horizontal {
                bounds.left = bounds.left.saturating_add(offset);
                bounds.width = bounds.width.saturating_add(count - 1);
            } else {
                bounds.top = bounds.top.saturating_add(offset);
                bounds.height = bounds.height.saturating_add(count - 1);
            }
            let Some(bounds) = bounds.intersection(image.rect()) else {
                continue;
            };
            let left = (f64::from(bounds.left) * f64::from(ratio[0])).floor() as i32 - 1;
            let top = (f64::from(bounds.top) * f64::from(ratio[1])).floor() as i32 - 1;
            let right = ((f64::from(bounds.left) + f64::from(bounds.width)) * f64::from(ratio[0]))
                .ceil() as i32
                + 1;
            let bottom = ((f64::from(bounds.top) + f64::from(bounds.height)) * f64::from(ratio[1]))
                .ceil() as i32
                + 1;
            let physical = Rect {
                left,
                top,
                width: (right - left) as u32,
                height: (bottom - top) as u32,
            };
            for input in &source.tiles {
                let input_rect = Rect {
                    left: input.rectangle.left + origin[0],
                    top: input.rectangle.top + origin[1],
                    ..input.rectangle
                };
                if input_rect.intersection(physical).is_none() {
                    continue;
                }
                let next = 1 - previous;
                self.box_target(program, &pool[next], area, area, [true; 4])?;
                self.bind_texture(0, &input.texture)?;
                program.four("u_source_visible", crate::drawing::rect(input_rect));
                self.bind_texture(1, &pool[previous])?;
                program.two(
                    "u_source_origin",
                    (input.sample_rectangle().left + origin[0]) as f32,
                    (input.sample_rectangle().top + origin[1]) as f32,
                );
                program.two(
                    "u_source_size",
                    input.sample_rectangle().width as f32,
                    input.sample_rectangle().height as f32,
                );
                program.two("u_backdrop_origin", area.left as f32, area.top as f32);
                program.two(
                    "u_backdrop_size",
                    pool[previous].size.width as f32,
                    pool[previous].size.height as f32,
                );
                program.two("u_source_scale", ratio[0], ratio[1]);
                program.two("u_canvas", image.width as f32, image.height as f32);
                program.one("u_offset", offset as f32);
                program.one("u_kind", count as f32);
                program.three(
                    "u_operation",
                    [
                        f32::from(horizontal),
                        f32::from(channel.is_some()),
                        f32::from(channel.is_some_and(|(_, alpha)| alpha)),
                    ],
                );
                let mut select = [0.; 4];
                if let Some((c, _)) = channel {
                    select[c] = 1.;
                }
                program.four("u_channel", select);
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
                self.device.check()?;
                previous = next;
            }
        }
        Ok(pool[previous].clone())
    }

    fn box_target(
        &self,
        program: &Program,
        target: &Texture,
        tile: Rect,
        area: Rect,
        mask: [bool; 4],
    ) -> Result<()> {
        program.bind();
        let local = Rect {
            left: area.left - tile.left,
            top: area.top - tile.top,
            ..area
        };
        // Sum passes overwrite all channels without discard or blending. Final
        // per-channel writes must retain the other channels, but only within
        // this band, not the full (possibly logical-resolution) destination.
        let framebuffer = if mask == [true; 4] {
            target.overwrite_framebuffer(local)?
        } else {
            target.framebuffer_region(local)?
        };
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.viewport(0, 0, target.size.width as i32, target.size.height as i32);
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
        }
        program.four(
            "u_target",
            [
                tile.left as f32,
                tile.top as f32,
                target.size.width as f32,
                target.size.height as f32,
            ],
        );
        program.four("u_rectangle", rect(area));
        program.one("u_flip", 1.);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn box_finish(
        &self,
        program: &Program,
        target: &Texture,
        tile: Rect,
        area: Rect,
        total: &Texture,
        origin: [i32; 2],
        alpha: Option<&Texture>,
        image: Size,
        radius: [u32; 2],
        reciprocal: bool,
        unpremultiply: bool,
        mask: [bool; 4],
        output_scale: [f32; 2],
    ) -> Result<()> {
        self.box_target(program, target, tile, area, mask)?;
        program.two("u_output_scale", output_scale[0], output_scale[1]);
        self.bind_texture(0, total)?;
        self.bind_texture(2, alpha.unwrap_or(&self.lookup))?;
        program.two("u_source_origin", origin[0] as f32, origin[1] as f32);
        program.two(
            "u_source_size",
            total.size.width as f32,
            total.size.height as f32,
        );
        let alpha_size = alpha.map_or(total.size, |t| t.size);
        program.two(
            "u_table_size",
            alpha_size.width as f32,
            alpha_size.height as f32,
        );
        program.two("u_canvas", image.width as f32, image.height as f32);
        program.four(
            "u_operation",
            [
                radius[0] as f32,
                radius[1] as f32,
                f32::from(reciprocal),
                f32::from(unpremultiply),
            ],
        );
        unsafe {
            self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        self.device.check()
    }
}

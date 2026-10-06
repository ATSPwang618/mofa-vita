//! Page atlases keep glyph masks on GPU. Runs retain CPU masks only through the
//! engine's font budget; drawing and overlap never read back destination pixels.
use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{Draw, face, rect, rgba},
    image::{Plane, Tile},
};
use glow::HasContext;
use krkr_protocol::{
    budget::Permit,
    graphics::{DrawFace, Rect, Size},
    text::{Glyph, Run, Style},
};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

struct Page {
    id: u64,
    texture: Rc<Texture>,
    layout: PageLayout,
    touched: u64,
    shadow: Option<krkr_protocol::pixels::Bytes>,
    dirty: Option<Rect>,
}
struct Entry {
    page: u64,
    rectangle: Rect,
    _permit: Permit,
}
struct Slot {
    texture: Rc<Texture>,
    rectangle: Rect,
}
#[derive(Default)]
pub(crate) struct Atlas {
    pages: Vec<Page>,
    entries: HashMap<u64, Entry>,
    clock: u64,
}
#[derive(Clone, Copy, Default)]
struct PageLayout {
    x: u32,
    y: u32,
    row_height: u32,
}
impl PageLayout {
    fn allocate(&mut self, bounds: Size, size: Size) -> Option<Rect> {
        let (x, y, height) = if self.x + size.width > bounds.width {
            (0, self.y + self.row_height, 0)
        } else {
            (self.x, self.y, self.row_height)
        };
        if size.width > bounds.width || y + size.height > bounds.height {
            return None;
        }
        self.x = x + size.width;
        self.y = y;
        self.row_height = height.max(size.height);
        Some(Rect {
            left: x as i32,
            top: y as i32,
            ..size.rect()
        })
    }
}
impl Page {
    fn allocate(&mut self, size: Size) -> Option<Rect> {
        self.layout.allocate(self.texture.size, size)
    }
}
impl Atlas {
    fn allocation_bytes(&self, run: &Run, clip: Rect) -> usize {
        if run.glyphs.iter().all(|p| {
            glyph_area(p).intersection(clip).is_none() || self.entries.contains_key(&p.glyph.id)
        }) {
            return 0;
        }
        // Simulate only page placement and LRU pins. No texture references are
        // cloned: preflight must not turn a reusable page into a pinned one.
        struct PlannedPage {
            id: u64,
            size: Size,
            layout: PageLayout,
            touched: u64,
            pinned: bool,
        }
        let mut pages: Vec<_> = self
            .pages
            .iter()
            .map(|p| PlannedPage {
                id: p.id,
                size: p.texture.size,
                layout: p.layout,
                touched: p.touched,
                pinned: Rc::strong_count(&p.texture) > 1,
            })
            .collect();
        let mut resolved = HashSet::new();
        let mut bytes = 0usize;
        let mut clock = self.clock;
        for placed in &run.glyphs {
            if glyph_area(placed).intersection(clip).is_none() {
                continue;
            }
            clock += 1;
            let glyph = &placed.glyph;
            if !resolved.insert(glyph.id) {
                continue;
            }
            if let Some(entry) = self.entries.get(&glyph.id)
                && let Some(page) = pages.iter_mut().find(|p| p.id == entry.page)
            {
                page.touched = clock;
                page.pinned = true;
                continue;
            }
            bytes = bytes.saturating_add(128);
            let Some(padded) = padded_size(glyph) else {
                return usize::MAX;
            };
            if let Some(page) = pages
                .iter_mut()
                .find_map(|p| p.layout.allocate(p.size, padded).map(|_| p))
            {
                page.touched = clock;
                page.pinned = true;
                continue;
            }
            if pages.len() >= 4 {
                let Some(index) = pages
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| !p.pinned)
                    .min_by_key(|(_, p)| p.touched)
                    .map(|(i, _)| i)
                else {
                    return usize::MAX;
                };
                pages.remove(index);
            }
            let size = Size {
                width: 512.max(padded.width),
                height: 512.max(padded.height),
            };
            let Some(rgba) = size.rgba_bytes() else {
                return usize::MAX;
            };
            bytes = bytes.saturating_add(rgba / 4);
            let mut layout = PageLayout::default();
            layout.allocate(size, padded).expect("new glyph page");
            pages.push(PlannedPage {
                id: clock,
                size,
                layout,
                touched: clock,
                pinned: true,
            });
        }
        bytes
    }
    fn resolve(&mut self, gpu: &Gpu, glyph: &Glyph) -> Result<Slot> {
        self.clock += 1;
        if let Some(entry) = self.entries.get(&glyph.id) {
            let page = self
                .pages
                .iter_mut()
                .find(|page| page.id == entry.page)
                .expect("glyph page");
            page.touched = self.clock;
            return Ok(Slot {
                texture: page.texture.clone(),
                rectangle: entry.rectangle,
            });
        }
        // Also charge the table entry and its amortized hash-table capacity.
        let metadata = gpu.resident.reserve(128)?;
        // Sampling at a compact canvas edge can land one texel outside the
        // mask after floating-point interpolation. Keep a zero-coverage moat
        // rather than exposing another glyph (or uninitialized page storage).
        let padded = padded_size(glyph).ok_or(Error::Message("glyph size overflow"))?;
        let mut allocation = None;
        for page in &mut self.pages {
            if let Some(rectangle) = page.allocate(padded) {
                allocation = Some((page.id, page.texture.clone(), rectangle));
                page.touched = self.clock;
                break;
            }
        }
        let (id, texture, rectangle) = if let Some(allocation) = allocation {
            allocation
        } else {
            if self.pages.len() >= 4 {
                let index = self
                    .pages
                    .iter()
                    .enumerate()
                    .filter(|(_, page)| Rc::strong_count(&page.texture) == 1)
                    .min_by_key(|(_, page)| page.touched)
                    .map(|(index, _)| index)
                    .ok_or(Error::Message("glyph atlas pages are all in use"))?;
                let old = self.pages.remove(index);
                self.entries.retain(|_, entry| entry.page != old.id);
            }
            let size = Size {
                width: 512.max(padded.width),
                height: 512.max(padded.height),
            };
            // Linear atlas sampling preserves thin strokes when a logical
            // text canvas is written into compact display-sized storage.
            let texture = gpu.device.mask_texture(size, &gpu.resident, true)?;
            let shadow = gpu
                .device
                .streamed_uploads()
                .then(|| {
                    krkr_protocol::pixels::Bytes::zeroed(
                        size.rgba_bytes().unwrap() / 4,
                        &gpu.staging,
                    )
                })
                .transpose()?;
            let mut page = Page {
                id: self.clock,
                texture: texture.clone(),
                layout: PageLayout::default(),
                touched: self.clock,
                shadow,
                dirty: None,
            };
            let rectangle = page.allocate(padded).expect("new glyph page");
            self.pages.push(page);
            (self.clock, texture, rectangle)
        };
        let page = self
            .pages
            .iter_mut()
            .find(|page| page.id == id)
            .expect("glyph page");
        let padded_rectangle = rectangle;
        let rectangle = Rect {
            left: rectangle.left + 1,
            top: rectangle.top + 1,
            ..glyph.size.rect()
        };
        if let Some(shadow) = &mut page.shadow {
            let stride = texture.size.width as usize;
            for (row, input) in glyph
                .mask
                .as_slice()
                .chunks_exact(glyph.size.width as usize)
                .enumerate()
            {
                let start = (rectangle.top as usize + row) * stride + rectangle.left as usize;
                shadow.as_mut_slice()[start..start + input.len()].copy_from_slice(input);
            }
            page.dirty = Some(if let Some(old) = page.dirty {
                union(old, padded_rectangle)
            } else {
                padded_rectangle
            });
        } else {
            let mut pixels = krkr_protocol::pixels::Bytes::zeroed(
                padded
                    .rgba_bytes()
                    .ok_or(Error::Message("glyph size overflow"))?
                    / 4,
                &gpu.staging,
            )?;
            for (row, data) in glyph
                .mask
                .as_slice()
                .chunks_exact(glyph.size.width as usize)
                .enumerate()
            {
                let at = (row + 1) * padded.width as usize + 1;
                pixels.as_mut_slice()[at..at + data.len()].copy_from_slice(data);
            }
            gpu.device
                .upload_owned_region(&texture, padded_rectangle, pixels)?;
        }
        self.entries.insert(
            glyph.id,
            Entry {
                page: id,
                rectangle,
                _permit: metadata,
            },
        );
        Ok(Slot { texture, rectangle })
    }
    fn flush(&mut self, gpu: &Gpu) -> Result<()> {
        for page in &mut self.pages {
            let Some(rectangle) = page.dirty else {
                continue;
            };
            let texture_size = page.texture.size;
            let full_bytes = texture_size.rgba_bytes().unwrap() / 4;
            let dirty_bytes = (rectangle.width as usize)
                .checked_mul(rectangle.height as usize)
                .ok_or(Error::Message("glyph upload rectangle overflow"))?;
            if dirty_bytes.saturating_mul(2) >= full_bytes {
                // A sparse union can cover most of a page. In that case the
                // old full replacement is cheaper and avoids packing rows.
                gpu.device
                    .upload(&page.texture, page.shadow.as_ref().unwrap().as_slice())?;
            } else {
                // PVR may retain a TexSubImage source after the call returns;
                // pass ownership of the packed rows to the transfer queue.
                // Keep the persistent page shadow available for later glyphs.
                let stride = texture_size.width as usize;
                let row_bytes = rectangle.width as usize;
                let mut packed = krkr_protocol::pixels::Bytes::zeroed(dirty_bytes, &gpu.staging)?;
                for row in 0..rectangle.height as usize {
                    let source = (rectangle.top as usize + row) * stride + rectangle.left as usize;
                    let destination = row * row_bytes;
                    packed.as_mut_slice()[destination..destination + row_bytes].copy_from_slice(
                        &page.shadow.as_ref().unwrap().as_slice()[source..source + row_bytes],
                    );
                }
                gpu.device
                    .upload_owned_region(&page.texture, rectangle, packed)?;
            }
            page.dirty = None;
        }
        Ok(())
    }
}
struct GlyphDraw {
    slot: Slot,
    area: Rect,
    draw: Draw,
}
impl GlyphDraw {
    fn same_batch(&self, other: &Self) -> bool {
        self.slot.texture.name() == other.slot.texture.name()
            && self.draw.color == other.draw.color
            && self.draw.operation == other.draw.operation
            && self.draw.mapping[..2] == other.draw.mapping[..2]
            && self.draw.mapping[3..5] == other.draw.mapping[3..5]
    }
}
impl Gpu {
    /// Atlas growth and destination storage for the visible masks only.
    /// Run masks already include shadows and decorations from font layout.
    pub fn text_write_bytes(&self, image: &Image, run: &Run, style: Style, clip: Rect) -> usize {
        if style.opacity == 0 {
            return 0;
        }
        let Some(clip) = clip.intersection(image.size.rect()) else {
            return 0;
        };
        let Some(bounds) = run
            .glyphs
            .iter()
            .filter_map(|placed| glyph_area(placed).intersection(clip))
            .reduce(union)
        else {
            return 0;
        };
        let text_image = image.text_view();
        let image = &text_image;
        if let Some(color) = self.text_tile_background(image, run, style) {
            return self.text_tile_write_bytes(image, run, clip, color);
        }
        if image.canvas
            && self.canvas_limit.is_some()
            && let Some(plane) = image.main.as_ref()
            && plane.size == self.fill_main_size(image)
            && let Ok(raster) = crate::scene::raster::Raster::new(image.size, plane.size, (0, 0))
            && let Some(area) = raster.rect(bounds)
            && let Some(bytes) = self.solid_region_write_bytes(image, area)
        {
            return bytes.saturating_add(self.atlas.borrow().allocation_bytes(run, clip));
        }
        self.canvas_blend_write_bytes(image, bounds, false)
            .saturating_add(self.atlas.borrow().allocation_bytes(run, clip))
    }
    pub fn draw_text(&self, image: &mut Image, run: &Run, style: Style, clip: Rect) -> Result<()> {
        let _profile = krkr_protocol::profile::span("gpu.text");
        self.check_image(image)?;
        image.plane(false)?;
        if style.opacity == 0 || run.glyphs.is_empty() {
            return Ok(());
        }
        if matches!(style.face, DrawFace::Mask | DrawFace::Province)
            || (style.opacity < 0 && style.face != DrawFace::Alpha)
            || !(-255..=255).contains(&style.opacity)
        {
            return Err(Error::Message("invalid text draw face or opacity"));
        }
        let Some(clip) = clip.intersection(image.size.rect()) else {
            return Ok(());
        };
        if let Some(color) = self.text_tile_background(image, run, style) {
            image.text = true;
            return self.draw_text_tile(image, run, clip, color);
        }
        // Typewriter updates usually contain one glyph and its shadow.
        // Keep those records on the stack; longer runs retain budgeted storage.
        let heap_glyphs = if run.glyphs.len() > 2 {
            run.glyphs.len()
        } else {
            0
        };
        let metadata = self.staging.reserve(
            heap_glyphs
                .checked_mul(std::mem::size_of::<GlyphDraw>())
                .ok_or(Error::Message("text batch byte size overflow"))?,
        )?;
        let mut draws = smallvec::SmallVec::<[GlyphDraw; 2]>::with_capacity(run.glyphs.len());
        let mut bounds = None;
        let mut maximum = 0;
        let mut atlas = self.atlas.borrow_mut();
        for placed in &run.glyphs {
            let glyph = &placed.glyph;
            let x = placed.x.saturating_add(glyph.origin[0]);
            let y = placed.y.saturating_add(glyph.origin[1]);
            let Some(area) = glyph_area(placed).intersection(clip) else {
                continue;
            };
            if !matches!(glyph.levels, 65 | 256)
                || glyph.size.rgba_bytes().map(|bytes| bytes / 4)
                    != Some(glyph.mask.as_slice().len())
            {
                return Err(Error::Message("invalid glyph mask"));
            }
            let slot = atlas.resolve(self, glyph)?;
            maximum = maximum.max(
                Size {
                    width: area.width.min(self.tile_edge),
                    height: area.height.min(self.tile_edge),
                }
                .rgba_bytes()
                .unwrap(),
            );
            bounds = Some(if let Some(old) = bounds {
                union(old, area)
            } else {
                area
            });
            let mut color = rgba(placed.color);
            color[3] = f32::from(glyph.levels);
            let draw = Draw {
                kind: 4.,
                color,
                operation: [
                    0.,
                    face(style.face),
                    f32::from(style.opacity),
                    f32::from(style.hold_alpha),
                ],
                ..Draw::copy(
                    [
                        1.,
                        0.,
                        (slot.rectangle.left - x) as f32,
                        0.,
                        1.,
                        (slot.rectangle.top - y) as f32,
                    ],
                    [true; 4],
                )
            };
            draws.push(GlyphDraw { slot, area, draw });
        }
        let Some(bounds) = bounds else {
            return Ok(());
        };
        atlas.flush(self)?;
        // All masks and the largest per-draw backdrop are admitted before
        // changing the first pixel. Prepared draws pin their atlas pages.
        if !self.device.streamed_uploads() {
            if self.scratch.available() < maximum {
                self.collect()?;
            }
            drop(self.scratch.reserve(maximum)?);
        }
        image.text = true;
        self.writable_compact(image, bounds, false)?;
        let target = image.plane(false)?;
        for glyph in &mut draws {
            if let Some((area, draw)) =
                self.raster_draw(image.size, target.size, glyph.area, &glyph.draw)?
            {
                glyph.area = area;
                glyph.draw = draw;
            } else {
                glyph.area = Rect::default();
            }
        }
        draws.retain(|glyph| glyph.area.width != 0 && glyph.area.height != 0);
        if draws.is_empty() {
            return Ok(());
        }
        if target
            .tiles
            .iter()
            .all(|tile| self.device.supports_work_draw(&tile.texture))
        {
            return self.draw_text_work(target, &draws);
        }
        for glyph in draws {
            let source = Plane {
                size: glyph.slot.texture.size,
                budget: self.resident.clone(),
                tiles: vec![Tile {
                    backing: None,
                    rectangle: glyph.slot.texture.size.rect(),
                    texture: glyph.slot.texture,
                }],
            };
            self.draw(target, Some(&source), glyph.area, &glyph.draw)?;
        }
        drop(metadata);
        Ok(())
    }
    fn draw_text_work(&self, target: &Plane, draws: &[GlyphDraw]) -> Result<()> {
        // A character and its shadow usually differ in color and overlap.
        // Packing each into a one-glyph vertex buffer saves no draw calls.
        // Use the persistent quad unless adjacent masks can actually share a
        // draw. Tile clipping may still split an otherwise useful batch.
        let batched = draws.windows(2).any(|pair| {
            pair[0].same_batch(&pair[1]) && pair[0].area.intersection(pair[1].area).is_none()
        });
        // One character with its overlapping shadow is the usual typewriter
        // update. Fuse the two exact alpha blends into one pass so the second
        // mask does not force a work-surface store and reload.
        let paired = draws.len() == 2
            && target.tiles.len() == 1
            && draws[0].area.intersection(draws[1].area).is_some()
            && draws[0].draw.operation == draws[1].draw.operation
            && draws[0].draw.operation == [0., face(DrawFace::Alpha), 255., 0.]
            && draws[0].draw.color[3] == draws[1].draw.color[3]
            && matches!(draws[0].draw.color[3], 65. | 256.);
        let program = if paired {
            let mut slot = self.glyph_pair_program.borrow_mut();
            if slot.is_none() {
                *slot = Some(Rc::new(crate::shader::Program::new(
                    self.device.clone(),
                    include_str!("quad.vert"),
                    include_str!("glyph_pair.frag"),
                )?));
            }
            slot.as_ref().unwrap().clone()
        } else if batched {
            let mut slot = self.glyph_batch_program.borrow_mut();
            if slot.is_none() {
                *slot = Some(Rc::new(crate::shader::Program::new(
                    self.device.clone(),
                    include_str!("glyph_batch.vert"),
                    &crate::draw_source::glyph_batch(),
                )?));
            }
            slot.as_ref().unwrap().clone()
        } else {
            self.program.select(&draws[0].draw)?
        };
        for tile in &target.tiles {
            let area = draws
                .iter()
                .filter_map(|glyph| glyph.area.intersection(tile.rectangle))
                .reduce(union);
            let Some(area) = area else { continue };
            let local = |part: Rect| Rect {
                left: part.left - tile.rectangle.left,
                top: part.top - tile.rectangle.top,
                ..part
            };
            let framebuffer = self
                .device
                .prepare_work_draw(&tile.texture, local(area))?
                .ok_or(Error::Message("text batch requires a work surface"))?;
            // A fresh fill/copy can still cover the whole run in the work
            // surface. Publish that backdrop once before drawing any masks;
            // otherwise each disjoint glyph forces its own transfer/barrier.
            // Only pending pixels inside the run's bounds are transferred.
            self.device.backdrop(&tile.texture, local(area))?;
            // One load/setup per tile, while overlapping masks retain their
            // original order and exact glyph shader arithmetic. Preparing the
            // bounds did not mark gaps or upcoming glyphs as pending writes.
            program.bind();
            unsafe {
                let gl = &self.device.gl;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                gl.viewport(
                    0,
                    0,
                    tile.rectangle.width as i32,
                    tile.rectangle.height as i32,
                );
                gl.disable(glow::SCISSOR_TEST);
                gl.disable(glow::BLEND);
                gl.color_mask(true, true, true, true);
                gl.active_texture(glow::TEXTURE1);
                gl.bind_texture(glow::TEXTURE_2D, Some(tile.texture.name()));
            }
            program.four("u_target", rect(tile.rectangle));
            program.one("u_flip", 1.);
            program.four("u_operation", draws[0].draw.operation);
            program.two(
                "u_backdrop_origin",
                tile.rectangle.left as f32,
                tile.rectangle.top as f32,
            );
            program.two(
                "u_backdrop_size",
                tile.rectangle.width as f32,
                tile.rectangle.height as f32,
            );
            program.two("u_source_origin", 0., 0.);
            self.bind_texture(2, &self.lookup)?;
            if paired {
                self.draw_text_pair(tile, draws, area, &program)?;
                continue;
            }
            if batched {
                self.draw_glyph_batches(tile, draws, &program)?;
                continue;
            }
            let mut atlas = None;
            for glyph in draws {
                let Some(part) = glyph.area.intersection(tile.rectangle) else {
                    continue;
                };
                // Only a real overlap needs to publish previous writes to the
                // backing texture. Work transfers preserve this pass's state.
                self.device.backdrop(&tile.texture, local(part))?;
                self.device.work_draw_region(&tile.texture, local(part))?;
                program.four("u_rectangle", rect(part));
                program.four("u_color", glyph.draw.color);
                program.three("u_map_x", glyph.draw.mapping[..3].try_into().unwrap());
                program.three("u_map_y", glyph.draw.mapping[3..].try_into().unwrap());
                let source = &glyph.slot.texture;
                if atlas != Some(source.name()) {
                    self.bind_texture(0, source)?;
                    program.two(
                        "u_source_size",
                        source.size.width as f32,
                        source.size.height as f32,
                    );
                    atlas = Some(source.name());
                }
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
            }
            self.device.check()?;
        }
        Ok(())
    }

    fn draw_text_pair(
        &self,
        tile: &Tile,
        draws: &[GlyphDraw],
        area: Rect,
        program: &crate::shader::Program,
    ) -> Result<()> {
        let local = Rect {
            left: area.left - tile.rectangle.left,
            top: area.top - tile.rectangle.top,
            ..area
        };
        self.device.work_draw_region(&tile.texture, local)?;
        program.four("u_rectangle", rect(area));
        for (index, glyph) in draws.iter().enumerate() {
            let (unit, color, map_x, map_y, size, bounds) = if index == 0 {
                (
                    0,
                    "u_color",
                    "u_map_x",
                    "u_map_y",
                    "u_source_size",
                    "u_area0",
                )
            } else {
                (
                    3,
                    "u_color2",
                    "u_map_x2",
                    "u_map_y2",
                    "u_source_size2",
                    "u_area1",
                )
            };
            self.bind_texture(unit, &glyph.slot.texture)?;
            program.four(color, glyph.draw.color);
            program.three(map_x, glyph.draw.mapping[..3].try_into().unwrap());
            program.three(map_y, glyph.draw.mapping[3..].try_into().unwrap());
            program.two(
                size,
                glyph.slot.texture.size.width as f32,
                glyph.slot.texture.size.height as f32,
            );
            let r = glyph.area;
            program.four(
                bounds,
                [
                    r.left as f32,
                    r.top as f32,
                    (i64::from(r.left) + i64::from(r.width)) as f32,
                    (i64::from(r.top) + i64::from(r.height)) as f32,
                ],
            );
        }
        unsafe {
            self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        self.device.check()
    }

    fn draw_glyph_batches(
        &self,
        tile: &Tile,
        draws: &[GlyphDraw],
        program: &crate::shader::Program,
    ) -> Result<()> {
        // Bound CPU overlap checks and temporary vertex storage, independently
        // of paragraph length. GPU buffers retain their staging permits until
        // completion, just like rectangle/mesh batches.
        const GLYPHS: usize = 64;
        let mut vertices = [[0f32; 4]; GLYPHS * 6];
        let mut areas = [Rect::default(); GLYPHS];
        let mut at = 0;
        let local = |r: Rect| Rect {
            left: r.left - tile.rectangle.left,
            top: r.top - tile.rectangle.top,
            ..r
        };
        while at < draws.len() {
            let first = &draws[at];
            if first.area.intersection(tile.rectangle).is_none() {
                at += 1;
                continue;
            }
            let mut count = 0;
            let mut bounds = None;
            while at < draws.len() && count < GLYPHS {
                let glyph = &draws[at];
                let Some(part) = glyph.area.intersection(tile.rectangle) else {
                    at += 1;
                    continue;
                };
                let b = &glyph.draw;
                if count > 0
                    && (!first.same_batch(glyph)
                        || areas[..count]
                            .iter()
                            .any(|r| r.intersection(part).is_some()))
                {
                    break;
                }
                // No two masks in a draw sample pixels written by that draw.
                // An overlap ends the batch, so shadows, ruby and negative
                // advances retain script order and the exact blend formula.
                bounds = Some(crate::scene_damage::union(bounds, part));
                areas[count] = part;
                let [x, y, w, h] = rect(part);
                for (i, [px, py]) in [
                    [x, y],
                    [x + w, y],
                    [x, y + h],
                    [x, y + h],
                    [x + w, y],
                    [x + w, y + h],
                ]
                .into_iter()
                .enumerate()
                {
                    vertices[count * 6 + i] = [px, py, b.mapping[2], b.mapping[5]];
                }
                count += 1;
                at += 1;
            }
            let Some(bounds) = bounds else { continue };
            let buffer = self.device.buffer(
                glow::ARRAY_BUFFER,
                bytemuck::cast_slice(&vertices[..count * 6]),
                &self.staging,
            )?;
            self.device.backdrop(&tile.texture, local(bounds))?;
            for &area in &areas[..count] {
                self.device.work_draw_region(&tile.texture, local(area))?;
            }
            program.four("u_color", first.draw.color);
            program.three(
                "u_map_x",
                [first.draw.mapping[0], first.draw.mapping[1], 0.],
            );
            program.three(
                "u_map_y",
                [first.draw.mapping[3], first.draw.mapping[4], 0.],
            );
            let source = &first.slot.texture;
            self.bind_texture(0, source)?;
            program.two(
                "u_source_size",
                source.size.width as f32,
                source.size.height as f32,
            );
            self.device.draw_state.invalidate_vertices();
            unsafe {
                let gl = &self.device.gl;
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer.name));
                gl.enable_vertex_attrib_array(1);
                gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
                gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
                gl.draw_arrays(glow::TRIANGLES, 0, (count * 6) as i32);
                gl.disable_vertex_attrib_array(1);
            }
            self.device.check()?;
        }
        Ok(())
    }
}
fn padded_size(glyph: &Glyph) -> Option<Size> {
    Some(Size {
        width: glyph.size.width.checked_add(2)?,
        height: glyph.size.height.checked_add(2)?,
    })
}
fn glyph_area(placed: &krkr_protocol::text::PlacedGlyph) -> Rect {
    Rect {
        left: placed.x.saturating_add(placed.glyph.origin[0]),
        top: placed.y.saturating_add(placed.glyph.origin[1]),
        ..placed.glyph.size.rect()
    }
}
fn union(a: Rect, b: Rect) -> Rect {
    let left = a.left.min(b.left);
    let top = a.top.min(b.top);
    let right =
        (i64::from(a.left) + i64::from(a.width)).max(i64::from(b.left) + i64::from(b.width));
    let bottom =
        (i64::from(a.top) + i64::from(a.height)).max(i64::from(b.top) + i64::from(b.height));
    Rect {
        left,
        top,
        width: (right - i64::from(left)) as u32,
        height: (bottom - i64::from(top)) as u32,
    }
}

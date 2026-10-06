//! Keep equal untouched canvas margins shared across independently drawn layers.
//! Constant tiles use one texel until a write expands them. Aligned tile rows
//! and columns retain exact coordinates for sampling across their boundaries.
use crate::{
    Gpu, Image, Result,
    device::Texture,
    image::{Plane, Tile},
};
use glow::HasContext;
use krkr_protocol::graphics::{Rect, Size};
use std::{
    collections::HashMap,
    rc::{Rc, Weak},
};

#[derive(Default)]
pub(crate) struct Solids(Vec<Weak<Texture>>);

// Tiny constant border fragments cost metadata, not pixel allocations. Leave
// room for another halo expansion after pressure compaction, instead of
// materializing megabytes of empty margins when a second blur touches them.
pub(crate) const MAX_WRITE_REGIONS: usize = 256;

fn solid_fill_part(
    tile: &Tile,
    logical: Size,
    raster: crate::scene::raster::Raster,
    area: Rect,
    fill: &krkr_protocol::graphics::Fill,
) -> Option<Rect> {
    let part = area.intersection(tile.rectangle)?;
    (!crate::fills::unchanged(tile, logical, raster, fill)
        && tile.texture.size != tile.size()
        && tile.texture.solid_color().is_some())
    .then_some(part)
}

struct Partition {
    color: u32,
    area: Rect,
    xs: Vec<u32>,
    ys: Vec<u32>,
}

impl Solids {
    pub(crate) fn trim(&mut self) {
        self.0.retain(|t| t.strong_count() != 0);
    }
}

impl Gpu {
    /// Long sequences of band writes can retain many old full-sized backings.
    /// Repack the stored pixels before those views consume the whole pool.
    /// This changes neither the logical dimensions nor the sampling density.
    pub fn compact_fragmented_canvas(&self, image: &mut Image) -> Result<bool> {
        if !image.canvas || !self.device.streamed_uploads() {
            return Ok(false);
        }
        let Some(old) = &image.main else {
            return Ok(false);
        };
        if old.tiles.len() < 8 || !old.tiles.iter().any(|tile| tile.backing.is_some()) {
            return Ok(false);
        }
        let Some(area) = old
            .tiles
            .iter()
            .filter(|tile| tile.solid_region(tile.size().rect()).is_none())
            .map(|tile| tile.rectangle)
            .reduce(|a, b| crate::scene_damage::union(Some(a), b))
        else {
            return Ok(false);
        };
        let dense = area.width as usize * area.height as usize * 4;
        let mut textures = std::collections::HashSet::new();
        let retained: usize = old
            .tiles
            .iter()
            .filter(|tile| textures.insert(Rc::as_ptr(&tile.texture)))
            .map(|tile| tile.texture.allocation_bytes())
            .sum();
        if retained <= dense.saturating_mul(2)
            || retained.saturating_sub(dense) < 1024 * 1024
            || dense.saturating_add(old.tiles.len() * 4) > old.budget.available()
        {
            return Ok(false);
        }
        let _profile = krkr_protocol::profile::span_detail("gpu.compact_views", || {
            format!(
                "retained={retained} packed={dense} tiles={}",
                old.tiles.len()
            )
        });
        let mut next = self.overwrite_plane(
            Size {
                width: area.width,
                height: area.height,
            },
            &old.budget,
        )?;
        let output = Rc::get_mut(&mut next).unwrap();
        output.size = old.size;
        for tile in &mut output.tiles {
            tile.rectangle.left += area.left;
            tile.rectangle.top += area.top;
        }
        self.draw(
            &next,
            Some(old),
            area,
            &crate::drawing::Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
        )?;
        let output = Rc::get_mut(&mut next).unwrap();
        for tile in &old.tiles {
            let Some(color) = tile.solid_region(tile.size().rect()) else {
                continue;
            };
            let regions = tile.rectangle.intersection(area).map_or(
                [
                    tile.rectangle,
                    Rect::default(),
                    Rect::default(),
                    Rect::default(),
                ],
                |inside| outside(tile.rectangle, inside),
            );
            for rectangle in regions
                .into_iter()
                .filter(|r| r.width != 0 && r.height != 0)
            {
                output.tiles.push(Tile {
                    rectangle,
                    backing: None,
                    texture: self.canvas_solid_tile(
                        Size {
                            width: 1,
                            height: 1,
                        },
                        color,
                        true,
                    )?,
                });
            }
        }
        image.main = Some(next);
        Ok(true)
    }

    fn full_copy_area(&self, target: &Image, source: &Image, src: Rect, dst: Rect) -> Option<Rect> {
        if !target.canvas || !self.device.streamed_uploads() || dst != target.size.rect() {
            return None;
        }
        let old = target.main.as_ref()?;
        let input = source.main.as_ref()?;
        let size = self.fill_main_size(target);
        if input.tiles.len() > 24
            || input
                .tiles
                .iter()
                .any(|tile| !tile.texture.belongs_to(&old.budget))
            || u64::from(input.size.width) * u64::from(target.size.width)
                != u64::from(size.width) * u64::from(source.size.width)
            || u64::from(input.size.height) * u64::from(target.size.height)
                != u64::from(size.height) * u64::from(source.size.height)
        {
            return None;
        }
        let x = i64::from(src.left) * i64::from(input.size.width);
        let y = i64::from(src.top) * i64::from(input.size.height);
        if source.size.width == 0
            || source.size.height == 0
            || x % i64::from(source.size.width) != 0
            || y % i64::from(source.size.height) != 0
        {
            return None;
        }
        let area = Rect {
            left: i32::try_from(x / i64::from(source.size.width)).ok()?,
            top: i32::try_from(y / i64::from(source.size.height)).ok()?,
            ..size.rect()
        };
        (area.intersection(input.size.rect()) == Some(area)).then_some(area)
    }

    /// Whether an overwrite can retain source tiles without pixel storage.
    /// `src` and `dst` must already be clipped by blit::region.
    pub fn copy_is_view(&self, target: &Image, source: &Image, src: Rect, dst: Rect) -> bool {
        let target = if source.text {
            target.text_view()
        } else {
            std::borrow::Cow::Borrowed(target)
        };
        self.full_copy_area(&target, source, src, dst).is_some()
            || self
                .inset_copy_plane(&target, source, src, dst)
                .is_ok_and(|p| p.is_some())
    }

    pub(crate) fn share_full_copy(
        &self,
        target: &mut Image,
        source: &Image,
        src: Rect,
        dst: Rect,
    ) -> bool {
        let Some(area) = self.full_copy_area(target, source, src, dst) else {
            return false;
        };
        let old = target.main.as_ref().unwrap();
        let input = source.main.as_ref().unwrap();
        let tiles = input
            .tiles
            .iter()
            .filter_map(|tile| {
                let overlap = tile.rectangle.intersection(area)?;
                let mut tile = tile.cropped(overlap);
                tile.rectangle.left -= area.left;
                tile.rectangle.top -= area.top;
                if let Some(backing) = &mut tile.backing {
                    backing.left -= area.left;
                    backing.top -= area.top;
                }
                Some(tile)
            })
            .collect();
        // Preserve target policy/province. Writes to either side subsequently
        // detach their affected tiles; this snapshot owns no new pixel storage.
        target.main = Some(Rc::new(Plane {
            size: Size {
                width: area.width,
                height: area.height,
            },
            budget: old.budget.clone(),
            tiles,
        }));
        true
    }

    /// Small writes to a shared page need ownership of a row band, not a
    /// second full-screen texture. Keep untouched rows as immutable views.
    pub(crate) fn detach_canvas_bands(&self, image: &mut Image, area: Rect) -> Result<()> {
        if !self.device.streamed_uploads() {
            return Ok(());
        }
        let old = image.plane(false)?;
        let parts = canvas_band_layout(old, area, false);
        if parts.is_empty() {
            return Ok(());
        }
        let mut tiles = Vec::with_capacity(old.tiles.len() + parts.len() * 2);
        for (index, tile) in old.tiles.iter().enumerate() {
            let Some((_, band)) = parts.iter().find(|(i, _)| *i == index) else {
                tiles.push(tile.clone());
                continue;
            };
            let source = tile.cropped(*band);
            let texture = self.device.sample_texture(source.size(), &old.budget)?;
            self.copy_tile_storage(&source, &texture, &old.budget)?;
            tiles.push(Tile {
                rectangle: *band,
                backing: None,
                texture,
            });
            for rectangle in outside(tile.rectangle, *band)
                .into_iter()
                .filter(|r| r.width != 0 && r.height != 0)
            {
                tiles.push(tile.cropped(rectangle));
            }
        }
        // Preserve every alias and publish only fully initialized storage.
        image.main = Some(Rc::new(Plane {
            size: old.size,
            budget: old.budget.clone(),
            tiles,
        }));
        Ok(())
    }

    /// Admission for draws that preserve destination pixels. Match the band
    /// layout before evicting assets to make room for copy-on-write.
    pub fn canvas_blend_write_bytes(&self, image: &Image, bounds: Rect, snapshot: bool) -> usize {
        let fallback = || self.canvas_region_write_bytes(image, bounds, false, snapshot);
        if !image.canvas || self.canvas_limit.is_none() || !self.device.streamed_uploads() {
            return fallback();
        }
        let Some(old) = image.main.as_ref() else {
            return fallback();
        };
        if old.size != self.fill_main_size(image) {
            return fallback();
        }
        let Some(area) = bounds.intersection(image.size.rect()).and_then(|r| {
            crate::scene::raster::Raster::new(image.size, old.size, (0, 0))
                .ok()?
                .rect(r)
        }) else {
            return 0;
        };
        // Solid-region writes run before band detachment and retain their own
        // layout. The ordinary estimate remains conservative for that path.
        if solid_write_layout(old, area, false).is_some() {
            return fallback();
        }
        let parts = canvas_band_layout(old, area, snapshot);
        old.tiles
            .iter()
            .enumerate()
            .filter(|(_, tile)| {
                tile.rectangle.intersection(area).is_some()
                    && (!tile.renderable()
                        || snapshot
                        || Rc::strong_count(old) > 1
                        || Rc::strong_count(&tile.texture) > 1)
            })
            .fold(0usize, |sum, (index, tile)| {
                let area = parts
                    .iter()
                    .find(|(i, _)| *i == index)
                    .map_or(tile.rectangle, |(_, area)| *area);
                sum.saturating_add(area.width as usize * area.height as usize * 4)
            })
    }

    pub(crate) fn share_inset_copy(
        &self,
        target: &mut Image,
        source: &Image,
        src: Rect,
        dst: Rect,
    ) -> Result<bool> {
        let Some(plane) = self.inset_copy_plane(target, source, src, dst)? else {
            return Ok(false);
        };
        target.main = Some(plane);
        Ok(true)
    }

    fn inset_copy_plane(
        &self,
        target: &Image,
        source: &Image,
        src: Rect,
        dst: Rect,
    ) -> Result<Option<Rc<Plane>>> {
        if !target.canvas || !self.device.streamed_uploads() {
            return Ok(None);
        }
        let previous = target.plane(false)?;
        let desired = self.fill_main_size(target);
        // A cleared canvas is a one-texel constant. Expand its coordinate
        // view, not its pixel storage, before inserting the moving sprite.
        let expanded;
        let old = if previous.size != desired
            && previous.tiles.len() == 1
            && previous.tiles[0].texture.solid_color().is_some()
        {
            expanded = Plane {
                size: desired,
                budget: previous.budget.clone(),
                tiles: vec![Tile {
                    rectangle: desired.rect(),
                    backing: None,
                    texture: previous.tiles[0].texture.clone(),
                }],
            };
            &expanded
        } else {
            previous.as_ref()
        };
        let input = source.plane(false)?;
        if old.size != desired
            // Different densities use alpha-weighted linear resampling in
            // copy_rect; a tile view must preserve that operation as well.
            || u64::from(input.size.width) * u64::from(target.size.width)
                != u64::from(old.size.width) * u64::from(source.size.width)
            || u64::from(input.size.height) * u64::from(target.size.height)
                != u64::from(old.size.height) * u64::from(source.size.height)
            // Script canvases must not pin a temporary scene capture in the
            // scratch budget. Cross-budget copies use resident storage.
            || input.tiles.iter().any(|t| !t.texture.belongs_to(&old.budget))
            || input.tiles.len() > 24
            || input
                .tiles
                .iter()
                .any(|t| t.rectangle.intersection(input.size.rect()) != Some(t.rectangle))
        {
            return Ok(None);
        }
        let Some(area) =
            crate::scene::raster::Raster::new(target.size, old.size, (0, 0))?.rect(dst)
        else {
            return Ok(None);
        };
        let Some(input_area) =
            crate::scene::raster::Raster::new(source.size, input.size, (0, 0))?.rect(src)
        else {
            return Ok(None);
        };
        if area.width != input_area.width || area.height != input_area.height {
            return Ok(None);
        }
        let axis = |logical: u32,
                    stored: u32,
                    out_logical: u32,
                    out_stored: u32,
                    offset: i32,
                    physical: i32,
                    input_start: i32,
                    length: u32| {
            let ratio = stored as f32 / logical as f32;
            let scale = out_logical as f32 / out_stored as f32;
            let a = ratio * scale;
            let t = -(offset as f32) * ratio + (ratio - 1.) * 0.5 + ratio * (scale - 1.) * 0.5;
            let start = f64::from(physical);
            let end = start + f64::from(length - 1);
            let error = 16. * f64::from(f32::EPSILON) * (end + t.abs() as f64 + 1.);
            [start, end].into_iter().all(|p| {
                ((f64::from(a) - 1.) * p + f64::from(t) + start - f64::from(input_start)).abs()
                    + error
                    < 0.5
            })
        };
        if !axis(
            source.size.width,
            input.size.width,
            target.size.width,
            old.size.width,
            dst.left - src.left,
            area.left,
            input_area.left,
            area.width,
        ) || !axis(
            source.size.height,
            input.size.height,
            target.size.height,
            old.size.height,
            dst.top - src.top,
            area.top,
            input_area.top,
            area.height,
        ) {
            return Ok(None);
        }
        // Keep every outside pixel, even when the pending border clear has
        // not reached the right/bottom edges yet. All tiles remain read-only
        // views; no synchronous command is deferred to make this possible.
        // Compressed textures are valid sampled views as well. Their tiles
        // detach to writable storage before a later fill or filter; copying
        // a sprite must not expand BC merely to make it framebuffer-ready.
        let mut shifted: Vec<_> = input
            .tiles
            .iter()
            .filter_map(|tile| {
                let rectangle = tile.rectangle.intersection(input_area)?;
                Some(if tile.texture.solid_color().is_some() {
                    Tile {
                        rectangle,
                        backing: None,
                        texture: tile.texture.clone(),
                    }
                } else if rectangle == tile.rectangle {
                    tile.clone()
                } else {
                    tile.cropped(rectangle)
                })
            })
            .collect();
        let dx = area.left - input_area.left;
        let dy = area.top - input_area.top;
        for tile in &mut shifted {
            tile.rectangle.left += dx;
            tile.rectangle.top += dy;
            if let Some(b) = &mut tile.backing {
                b.left += dx;
                b.top += dy;
            }
        }
        let mut xs = vec![
            0,
            area.left as u32,
            area.left as u32 + area.width,
            old.size.width,
        ];
        let mut ys = vec![
            0,
            area.top as u32,
            area.top as u32 + area.height,
            old.size.height,
        ];
        for t in old.tiles.iter().chain(&shifted) {
            xs.extend([
                t.rectangle.left as u32,
                t.rectangle.left as u32 + t.rectangle.width,
            ]);
            ys.extend([
                t.rectangle.top as u32,
                t.rectangle.top as u32 + t.rectangle.height,
            ]);
        }
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        if (xs.len() - 1) * (ys.len() - 1) > 128 {
            return Ok(None);
        }
        let mut tiles = Vec::with_capacity((xs.len() - 1) * (ys.len() - 1));
        for y in ys.windows(2) {
            for x in xs.windows(2) {
                let rectangle = Rect {
                    left: x[0] as i32,
                    top: y[0] as i32,
                    width: x[1] - x[0],
                    height: y[1] - y[0],
                };
                let candidates = if rectangle.intersection(area).is_some() {
                    &shifted
                } else {
                    &old.tiles
                };
                let Some(tile) = candidates
                    .iter()
                    .find(|t| t.rectangle.intersection(rectangle) == Some(rectangle))
                else {
                    return Ok(None);
                };
                if rectangle == tile.rectangle {
                    tiles.push(tile.clone());
                } else if tile.texture.solid_color().is_some() {
                    tiles.push(Tile {
                        rectangle,
                        backing: None,
                        texture: tile.texture.clone(),
                    });
                } else {
                    tiles.push(tile.cropped(rectangle));
                }
            }
        }
        Ok(Some(Rc::new(Plane {
            size: old.size,
            budget: old.budget.clone(),
            tiles,
        })))
    }
    pub(crate) fn canvas_is_fragmented(&self, image: &Image) -> bool {
        image.canvas
            && image.main.as_ref().is_some_and(|plane| {
                let regular_tiles = plane.size.width.div_ceil(self.tile_edge) as usize
                    * plane.size.height.div_ceil(self.tile_edge) as usize;
                plane.tiles.len() > 24 && plane.tiles.len() > regular_tiles * 4
            })
    }

    pub(crate) fn crop_fill_area(
        &self,
        image: &Image,
        fill: &krkr_protocol::graphics::Fill,
    ) -> Option<Rect> {
        if !image.canvas
            || !self.device.streamed_uploads()
            || fill.face == krkr_protocol::graphics::DrawFace::Province
            || crate::fills::mask(fill) != [true; 4]
        {
            return None;
        }
        let plane = image.main.as_ref()?;
        if plane.size != self.fill_main_size(image) || plane.tiles.len() > MAX_WRITE_REGIONS {
            return None;
        }
        let area = crate::scene::raster::Raster::new(image.size, plane.size, (0, 0))
            .ok()?
            .rect(fill.rectangle.intersection(image.size.rect())?)?;
        if !self.canvas_is_fragmented(image)
            && !plane.tiles.iter().any(|t| {
                t.rectangle.intersection(area).is_some()
                    && t.texture.renderable()
                    && (t.backing.is_some() || t.texture.size == t.size())
                    && (Rc::strong_count(plane) > 1
                        || Rc::strong_count(&t.texture) > 1
                        || t.backing.is_some())
                    && t.size().rgba_bytes().unwrap_or(0) >= 64 * 1024
            })
        {
            return None;
        }
        // A clear replaces its whole rectangle, independent of old tile
        // boundaries. Bound the outside pieces and a regular grid of solids.
        let cleared_tiles = area.width.div_ceil(self.tile_edge) as usize
            * area.height.div_ceil(self.tile_edge) as usize;
        let count = cleared_tiles
            + plane
                .tiles
                .iter()
                .map(|tile| {
                    tile.rectangle.intersection(area).map_or(1, |part| {
                        outside(tile.rectangle, part)
                            .into_iter()
                            .filter(|r| r.width != 0 && r.height != 0)
                            .count()
                    })
                })
                .sum::<usize>();
        if count > MAX_WRITE_REGIONS {
            return None;
        }
        if plane.tiles.iter().any(|t| {
            t.rectangle.intersection(area).is_some()
                && !t.texture.renderable()
                && t.texture.solid_color().is_none()
        }) {
            return None;
        }
        Some(area)
    }
    pub(crate) fn crop_fill_bytes(
        &self,
        image: &Image,
        fill: &krkr_protocol::graphics::Fill,
    ) -> Option<usize> {
        self.crop_fill_area(image, fill).map(|_| 4)
    }
    pub(crate) fn fill_cropped_regions(
        &self,
        image: &mut Image,
        fill: &krkr_protocol::graphics::Fill,
    ) -> Result<bool> {
        let Some(area) = self.crop_fill_area(image, fill) else {
            return Ok(false);
        };
        let old = image.plane(false)?;
        let solid = self.canvas_solid_tile(
            Size {
                width: 1,
                height: 1,
            },
            fill.color,
            true,
        )?;
        let mut tiles = Vec::with_capacity(old.tiles.len() * 5);
        for tile in &old.tiles {
            let Some(part) = tile.rectangle.intersection(area) else {
                tiles.push(tile.clone());
                continue;
            };
            for rectangle in outside(tile.rectangle, part)
                .into_iter()
                .filter(|r| r.width != 0 && r.height != 0)
            {
                if tile.texture.solid_color().is_some() {
                    tiles.push(Tile {
                        rectangle,
                        backing: None,
                        texture: tile.texture.clone(),
                    });
                } else {
                    tiles.push(tile.cropped(rectangle));
                }
            }
        }
        // Do not inherit the old partition inside a cleared area. Moving
        // sprites otherwise leave hundreds of tiny tiles after a few frames.
        for top in (0..area.height).step_by(self.tile_edge as usize) {
            for left in (0..area.width).step_by(self.tile_edge as usize) {
                tiles.push(Tile {
                    rectangle: Rect {
                        left: area.left + left as i32,
                        top: area.top + top as i32,
                        width: (area.width - left).min(self.tile_edge),
                        height: (area.height - top).min(self.tile_edge),
                    },
                    backing: None,
                    texture: solid.clone(),
                });
            }
        }
        tiles.sort_by_key(|t| (t.rectangle.top, t.rectangle.left));
        image.main = Some(Rc::new(Plane {
            size: old.size,
            budget: old.budget.clone(),
            tiles,
        }));
        Ok(true)
    }
    /// Growing a canvas often adds only a blur halo. If the ordinary copy
    /// would sample every existing stored texel unchanged, retain those
    /// tiles and represent the new padding with constant texels instead.
    pub(crate) fn shared_resize_storage(&self, source: &Image, size: Size) -> Option<Size> {
        if !self.device.streamed_uploads()
            || self.canvas_limit.is_none()
            || source.has_province()
            || size.width < source.size.width
            || size.height < source.size.height
        {
            return None;
        }
        let old = source.main.as_ref()?;
        // Including two padding bands, at most 124 bounded views are produced.
        if old.tiles.len() > 24
            || old.tiles.iter().any(|t| {
                t.rectangle.intersection(old.size.rect()) != Some(t.rectangle)
                    || !t.texture.belongs_to(&self.resident)
            })
        {
            return None;
        }
        let stored = self.canvas_storage(size, Some(source));
        // A density change invokes copy_rect's filtered resample. Matching
        // nearest texel addresses alone does not preserve its output pixels.
        if !source.text
            && (u64::from(old.size.width) * u64::from(size.width)
                != u64::from(stored.width) * u64::from(source.size.width)
                || u64::from(old.size.height) * u64::from(size.height)
                    != u64::from(stored.height) * u64::from(source.size.height))
        {
            return None;
        }
        if stored.width.saturating_sub(old.size.width) > self.tile_edge
            || stored.height.saturating_sub(old.size.height) > self.tile_edge
        {
            return None;
        }
        let coverage = crate::scene::raster::Raster::new(size, stored, (0, 0))
            .ok()?
            .rect(source.size.rect())?;
        if coverage != old.size.rect() {
            return None;
        }
        let unchanged = |logical: u32, previous: u32, next: u32, physical: u32| {
            // Reproduce copy_rect + raster_draw's f32 coefficients. Bound
            // rounding at both endpoints; their affine interior cannot escape.
            let ratio = previous as f32 / logical as f32;
            let scale = next as f32 / physical as f32;
            let a = ratio * scale;
            let offset = (ratio - 1.) * 0.5 + ratio * (scale - 1.) * 0.5;
            let last = f64::from(previous - 1);
            let error = 16. * f64::from(f32::EPSILON) * (last + f64::from(offset.abs()) + 1.);
            f64::from(offset.abs()) + error < 0.5
                && ((f64::from(a) - 1.) * last + f64::from(offset)).abs() + error < 0.5
        };
        (unchanged(source.size.width, old.size.width, size.width, stored.width)
            && unchanged(
                source.size.height,
                old.size.height,
                size.height,
                stored.height,
            ))
        .then_some(stored)
    }

    pub(crate) fn grow_shared_canvas(
        &self,
        source: &Image,
        size: Size,
        stored: Size,
        color: u32,
    ) -> Result<Image> {
        let old = source.plane(false)?;
        let solid = self.canvas_solid_tile(
            Size {
                width: 1,
                height: 1,
            },
            color,
            true,
        )?;
        let mut tiles = old.tiles.clone();
        // Split padding on existing tile boundaries. Neighbour samplers keep
        // their aligned rows/columns without expanding an entire large image.
        let mut xs = vec![0, old.size.width, stored.width];
        let mut ys = vec![0, old.size.height];
        for tile in &old.tiles {
            xs.extend([
                tile.rectangle.left as u32,
                tile.rectangle.left as u32 + tile.rectangle.width,
            ]);
            ys.extend([
                tile.rectangle.top as u32,
                tile.rectangle.top as u32 + tile.rectangle.height,
            ]);
        }
        xs.sort_unstable();
        xs.dedup();
        ys.sort_unstable();
        ys.dedup();
        if stored.width > old.size.width {
            for y in ys.windows(2) {
                tiles.push(Tile {
                    backing: None,
                    rectangle: Rect {
                        left: old.size.width as i32,
                        top: y[0] as i32,
                        width: stored.width - old.size.width,
                        height: y[1] - y[0],
                    },
                    texture: solid.clone(),
                });
            }
        }
        if stored.height > old.size.height {
            for x in xs.windows(2) {
                tiles.push(Tile {
                    backing: None,
                    rectangle: Rect {
                        left: x[0] as i32,
                        top: old.size.height as i32,
                        width: x[1] - x[0],
                        height: stored.height - old.size.height,
                    },
                    texture: solid.clone(),
                });
            }
        }
        Ok(Image {
            size,
            canvas: true,
            text: source.text,
            device: source.device.clone(),
            province: None,
            main: Some(Rc::new(Plane {
                size: stored,
                budget: old.budget.clone(),
                tiles,
            })),
        })
    }

    /// Compact all aliases of a plane together before a scene pins its pixels.
    /// A shared layer is reclaimable only when every owner is in this set;
    /// snapshots outside it keep their original storage and never get mutated.
    pub fn compact_canvases<'a>(
        &self,
        images: impl Iterator<Item = &'a mut Image>,
        headroom: usize,
    ) -> Result<()> {
        if !self.device.streamed_uploads() {
            return Ok(());
        }
        let mut groups = HashMap::<usize, Vec<&mut Image>>::new();
        for image in images {
            self.check_image(image)?;
            if let Some(plane) = &image.main {
                groups
                    .entry(Rc::as_ptr(plane) as usize)
                    .or_default()
                    .push(image);
            }
        }
        let mut groups: Vec<_> = groups
            .into_values()
            .filter_map(|mut group| {
                let first = group.iter().position(|image| image.canvas)?;
                group.swap(0, first);
                let plane = group[0].main.as_ref().unwrap();
                (Rc::strong_count(plane) == group.len()).then_some(group)
            })
            .collect();
        // Reclaim the most promising groups first while their smaller replacement
        // still fits. Hash-map iteration must not decide which large copy wins.
        groups.sort_by_key(|group| {
            std::cmp::Reverse(
                group[0]
                    .main
                    .as_ref()
                    .unwrap()
                    .tiles
                    .iter()
                    .map(|tile| {
                        tile.texture.constant_background().map_or(0, |(_, damage)| {
                            tile.texture.allocation_bytes().saturating_sub(
                                damage.map_or(4, |r| r.width as usize * r.height as usize * 4),
                            )
                        })
                    })
                    .sum::<usize>(),
            )
        });
        for borders in [false, true] {
            let mut changed = false;
            for group in &mut groups {
                if self.resident.available() >= headroom {
                    break;
                }
                let owners = group.len();
                if !self.compact_canvas_owned(group[0], borders, owners)? {
                    continue;
                }
                let plane = group[0].main.as_ref().unwrap().clone();
                for image in &mut group[1..] {
                    image.main = Some(plane.clone());
                }
                changed = true;
                if borders {
                    self.collect()?;
                }
            }
            if changed && !borders {
                self.collect()?;
            }
        }
        self.compact_shared_tile_views(&mut groups, headroom)?;
        self.compact_shared_tile_borders(&mut groups, headroom)?;
        Ok(())
    }

    fn compact_shared_tile_views(
        &self,
        groups: &mut [Vec<&mut Image>],
        headroom: usize,
    ) -> Result<()> {
        if self.resident.available() >= headroom {
            return Ok(());
        }
        let mut candidates = HashMap::<usize, (Weak<Texture>, usize, Rect)>::new();
        for group in groups.iter() {
            for tile in &group[0].main.as_ref().unwrap().tiles {
                let backing = tile.sample_rectangle();
                if !tile.texture.renderable()
                    || !tile.texture.belongs_to(&self.resident)
                    || tile.texture.solid_color().is_some()
                    || backing.width != tile.texture.size.width
                    || backing.height != tile.texture.size.height
                    || tile.rectangle.intersection(backing) != Some(tile.rectangle)
                {
                    continue;
                }
                // Keep the neighbouring texels used by linear filtering too.
                let left = (tile.rectangle.left - backing.left)
                    .saturating_sub(1)
                    .max(0);
                let top = (tile.rectangle.top - backing.top).saturating_sub(1).max(0);
                let right = (i64::from(tile.rectangle.left) - i64::from(backing.left)
                    + i64::from(tile.rectangle.width)
                    + 1)
                .min(i64::from(backing.width));
                let bottom = (i64::from(tile.rectangle.top) - i64::from(backing.top)
                    + i64::from(tile.rectangle.height)
                    + 1)
                .min(i64::from(backing.height));
                let area = Rect {
                    left,
                    top,
                    width: (right - i64::from(left)) as u32,
                    height: (bottom - i64::from(top)) as u32,
                };
                let entry = candidates
                    .entry(Rc::as_ptr(&tile.texture) as usize)
                    .or_insert((Rc::downgrade(&tile.texture), 0, area));
                entry.1 += 1;
                entry.2 = crate::scene_damage::union(Some(entry.2), area);
            }
        }
        let mut candidates: Vec<_> = candidates
            .into_iter()
            .filter_map(|(key, (weak, owners, area))| {
                let source = weak.upgrade()?;
                let bytes = area.width as usize * area.height as usize * 4;
                let saving = source.allocation_bytes().saturating_sub(bytes);
                (saving >= 64 * 1024 && saving >= source.allocation_bytes() / 8)
                    .then_some((saving, key, weak, owners, area, bytes))
            })
            .collect();
        candidates.sort_by_key(|(saving, ..)| std::cmp::Reverse(*saving));
        for (_, key, weak, owners, area, bytes) in candidates {
            if self.resident.available() >= headroom {
                break;
            }
            // External snapshots and unlisted planes must retain their pixels.
            if weak.strong_count() != owners || bytes > self.resident.available() {
                continue;
            }
            let Some(source) = weak.upgrade() else {
                continue;
            };
            let texture = self.device.sample_texture(
                Size {
                    width: area.width,
                    height: area.height,
                },
                &self.resident,
            )?;
            self.device.copy_region(&source, area, &texture)?;
            for group in groups.iter_mut() {
                let old = group[0].main.as_ref().unwrap();
                if !old
                    .tiles
                    .iter()
                    .any(|tile| Rc::as_ptr(&tile.texture) as usize == key)
                {
                    continue;
                }
                let mut tiles = old.tiles.clone();
                for tile in &mut tiles {
                    if Rc::as_ptr(&tile.texture) as usize != key {
                        continue;
                    }
                    let backing = tile.sample_rectangle();
                    tile.backing = Some(Rect {
                        left: backing.left + area.left,
                        top: backing.top + area.top,
                        width: area.width,
                        height: area.height,
                    });
                    tile.texture = texture.clone();
                }
                let plane = Rc::new(Plane {
                    size: old.size,
                    budget: old.budget.clone(),
                    tiles,
                });
                for image in group {
                    image.main = Some(plane.clone());
                }
            }
            drop(source);
            self.collect()?;
        }
        Ok(())
    }

    fn compact_shared_tile_borders(
        &self,
        groups: &mut [Vec<&mut Image>],
        headroom: usize,
    ) -> Result<()> {
        if self.resident.available() >= headroom {
            return Ok(());
        }
        let mut candidates = HashMap::<usize, (Weak<Texture>, usize, usize)>::new();
        for group in groups.iter() {
            for tile in &group[0].main.as_ref().unwrap().tiles {
                if !tile.renderable() {
                    continue;
                }
                let Some((_, Some(damage))) = tile.texture.constant_background() else {
                    continue;
                };
                let saving = tile
                    .texture
                    .allocation_bytes()
                    .saturating_sub(damage.width as usize * damage.height as usize * 4 + 4);
                if saving < 64 * 1024 {
                    continue;
                }
                let entry = candidates
                    .entry(Rc::as_ptr(&tile.texture) as usize)
                    .or_insert((Rc::downgrade(&tile.texture), 0, saving));
                entry.1 += 1;
            }
        }
        let mut candidates: Vec<_> = candidates.into_iter().collect();
        candidates.sort_by_key(|(_, (_, _, saving))| std::cmp::Reverse(*saving));
        for (key, (weak, owners, _)) in candidates {
            if self.resident.available() >= headroom {
                break;
            }
            // Any unlisted texture or plane owner is an immutable snapshot.
            if owners < 2 || weak.strong_count() != owners {
                continue;
            }
            if groups.iter().any(|g| {
                let p = g[0].main.as_ref().unwrap();
                let count = p
                    .tiles
                    .iter()
                    .filter(|t| Rc::as_ptr(&t.texture) as usize == key)
                    .count();
                count > 0
                    && (Rc::strong_count(p) != g.len()
                        || p.tiles.len() + count * 4 > MAX_WRITE_REGIONS)
            }) {
                continue;
            }
            let Some(source) = weak.upgrade() else {
                continue;
            };
            let Some((color, Some(damage))) = source.constant_background() else {
                continue;
            };
            let size = Size {
                width: damage.width,
                height: damage.height,
            };
            if size.rgba_bytes().unwrap().saturating_add(4) > self.resident.available() {
                continue;
            }
            let texture = self.device.sample_texture(size, &self.resident)?;
            let constant = self.canvas_solid_tile(
                Size {
                    width: 1,
                    height: 1,
                },
                color,
                true,
            )?;
            self.copy_tile_storage(
                &Tile {
                    rectangle: damage,
                    backing: Some(source.size.rect()),
                    texture: source.clone(),
                },
                &texture,
                &self.resident,
            )?;
            for group in groups.iter_mut() {
                let old = group[0].main.as_ref().unwrap();
                if !old
                    .tiles
                    .iter()
                    .any(|t| Rc::as_ptr(&t.texture) as usize == key)
                {
                    continue;
                }
                let mut tiles = Vec::new();
                for tile in &old.tiles {
                    if Rc::as_ptr(&tile.texture) as usize != key {
                        tiles.push(tile.clone());
                        continue;
                    }
                    let rectangle = Rect {
                        left: tile.rectangle.left + damage.left,
                        top: tile.rectangle.top + damage.top,
                        ..damage
                    };
                    tiles.push(Tile {
                        rectangle,
                        backing: None,
                        texture: texture.clone(),
                    });
                    tiles.extend(
                        outside(tile.rectangle, rectangle)
                            .into_iter()
                            .filter(|r| r.width != 0 && r.height != 0)
                            .map(|rectangle| Tile {
                                rectangle,
                                backing: None,
                                texture: constant.clone(),
                            }),
                    );
                }
                let plane = Rc::new(Plane {
                    size: old.size,
                    budget: old.budget.clone(),
                    tiles,
                });
                for image in group {
                    image.main = Some(plane.clone());
                }
            }
            drop(source);
            self.collect()?;
        }
        Ok(())
    }
    /// Recover known constant borders under pressure without reading pixels
    /// back. Only unique nonuniform storage is copied, so replacing it really
    /// releases memory rather than duplicating another layer's shared image.
    pub fn compact_canvas(&self, image: &mut Image, borders: bool) -> Result<bool> {
        self.compact_canvas_owned(image, borders, 1)
    }
    fn compact_canvas_owned(
        &self,
        image: &mut Image,
        borders: bool,
        owners: usize,
    ) -> Result<bool> {
        if !image.canvas || !self.device.streamed_uploads() {
            return Ok(false);
        }
        let old = image.plane(false)?;
        if old.tiles.len() > MAX_WRITE_REGIONS {
            return Ok(false);
        }
        let mut tiles = Vec::with_capacity(old.tiles.len());
        let mut changed = false;
        for (index, tile) in old.tiles.iter().enumerate() {
            let background = tile.texture.constant_background().filter(|(_, damage)| {
                tile.renderable()
                    && tiles.len() + old.tiles.len() - index + 4 <= MAX_WRITE_REGIONS
                    && tile.texture.allocation_bytes() >= 64 * 1024
                    && (damage.is_none()
                        || (borders
                            && Rc::strong_count(old) == owners
                            && Rc::strong_count(&tile.texture) == 1))
            });
            let Some((color, damage)) = background else {
                tiles.push(tile.clone());
                continue;
            };
            let bytes = damage.map_or(0, |r| r.width as usize * r.height as usize * 4);
            let saving = tile.texture.allocation_bytes().saturating_sub(bytes);
            if saving < 64 * 1024
                || saving < tile.texture.allocation_bytes() / 8
                || bytes.saturating_add(4) > self.resident.available()
            {
                tiles.push(tile.clone());
                continue;
            }
            let constant = self.canvas_solid_tile(
                Size {
                    width: 1,
                    height: 1,
                },
                color,
                true,
            )?;
            if let Some(damage) = damage {
                let rectangle = Rect {
                    left: tile.rectangle.left + damage.left,
                    top: tile.rectangle.top + damage.top,
                    ..damage
                };
                let texture = self.device.sample_texture(
                    Size {
                        width: damage.width,
                        height: damage.height,
                    },
                    &old.budget,
                )?;
                let output = Plane {
                    size: old.size,
                    budget: old.budget.clone(),
                    tiles: vec![Tile {
                        backing: None,
                        rectangle,
                        texture,
                    }],
                };
                self.draw(
                    &output,
                    Some(old),
                    rectangle,
                    &crate::drawing::Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
                )?;
                tiles.extend(output.tiles);
                for rectangle in outside(tile.rectangle, rectangle)
                    .into_iter()
                    .filter(|r| r.width != 0 && r.height != 0)
                {
                    tiles.push(Tile {
                        backing: None,
                        rectangle,
                        texture: constant.clone(),
                    });
                }
            } else {
                tiles.push(Tile {
                    backing: None,
                    rectangle: tile.rectangle,
                    texture: constant,
                });
            }
            changed = true;
        }
        if changed {
            image.main = Some(Rc::new(Plane {
                size: old.size,
                budget: old.budget.clone(),
                tiles,
            }));
        }
        Ok(changed)
    }
    pub(crate) fn solid_region_write_bytes(&self, image: &Image, area: Rect) -> Option<usize> {
        if !image.canvas || !self.device.streamed_uploads() {
            return None;
        }
        let plane = image.main.as_ref()?;
        // New masks can consume atlas space before the destination is made
        // writable. Its tighter exact layout must fit the tile-count cap too.
        solid_write_layout(plane, area, true)?;
        let (materialized, _) = solid_write_layout(plane, area, false)?;
        Some(plane.tiles.iter().fold(0usize, |sum, tile| {
            if tile.rectangle.intersection(area).is_none() {
                return sum;
            }
            let size = if tile.backing.is_none() && tile.texture.size != tile.size() {
                let part = tile.rectangle.intersection(materialized).unwrap();
                Size {
                    width: part.width,
                    height: part.height,
                }
            } else if !tile.renderable()
                || Rc::strong_count(plane) > 1
                || Rc::strong_count(&tile.texture) > 1
            {
                tile.size()
            } else {
                return sum;
            };
            sum.saturating_add(size.rgba_bytes().unwrap_or(usize::MAX))
        }))
    }
    /// A draw into half a virtual margin needs storage only for that half.
    /// Preserve the remaining constant rectangles without allocating pixels.
    pub(crate) fn write_solid_regions(
        &self,
        image: &mut Image,
        bounds: Rect,
        discard: bool,
    ) -> Result<bool> {
        if !image.canvas || !self.device.streamed_uploads() {
            return Ok(false);
        }
        let old = image.plane(false)?;
        if old.size != self.fill_main_size(image) {
            return Ok(false);
        }
        let raster = crate::scene::raster::Raster::new(image.size, old.size, (0, 0))?;
        let Some(area) = bounds
            .intersection(image.size.rect())
            .and_then(|r| raster.rect(r))
        else {
            return Ok(true);
        };
        let Some((materialized, count)) = solid_write_layout(old, area, discard) else {
            return Ok(false);
        };
        let mut tiles = Vec::with_capacity(count);
        for tile in &old.tiles {
            let Some(part) = tile.rectangle.intersection(area) else {
                tiles.push(tile.clone());
                continue;
            };
            if tile.backing.is_none() && tile.texture.size != tile.size() {
                let part = tile.rectangle.intersection(materialized).unwrap();
                for rectangle in outside(tile.rectangle, part)
                    .into_iter()
                    .filter(|r| r.width != 0 && r.height != 0)
                {
                    tiles.push(Tile {
                        backing: None,
                        rectangle,
                        texture: tile.texture.clone(),
                    });
                }
                let size = Size {
                    width: part.width,
                    height: part.height,
                };
                let texture = self.device.sample_texture(size, &old.budget)?;
                if !discard {
                    self.copy_tile_storage(tile, &texture, &old.budget)?;
                }
                tiles.push(Tile {
                    backing: None,
                    rectangle: part,
                    texture,
                });
            } else {
                let texture = if !tile.renderable()
                    || Rc::strong_count(old) > 1
                    || Rc::strong_count(&tile.texture) > 1
                {
                    let texture = self.device.sample_texture(tile.size(), &old.budget)?;
                    if !discard || part != tile.rectangle {
                        self.copy_tile_storage(tile, &texture, &old.budget)?;
                    }
                    texture
                } else {
                    tile.texture.clone()
                };
                tiles.push(Tile {
                    backing: None,
                    rectangle: tile.rectangle,
                    texture,
                });
            }
        }
        image.main = Some(Rc::new(Plane {
            size: old.size,
            budget: old.budget.clone(),
            tiles,
        }));
        Ok(true)
    }
    fn solid_fill_layout(
        &self,
        image: &Image,
        fill: &krkr_protocol::graphics::Fill,
    ) -> Option<(crate::scene::raster::Raster, Rect, usize)> {
        if !image.canvas || !self.device.streamed_uploads() || crate::fills::mask(fill) != [true; 4]
        {
            return None;
        }
        let old = image.main.as_ref()?;
        if old.size != self.fill_main_size(image)
            || old.size.rgba_bytes().unwrap_or(0) < 256 * 1024
            || self.capacity_after_collect(&self.resident) < 8
        {
            return None;
        }
        let raster = crate::scene::raster::Raster::new(image.size, old.size, (0, 0)).ok()?;
        let area = fill
            .rectangle
            .intersection(image.size.rect())
            .and_then(|r| raster.rect(r))?;
        let split = |tile: &Tile| solid_fill_part(tile, image.size, raster, area, fill);
        if !old.tiles.iter().any(|t| split(t).is_some()) {
            return None;
        }
        let count = old
            .tiles
            .iter()
            .map(|t| {
                split(t).map_or(1, |p| {
                    1 + outside(t.rectangle, p)
                        .iter()
                        .filter(|r| r.width != 0 && r.height != 0)
                        .count()
                })
            })
            .sum::<usize>();
        (count <= 64).then_some((raster, area, count))
    }

    pub(crate) fn solid_fill_bytes(
        &self,
        image: &Image,
        fill: &krkr_protocol::graphics::Fill,
    ) -> Option<usize> {
        let (raster, area, _) = self.solid_fill_layout(image, fill)?;
        let old = image.main.as_ref()?;
        Some(old.tiles.iter().fold(0usize, |bytes, tile| {
            let added = if solid_fill_part(tile, image.size, raster, area, fill).is_some() {
                // The old and new constants are each one RGBA texel. All
                // outside pieces of this tile share the same old constant.
                8
            } else if !crate::fills::unchanged(tile, image.size, raster, fill)
                && (!tile.renderable()
                    || Rc::strong_count(old) > 1
                    || Rc::strong_count(&tile.texture) > 1)
            {
                tile.size().rgba_bytes().unwrap_or(usize::MAX)
            } else {
                0
            };
            bytes.saturating_add(added)
        }))
    }

    /// Clearing a constant tile only changes rectangular metadata. Keep the
    /// untouched color and the cleared part as single-texel views; detach other
    /// shared tiles before publishing any of these changes.
    pub(crate) fn fill_solid_regions(
        &self,
        image: &mut Image,
        fill: &krkr_protocol::graphics::Fill,
    ) -> Result<bool> {
        let Some((raster, area, count)) = self.solid_fill_layout(image, fill) else {
            return Ok(false);
        };
        let old = image.plane(false)?;
        let split = |tile: &Tile| solid_fill_part(tile, image.size, raster, area, fill);
        let mut tiles = Vec::with_capacity(count);
        for tile in &old.tiles {
            if let Some(part) = split(tile) {
                for rectangle in outside(tile.rectangle, part)
                    .into_iter()
                    .filter(|r| r.width != 0 && r.height != 0)
                {
                    let color = tile
                        .texture
                        .solid_color()
                        .expect("partial split has a constant background");
                    let texture = self.canvas_solid_tile(
                        Size {
                            width: 1,
                            height: 1,
                        },
                        color,
                        true,
                    )?;
                    tiles.push(Tile {
                        backing: None,
                        rectangle,
                        texture,
                    });
                }
                let texture = self.canvas_solid_tile(
                    Size {
                        width: 1,
                        height: 1,
                    },
                    fill.color,
                    true,
                )?;
                tiles.push(Tile {
                    backing: None,
                    rectangle: part,
                    texture,
                });
            } else {
                let texture = if !crate::fills::unchanged(tile, image.size, raster, fill)
                    && (!tile.renderable()
                        || Rc::strong_count(old) > 1
                        || Rc::strong_count(&tile.texture) > 1)
                {
                    let texture = self.device.sample_texture(tile.size(), &old.budget)?;
                    self.copy_tile_storage(tile, &texture, &old.budget)?;
                    texture
                } else {
                    tile.texture.clone()
                };
                tiles.push(Tile {
                    backing: None,
                    rectangle: tile.rectangle,
                    texture,
                });
            }
        }
        image.main = Some(Rc::new(Plane {
            size: old.size,
            budget: old.budget.clone(),
            tiles,
        }));
        Ok(true)
    }
    pub(crate) fn canvas_solid_tile(
        &self,
        size: Size,
        color: u32,
        shared: bool,
    ) -> Result<Rc<Texture>> {
        if shared {
            let mut pool = self.canvas_solids.borrow_mut();
            pool.trim();
            if let Some(texture) = pool.0.iter().filter_map(Weak::upgrade).find(|t| {
                t.size
                    == (Size {
                        width: 1,
                        height: 1,
                    })
                    && t.belongs_to(&self.resident)
                    && t.solid_color() == Some(color)
            }) {
                return Ok(texture);
            }
        }
        let size = if shared {
            Size {
                width: 1,
                height: 1,
            }
        } else {
            size
        };
        let texture = self.device.sample_texture(size, &self.resident)?;
        let framebuffer = texture.overwrite_framebuffer(size.rect())?;
        let rgba = crate::drawing::rgba(color);
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, size.width as i32, size.height as i32);
            gl.color_mask(true, true, true, true);
            gl.clear_color(
                rgba[0] / 255.,
                rgba[1] / 255.,
                rgba[2] / 255.,
                rgba[3] / 255.,
            );
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.device.check()?;
        texture.cleared(size.rect(), color);
        if shared {
            let mut pool = self.canvas_solids.borrow_mut();
            if pool.0.len() == 128 {
                pool.0.remove(0);
            }
            pool.0.push(Rc::downgrade(&texture));
        }
        Ok(texture)
    }

    pub(crate) fn partition_canvas(
        &self,
        image: &Image,
        size: Size,
        bounds: Rect,
    ) -> Result<Option<Rc<Plane>>> {
        let Some(partition) = self.canvas_partition(image, size, bounds) else {
            return Ok(None);
        };
        let Partition {
            color,
            area,
            xs,
            ys,
        } = partition;
        let mut tiles = Vec::with_capacity((xs.len() - 1) * (ys.len() - 1));
        for y in ys.windows(2) {
            for x in xs.windows(2) {
                let rectangle = Rect {
                    left: x[0] as i32,
                    top: y[0] as i32,
                    width: x[1] - x[0],
                    height: y[1] - y[0],
                };
                let texture = self.canvas_solid_tile(
                    Size {
                        width: rectangle.width,
                        height: rectangle.height,
                    },
                    color,
                    rectangle.intersection(area).is_none(),
                )?;
                tiles.push(Tile {
                    backing: None,
                    rectangle,
                    texture,
                });
            }
        }
        Ok(Some(Rc::new(Plane {
            size,
            budget: self.resident.clone(),
            tiles,
        })))
    }

    /// Match sparse materialization during admission. Charging the complete
    /// canvas for a tiny initial copy evicts useful assets before the renderer
    /// can retain its unchanged margins as one-texel views.
    pub(crate) fn partition_canvas_bytes(
        &self,
        image: &Image,
        size: Size,
        bounds: Rect,
    ) -> Option<usize> {
        let partition = self.canvas_partition(image, size, bounds)?;
        let area = partition.area;
        (area.width as usize)
            .checked_mul(area.height as usize)?
            .checked_mul(4)?
            .checked_add(4)
    }

    fn canvas_partition(&self, image: &Image, size: Size, bounds: Rect) -> Option<Partition> {
        if !self.device.streamed_uploads() || size.rgba_bytes().unwrap_or(0) < 256 * 1024 {
            return None;
        }
        let old = image.main.as_ref()?;
        if old.size
            != (Size {
                width: 1,
                height: 1,
            })
        {
            return None;
        }
        let color = old.tiles.first()?.texture.solid_color()?;
        let area = crate::scene::raster::Raster::new(image.size, size, (0, 0))
            .ok()?
            .rect(bounds)?;
        let pixels = u64::from(size.width) * u64::from(size.height);
        let changed = u64::from(area.width) * u64::from(area.height);
        // A tiny initial write is particularly wasteful to materialize at
        // full size. Keep the same bounded aligned layout for these writes;
        // almost-full copies still use the cheaper ordinary dense path.
        if (pixels - changed) * 16 < pixels {
            return None;
        }
        let right = area.left as u32 + area.width;
        let bottom = area.top as u32 + area.height;
        // Aligned rows and columns let bilinear kernels bind the exact four
        // neighbours without expanding this sparse layout into another image.
        let axis = |inner_start: u32, inner_end: u32, end: u32| {
            let mut starts = Vec::new();
            for pair in [0, inner_start, inner_end, end].windows(2) {
                starts.extend((pair[0]..pair[1]).step_by(self.tile_edge as usize));
            }
            starts.push(end);
            starts
        };
        let xs = axis(area.left as u32, right, size.width);
        let ys = axis(area.top as u32, bottom, size.height);
        let count = (xs.len() - 1) * (ys.len() - 1);
        if count > 16 {
            return None;
        }
        Some(Partition {
            color,
            area,
            xs,
            ys,
        })
    }
}

pub(crate) fn outside(r: Rect, p: Rect) -> [Rect; 4] {
    let right = p.left + p.width as i32;
    let bottom = p.top + p.height as i32;
    [
        Rect {
            left: r.left,
            top: r.top,
            width: r.width,
            height: (p.top - r.top) as u32,
        },
        Rect {
            left: r.left,
            top: bottom,
            width: r.width,
            height: (r.top + r.height as i32 - bottom) as u32,
        },
        Rect {
            left: r.left,
            top: p.top,
            width: (p.left - r.left) as u32,
            height: p.height,
        },
        Rect {
            left: right,
            top: p.top,
            width: (r.left + r.width as i32 - right) as u32,
            height: p.height,
        },
    ]
}

// Admission includes self-copy snapshots that execution will create later.
fn canvas_band_layout(old: &Rc<Plane>, area: Rect, snapshot: bool) -> Vec<(usize, Rect)> {
    let mut parts = Vec::new();
    // Limit draw-list growth independently of the sparse-solid tile cap.
    if area.width > 128 || area.height > 128 || old.tiles.len() > 30 {
        return parts;
    }
    for (index, tile) in old.tiles.iter().enumerate() {
        let Some(hit) = tile.rectangle.intersection(area) else {
            continue;
        };
        let sampling = tile.sample_rectangle();
        if !tile.texture.renderable()
            || tile.texture.size.width != sampling.width
            || tile.texture.size.height != sampling.height
            || (tile.renderable()
                && !snapshot
                && Rc::strong_count(old) == 1
                && Rc::strong_count(&tile.texture) == 1)
        {
            continue;
        }
        // Whole rows amortize later characters and keep vertical seams out
        // of a line. Rounding is in stored pixels, after canvas scaling.
        let top = (hit.top / 64 * 64).max(tile.rectangle.top);
        let bottom = ((hit.top as u32 + hit.height).div_ceil(64) * 64)
            .min(tile.rectangle.top as u32 + tile.rectangle.height);
        let band = Rect {
            top,
            height: bottom - top as u32,
            ..tile.rectangle
        };
        if band.height.saturating_mul(4) > tile.rectangle.height
            || tile.size().rgba_bytes().unwrap_or(0) < 256 * 1024
            || old.tiles.len() + (parts.len() + 1) * 2 > 32
        {
            continue;
        }
        parts.push((index, band));
    }
    parts
}

// Shared by admission and execution so sparse writes charge only the pixels
// they materialize, including the neighboring cell retained for later glyphs.
fn solid_write_layout(old: &Plane, area: Rect, discard: bool) -> Option<(Rect, usize)> {
    if !old.tiles.iter().any(|t| {
        t.backing.is_none()
            && t.texture.size != t.size()
            && t.rectangle.intersection(area).is_some()
    }) {
        return None;
    }
    // Incremental writes must not split every untouched margin at a new
    // pixel boundary. Round each narrow axis independently: a one-pixel-wide
    // gradient column may span the full canvas height. Requiring both axes
    // to fit a cell leaves hundreds of tiny native textures and sync objects.
    // Neighboring pixels keep their solid color until a later write.
    // Discarding writes retain exact bounds because those extra pixels
    // would otherwise be left uninitialized.
    let materialized = if discard {
        area
    } else {
        let axis = |start: i32, length: u32, limit: u32| {
            let end = start as u32 + length;
            if length <= 64 {
                (start / 64 * 64, (end.div_ceil(64) * 64).min(limit))
            } else {
                (start, end)
            }
        };
        let (left, right) = axis(area.left, area.width, old.size.width);
        let (top, bottom) = axis(area.top, area.height, old.size.height);
        let rounded = Rect {
            left,
            top,
            width: right - left as u32,
            height: bottom - top as u32,
        };
        if old.budget.available() >= rounded.width as usize * rounded.height as usize * 4 {
            rounded
        } else {
            area
        }
    };
    let count: usize = old
        .tiles
        .iter()
        .map(|t| {
            if t.backing.is_some() || t.texture.size == t.size() {
                return 1;
            }
            t.rectangle.intersection(area).map_or(1, |_| {
                let part = t.rectangle.intersection(materialized).unwrap();
                1 + outside(t.rectangle, part)
                    .iter()
                    .filter(|r| r.width != 0 && r.height != 0)
                    .count()
            })
        })
        .sum();
    if count > MAX_WRITE_REGIONS {
        return None;
    }
    Some((materialized, count))
}

use crate::{
    Error, Gpu, Result,
    device::{Device, Texture},
};
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
};
use std::rc::Rc;

#[derive(Clone)]
pub struct Image {
    pub size: Size,
    pub(crate) device: Rc<Device>,
    pub(crate) canvas: bool,
    // Text and its bitmap copies retain logical pixels until scene composition.
    pub(crate) text: bool,
    pub(crate) main: Option<Rc<Plane>>,
    pub(crate) province: Option<Rc<Plane>>,
}
impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut result = f.debug_struct("Image");
        result
            .field("logical", &self.size)
            .field("canvas", &self.canvas)
            .field("text", &self.text);
        if let Some(plane) = &self.main {
            result
                .field("stored", &plane.size)
                .field("plane_owners", &Rc::strong_count(plane))
                .field("tiles", &plane.tiles.len())
                .field("bytes", &self.resident_bytes())
                .field(
                    "constant_borders",
                    &plane
                        .tiles
                        .iter()
                        .filter(|t| t.texture.constant_background().is_some())
                        .count(),
                )
                .field(
                    "shared_pixels",
                    &plane
                        .tiles
                        .iter()
                        .filter(|t| {
                            t.texture.solid_color().is_none() && Rc::strong_count(&t.texture) > 1
                        })
                        .count(),
                )
                .field(
                    "views",
                    &plane.tiles.iter().filter(|t| t.backing.is_some()).count(),
                )
                .field(
                    "sample_only",
                    &plane
                        .tiles
                        .iter()
                        .filter(|t| !t.texture.renderable())
                        .count(),
                );
        }
        result.finish()
    }
}
#[derive(Clone)]
pub(crate) struct Tile {
    pub rectangle: Rect,
    /// Coordinates occupied by the full texture before a read-only crop.
    /// The visible rectangle never changes the texel sampling grid.
    pub backing: Option<Rect>,
    pub texture: Rc<Texture>,
}
impl Tile {
    pub(crate) fn sample_rectangle(&self) -> Rect {
        self.backing.unwrap_or(self.rectangle)
    }
    pub(crate) fn cropped(&self, rectangle: Rect) -> Self {
        Self {
            rectangle,
            backing: Some(self.sample_rectangle()),
            texture: self.texture.clone(),
        }
    }
    pub fn size(&self) -> Size {
        Size {
            width: self.rectangle.width,
            height: self.rectangle.height,
        }
    }
    pub fn renderable(&self) -> bool {
        self.backing.is_none() && self.texture.renderable() && self.texture.size == self.size()
    }
    pub fn solid_region(&self, area: Rect) -> Option<u32> {
        if self.backing.is_none() && self.texture.size != self.size() {
            self.texture.solid_color()
        } else {
            let source = self.sample_rectangle();
            self.texture.solid_region(Rect {
                left: area.left + self.rectangle.left - source.left,
                top: area.top + self.rectangle.top - source.top,
                ..area
            })
        }
    }
}
#[derive(Clone)]
pub(crate) struct Plane {
    pub size: Size,
    pub budget: Budget,
    pub tiles: Vec<Tile>,
}
impl Image {
    pub(crate) fn text_view(&self) -> std::borrow::Cow<'_, Self> {
        if self.text {
            std::borrow::Cow::Borrowed(self)
        } else {
            let mut image = self.shared();
            image.text = true;
            std::borrow::Cow::Owned(image)
        }
    }
    pub fn shared(&self) -> Self {
        self.clone()
    }
    pub fn shared_main(&self) -> Self {
        Self {
            province: None,
            ..self.clone()
        }
    }
    pub fn has_main(&self) -> bool {
        self.main.is_some()
    }
    pub fn has_province(&self) -> bool {
        self.province.is_some()
    }
    pub fn stored_size(&self) -> Option<Size> {
        self.main.as_ref().map(|plane| plane.size)
    }
    /// Distinct canvas planes can retain views of the same pixel allocation.
    pub fn shares_main_storage(&self, other: &Image) -> bool {
        self.main
            .as_ref()
            .zip(other.main.as_ref())
            .is_some_and(|(a, b)| {
                a.tiles
                    .iter()
                    .any(|a| b.tiles.iter().any(|b| Rc::ptr_eq(&a.texture, &b.texture)))
            })
    }
    pub fn resident_bytes(&self) -> usize {
        let count = self
            .main
            .iter()
            .chain(self.province.iter())
            .map(|plane| plane.tiles.len())
            .sum::<usize>();
        if count <= 8 {
            let mut seen = [std::ptr::null(); 8];
            let mut used = 0;
            let mut bytes = 0;
            for tile in self
                .main
                .iter()
                .chain(self.province.iter())
                .flat_map(|plane| &plane.tiles)
            {
                let texture = Rc::as_ptr(&tile.texture);
                if !seen[..used].contains(&texture) {
                    seen[used] = texture;
                    used += 1;
                    bytes += tile.texture.allocation_bytes();
                }
            }
            return bytes;
        }
        let mut textures = std::collections::HashSet::new();
        self.main
            .iter()
            .chain(self.province.iter())
            .flat_map(|plane| &plane.tiles)
            .filter(|tile| textures.insert(Rc::as_ptr(&tile.texture)))
            .map(|tile| tile.texture.allocation_bytes())
            .sum()
    }
    /// Storage released by dropping this image alone. Shared planes and
    /// texture views conservatively contribute no immediately reclaimable bytes.
    pub fn reclaimable_bytes(&self) -> usize {
        self.main
            .iter()
            .chain(self.province.iter())
            .filter(|plane| Rc::strong_count(plane) == 1)
            .flat_map(|plane| &plane.tiles)
            .filter(|tile| Rc::strong_count(&tile.texture) == 1)
            .map(|tile| tile.texture.allocation_bytes())
            .sum()
    }
    pub fn independ_bytes(&self, province: bool, copy: bool) -> usize {
        let Some(plane) = (if province { &self.province } else { &self.main }) else {
            return 0;
        };
        (if copy { plane.size } else { self.size })
            .rgba_bytes()
            .unwrap_or(usize::MAX)
    }
    /// Conservative admission for a write before optional cache aliases are
    /// evicted. Unique physical tiles need no replacement allocation.
    pub fn write_bytes(&self, province: bool) -> usize {
        let Some(plane) = (if province { &self.province } else { &self.main }) else {
            return if province {
                self.size.rgba_bytes().unwrap_or(usize::MAX)
            } else {
                0
            };
        };
        if !province && plane.size != self.size {
            return self.size.rgba_bytes().unwrap_or(usize::MAX);
        }
        self.stored_write_bytes(province)
    }
    pub(crate) fn stored_write_bytes(&self, province: bool) -> usize {
        let Some(plane) = (if province { &self.province } else { &self.main }) else {
            return 0;
        };
        plane
            .tiles
            .iter()
            .filter(|tile| {
                !tile.renderable()
                    || Rc::strong_count(plane) > 1
                    || Rc::strong_count(&tile.texture) > 1
            })
            .map(|tile| tile.size().rgba_bytes().unwrap())
            .sum()
    }
    pub(crate) fn plane(&self, province: bool) -> Result<&Rc<Plane>> {
        if province {
            self.province.as_ref()
        } else {
            self.main.as_ref()
        }
        .ok_or(Error::Message("image plane is absent"))
    }
}
impl Gpu {
    pub(crate) fn copy_tile_storage(
        &self,
        source: &Tile,
        target: &Rc<Texture>,
        budget: &Budget,
    ) -> Result<()> {
        if source.backing.is_none() {
            return self.copy_storage(&source.texture, target, budget);
        }
        let size = source.size();
        let mut input = source.clone();
        let backing = source.sample_rectangle();
        input.backing = Some(Rect {
            left: backing.left - source.rectangle.left,
            top: backing.top - source.rectangle.top,
            ..backing
        });
        input.rectangle = size.rect();
        let plane = Plane {
            size,
            budget: budget.clone(),
            tiles: vec![input],
        };
        let output = Plane {
            size,
            budget: budget.clone(),
            tiles: vec![Tile {
                rectangle: size.rect(),
                backing: None,
                texture: target.clone(),
            }],
        };
        self.draw(
            &output,
            Some(&plane),
            size.rect(),
            &crate::drawing::Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
        )
    }
    pub(crate) fn copy_storage(
        &self,
        source: &Rc<Texture>,
        target: &Rc<Texture>,
        budget: &Budget,
    ) -> Result<()> {
        if source.renderable() && source.size == target.size {
            return self.device.copy(source, target);
        }
        // Compressed textures are not framebuffer attachments. Expand on GPU
        // with the same byte-preserving sampler used by ordinary image copies.
        let size = target.size;
        let plane = |texture: Rc<Texture>| Plane {
            size,
            budget: budget.clone(),
            tiles: vec![Tile {
                backing: None,
                rectangle: size.rect(),
                texture,
            }],
        };
        self.draw(
            &plane(target.clone()),
            Some(&plane(source.clone())),
            size.rect(),
            &crate::drawing::Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
        )
    }
    /// New resident storage needed for a script canvas, after density and
    /// reusable allocations are accounted for. Province remains logical-sized.
    pub fn canvas_allocation_bytes(&self, size: Size, source: Option<&Image>) -> usize {
        if size.width == 0
            || size.height == 0
            || size.width > i32::MAX as u32
            || size.height > i32::MAX as u32
        {
            return usize::MAX;
        }
        self.device.plane_allocation_bytes(
            self.canvas_storage(size, source),
            self.tile_edge,
            &self.resident,
        )
    }
    pub fn create_image_bytes(&self, size: Size, color: u32) -> usize {
        let size = self.solid_storage(size);
        if self
            .solid_images
            .borrow_mut()
            .get(self.canvas_storage(size, None), color)
            .is_some()
        {
            0
        } else {
            self.canvas_allocation_bytes(size, None)
        }
    }
    pub fn resize_image_bytes(&self, source: &Image, size: Size, color: u32) -> usize {
        if source.size == size {
            return 0;
        }
        let uniform = self.solid_images.borrow().color(source) == Some(color);
        if uniform && self.canvas_limit.is_some() && !source.has_province() {
            return self.create_image_bytes(size, color);
        }
        if self.shared_resize_storage(source, size).is_some() {
            return 4;
        }
        let main = if !source.has_main()
            || (uniform
                && self
                    .solid_images
                    .borrow_mut()
                    .get(self.canvas_storage(size, Some(source)), color)
                    .is_some())
        {
            0
        } else {
            // Sparse resize partitions have different tile shapes from an
            // ordinary grid. Do not subtract cached dense-grid allocations
            // which cannot actually satisfy those smaller content tiles.
            self.canvas_storage(size, Some(source))
                .rgba_bytes()
                .unwrap_or(usize::MAX)
        };
        main.saturating_add(if source.has_province() {
            size.rgba_bytes().unwrap_or(usize::MAX)
        } else {
            0
        })
    }
    /// Admission for drawing at display density. A unique compact canvas
    /// needs no new full-resolution image just to append another glyph.
    pub fn canvas_write_bytes(&self, image: &Image, province: bool) -> usize {
        if province || !image.canvas || self.canvas_limit.is_none() {
            return image.write_bytes(province);
        }
        let Some(plane) = &image.main else {
            return 0;
        };
        let desired = self.canvas_storage(image.size, Some(image));
        if desired != plane.size {
            return desired.rgba_bytes().unwrap_or(usize::MAX);
        }
        plane
            .tiles
            .iter()
            .filter(|t| {
                !t.renderable() || Rc::strong_count(plane) > 1 || Rc::strong_count(&t.texture) > 1
            })
            .map(|t| t.size().rgba_bytes().unwrap())
            .sum()
    }
    /// Copy admission includes promotion of text to logical resolution.
    pub fn copy_write_bytes(
        &self,
        image: &Image,
        source: &Image,
        area: Rect,
        province: bool,
        snapshot: bool,
    ) -> usize {
        let target = if source.text && !province {
            image.text_view()
        } else {
            std::borrow::Cow::Borrowed(image)
        };
        self.canvas_region_write_bytes(&target, area, province, snapshot)
    }
    /// Blend admission includes the source text's required pixel density.
    pub fn operate_write_bytes(
        &self,
        image: &Image,
        source: &Image,
        area: Rect,
        snapshot: bool,
    ) -> usize {
        let target = if source.text {
            image.text_view()
        } else {
            std::borrow::Cow::Borrowed(image)
        };
        self.canvas_blend_write_bytes(&target, area, snapshot)
    }
    /// A clipped write detaches only intersecting shared/virtual tiles. The
    /// caller marks self-copies because their source snapshot is created later.
    pub fn canvas_region_write_bytes(
        &self,
        image: &Image,
        area: Rect,
        province: bool,
        snapshot: bool,
    ) -> usize {
        let Some(area) = area.intersection(image.size.rect()) else {
            return 0;
        };
        let plane = if province {
            &image.province
        } else {
            &image.main
        };
        let desired = if province {
            image.size
        } else {
            self.fill_main_size(image)
        };
        let Some(plane) = plane else {
            return if province {
                desired.rgba_bytes().unwrap_or(usize::MAX)
            } else {
                0
            };
        };
        if desired != plane.size {
            if !province
                && image.canvas
                && let Some(bytes) = self.partition_canvas_bytes(image, desired, area)
            {
                return bytes;
            }
            return desired.rgba_bytes().unwrap_or(usize::MAX);
        }
        let Ok(raster) = crate::scene::raster::Raster::new(image.size, desired, (0, 0)) else {
            return usize::MAX;
        };
        let Some(area) = raster.rect(area) else {
            return 0;
        };
        plane
            .tiles
            .iter()
            .filter(|t| {
                t.rectangle.intersection(area).is_some()
                    && (!t.renderable()
                        || snapshot
                        || Rc::strong_count(plane) > 1
                        || Rc::strong_count(&t.texture) > 1)
            })
            .fold(0usize, |sum, t| {
                sum.saturating_add(t.size().rgba_bytes().unwrap_or(usize::MAX))
            })
    }
    pub(crate) fn fill_main_size(&self, image: &Image) -> Size {
        if image.text {
            return image.size;
        }
        if image.canvas && self.canvas_limit.is_some() {
            let stored = image.main.as_ref().map_or(image.size, |p| p.size);
            let desired = self.canvas_storage(image.size, Some(image));
            if desired.width < stored.width
                || desired.height < stored.height
                || (stored.width == 1 && stored.height == 1)
            {
                desired
            } else {
                stored
            }
        } else {
            image.size
        }
    }
    /// A clear outside a solid tile's conservative damage bounds can already
    /// contain the requested channels. Prove this before copy-on-write: border
    /// clears otherwise detach an entire shared portrait or effect canvas.
    pub(crate) fn fills_unchanged(&self, image: &Image, fills: &[Fill]) -> bool {
        fills.iter().all(|fill| {
            let Some(area) = fill.rectangle.intersection(image.size.rect()) else {
                return true;
            };
            let province = fill.face == DrawFace::Province;
            let Ok(plane) = image.plane(province) else {
                return false;
            };
            let (mask, color) = match fill.face {
                DrawFace::Province => (0x00ff0000, (fill.color & 255) << 16),
                DrawFace::Mask => (0xff000000, (fill.color & 255) << 24),
                DrawFace::Opaque if fill.hold_alpha => (0x00ffffff, fill.color),
                _ => (u32::MAX, fill.color),
            };
            if plane.size
                == (Size {
                    width: 1,
                    height: 1,
                })
            {
                // A small logical rectangle may round to no stored pixels in
                // a virtual solid, yet materialization would make it visible.
                return plane.tiles.len() == 1
                    && plane.tiles[0]
                        .texture
                        .solid_color()
                        .is_some_and(|old| old & mask == color & mask);
            }
            // A write that changes storage density can resample unrelated
            // pixels. Only a virtual solid is safe across that conversion.
            if !province && plane.size != self.fill_main_size(image) {
                return false;
            }
            let Ok(raster) = crate::scene::raster::Raster::new(image.size, plane.size, (0, 0))
            else {
                return false;
            };
            let Some(area) = raster.rect(area) else {
                return true;
            };
            plane.tiles.iter().all(|tile| {
                let Some(part) = area.intersection(tile.rectangle) else {
                    return true;
                };
                tile.solid_region(Rect {
                    left: part.left - tile.rectangle.left,
                    top: part.top - tile.rectangle.top,
                    ..part
                })
                .is_some_and(|old| old & mask == color & mask)
            })
        })
    }
    /// Clearing the entire recorded damage restores the original background,
    /// even when the script excludes unchanged outer margins from its clip.
    /// Prove the complement before detaching shared storage.
    pub(crate) fn fill_restores_solid(&self, image: &Image, fill: &Fill) -> bool {
        if !image.canvas
            || self.canvas_limit.is_none()
            || fill.face == DrawFace::Province
            || crate::fills::mask(fill) != [true; 4]
        {
            return false;
        }
        let Ok(plane) = image.plane(false) else {
            return false;
        };
        if plane.size != self.fill_main_size(image) {
            return false;
        }
        let Ok(raster) = crate::scene::raster::Raster::new(image.size, plane.size, (0, 0)) else {
            return false;
        };
        let Some(area) = fill
            .rectangle
            .intersection(image.size.rect())
            .and_then(|r| raster.rect(r))
        else {
            return false;
        };
        plane.tiles.iter().all(|tile| {
            let Some(part) = area.intersection(tile.rectangle) else {
                return tile.texture.solid_color() == Some(fill.color);
            };
            let left = (part.left - tile.rectangle.left) as u32;
            let top = (part.top - tile.rectangle.top) as u32;
            let right = left + part.width;
            let bottom = top + part.height;
            [
                Rect {
                    left: 0,
                    top: 0,
                    width: tile.rectangle.width,
                    height: top,
                },
                Rect {
                    left: 0,
                    top: bottom as i32,
                    width: tile.rectangle.width,
                    height: tile.rectangle.height - bottom,
                },
                Rect {
                    left: 0,
                    top: top as i32,
                    width: left,
                    height: part.height,
                },
                Rect {
                    left: right as i32,
                    top: top as i32,
                    width: tile.rectangle.width - right,
                    height: part.height,
                },
            ]
            .into_iter()
            .all(|outside| {
                outside.width == 0
                    || outside.height == 0
                    || tile.solid_region(outside) == Some(fill.color)
            })
        })
    }
    /// Charge only tiles touched by the fill, including compact pixel rounding.
    /// A shared solid already in the cache needs no replacement allocation.
    pub fn fill_write_bytes(&self, image: &Image, fills: &[Fill]) -> usize {
        if fills.len() == 1
            && let Some(fill) = self.constant_channel_fill(image, &fills[0])
        {
            return self.fill_write_bytes(image, std::slice::from_ref(&fill));
        }
        if self.fills_unchanged(image, fills) {
            return 0;
        }
        if fills.len() == 1
            && let Some(bytes) = self.crop_fill_bytes(image, &fills[0])
        {
            return bytes;
        }
        if fills.len() == 1 && self.fill_restores_solid(image, &fills[0]) {
            return self.create_image_bytes(image.size, fills[0].color);
        }
        if fills.len() == 1
            && let Some(bytes) = self.solid_fill_bytes(image, &fills[0])
        {
            return bytes;
        }
        if image.canvas
            && self.canvas_limit.is_some()
            && fills.len() == 1
            && fills[0].face != DrawFace::Province
            && crate::fills::mask(&fills[0]) == [true; 4]
            && fills[0].rectangle.intersection(image.size.rect()) == Some(image.size.rect())
        {
            return self.create_image_bytes(image.size, fills[0].color);
        }
        let mut bytes = 0usize;
        for province in [false, true] {
            let touches = |area: Rect, raster: crate::scene::raster::Raster| {
                fills.iter().any(|fill| {
                    (fill.face == DrawFace::Province) == province
                        && fill
                            .rectangle
                            .intersection(image.size.rect())
                            .and_then(|r| raster.rect(r))
                            .and_then(|r| r.intersection(area))
                            .is_some()
                })
            };
            let desired = if province {
                image.size
            } else {
                self.fill_main_size(image)
            };
            let Ok(raster) = crate::scene::raster::Raster::new(image.size, desired, (0, 0)) else {
                return usize::MAX;
            };
            if !touches(desired.rect(), raster) {
                continue;
            }
            if !province
                && image.canvas
                && fills.len() == 1
                && crate::fills::mask(&fills[0]) == [true; 4]
                && fills[0].rectangle.intersection(image.size.rect()) == Some(image.size.rect())
                && self
                    .solid_images
                    .borrow_mut()
                    .get(desired, fills[0].color)
                    .is_some()
            {
                continue;
            }
            let plane = if province {
                &image.province
            } else {
                &image.main
            };
            let required = if let Some(plane) = plane {
                if plane.size != desired {
                    if province {
                        desired.rgba_bytes().unwrap_or(usize::MAX)
                    } else {
                        self.device
                            .plane_allocation_bytes(desired, self.tile_edge, &self.resident)
                    }
                } else {
                    plane
                        .tiles
                        .iter()
                        .filter(|t| {
                            touches(t.rectangle, raster)
                                && fills.iter().any(|fill| {
                                    !crate::fills::unchanged(t, image.size, raster, fill)
                                })
                                && (!t.renderable()
                                    || Rc::strong_count(plane) > 1
                                    || Rc::strong_count(&t.texture) > 1)
                        })
                        .map(|t| t.size().rgba_bytes().unwrap())
                        .sum()
                }
            } else if province {
                // Main and province may need the same tile shapes. Only the
                // main charge discounts reuse; one idle allocation cannot be
                // promised to both planes in a mixed fill command.
                desired.rgba_bytes().unwrap_or(usize::MAX)
            } else {
                0
            };
            bytes = bytes.saturating_add(required);
        }
        bytes
    }
    pub(crate) fn plane(&self, size: Size, budget: &Budget) -> Result<Rc<Plane>> {
        self.plane_with_edge(size, budget, self.tile_edge)
    }
    /// Private storage for a pass which initializes every pixel before any
    /// read or publication. Avoid a zero upload/clear before that overwrite.
    pub(crate) fn overwrite_plane(&self, size: Size, budget: &Budget) -> Result<Rc<Plane>> {
        self.allocate_plane(size, budget, self.tile_edge, false, None)
    }
    pub(crate) fn plane_with_edge(
        &self,
        size: Size,
        budget: &Budget,
        edge: u32,
    ) -> Result<Rc<Plane>> {
        self.allocate_plane(size, budget, edge, true, None)
    }
    fn allocate_plane(
        &self,
        size: Size,
        budget: &Budget,
        edge: u32,
        clear: bool,
        pixels: Option<&[u8]>,
    ) -> Result<Rc<Plane>> {
        if size.width == 0
            || size.height == 0
            || size.width > i32::MAX as u32
            || size.height > i32::MAX as u32
        {
            return Err(Error::Message("invalid logical image dimensions"));
        }
        size.rgba_bytes()
            .ok_or(Error::Message("image byte size overflow"))?;
        if pixels.is_some_and(|bytes| size.rgba_bytes() != Some(bytes.len())) {
            return Err(Error::Message("upload byte count differs from image"));
        }
        let mut packed = if pixels.is_some() && size.width > edge {
            let stride = edge as usize * 4;
            let full = stride * size.height.min(edge) as usize;
            let capacity = if full <= self.staging.available() {
                full
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
        let required = || self.device.plane_allocation_bytes(size, edge, budget);
        if budget.available() < required() {
            self.device.collect()?;
            if budget.available() < required() {
                self.collect()?;
            }
        }
        // Admit the whole image before creating the first tile. Each texture
        // then owns its slice of the same budget, including retired allocations.
        drop(budget.reserve(required())?);
        let mut tiles = Vec::new();
        for top in (0..size.height).step_by(edge as usize) {
            for left in (0..size.width).step_by(edge as usize) {
                let extent = Size {
                    width: edge.min(size.width - left),
                    height: edge.min(size.height - top),
                };
                let texture = if let Some(bytes) = pixels {
                    if let Some(packed) = packed.as_mut() {
                        let row_bytes = extent.width as usize * 4;
                        let rows =
                            (packed.as_slice().len() / row_bytes).min(extent.height as usize);
                        if rows == extent.height as usize {
                            let output = &mut packed.as_mut_slice()[..extent.rgba_bytes().unwrap()];
                            for (row, target) in output.chunks_exact_mut(row_bytes).enumerate() {
                                let start = ((top as usize + row) * size.width as usize
                                    + left as usize)
                                    * 4;
                                target.copy_from_slice(&bytes[start..start + row_bytes]);
                            }
                            self.device.uploaded_texture(extent, output, budget)?
                        } else {
                            // A decoded image may already occupy nearly all staging.
                            // Pack rows into bounded strips without another full tile.
                            let texture = self.device.sample_texture(extent, budget)?;
                            for y in (0..extent.height as usize).step_by(rows) {
                                let height = rows.min(extent.height as usize - y);
                                let output = &mut packed.as_mut_slice()[..height * row_bytes];
                                for (row, target) in output.chunks_exact_mut(row_bytes).enumerate()
                                {
                                    let start = ((top as usize + y + row) * size.width as usize
                                        + left as usize)
                                        * 4;
                                    target.copy_from_slice(&bytes[start..start + row_bytes]);
                                }
                                self.device.upload_region(
                                    &texture,
                                    Rect {
                                        left: 0,
                                        top: y as i32,
                                        width: extent.width,
                                        height: height as u32,
                                    },
                                    output,
                                )?;
                            }
                            texture
                        }
                    } else {
                        let start = top as usize * size.width as usize * 4;
                        self.device.uploaded_texture(
                            extent,
                            &bytes[start..start + extent.rgba_bytes().unwrap()],
                            budget,
                        )?
                    }
                } else if clear {
                    self.device.texture(extent, budget)?
                } else {
                    self.device.sample_texture(extent, budget)?
                };
                tiles.push(Tile {
                    backing: None,
                    rectangle: Rect {
                        left: left as i32,
                        top: top as i32,
                        ..extent.rect()
                    },
                    texture,
                });
            }
        }
        Ok(Rc::new(Plane {
            size,
            tiles,
            budget: budget.clone(),
        }))
    }
    /// Private until a complete RGBA upload or copy succeeds. Unlike
    /// reserve_upload, this must never expose partial initialization.
    pub(crate) fn reserve_full_upload(&self, size: Size) -> Result<Image> {
        Ok(Image {
            size,
            canvas: false,
            text: false,
            device: self.device.clone(),
            main: Some(self.allocate_plane(size, &self.resident, self.tile_edge, false, None)?),
            province: None,
        })
    }
    pub(crate) fn upload_main(&self, pixels: &krkr_protocol::pixels::Pixels) -> Result<Image> {
        let bytes = pixels
            .main
            .as_ref()
            .ok_or(Error::Message("RGBA pixels required"))?;
        if pixels.province.is_some() {
            return Err(Error::Message("main upload cannot contain province pixels"));
        }
        Ok(Image {
            size: pixels.size,
            canvas: false,
            text: false,
            device: self.device.clone(),
            main: Some(self.allocate_plane(
                pixels.size,
                &self.resident,
                self.tile_edge,
                false,
                Some(bytes.as_slice()),
            )?),
            province: None,
        })
    }
    pub fn reserve_upload(&self, size: Size, main: bool, province: bool) -> Result<Image> {
        Ok(Image {
            size,
            canvas: false,
            text: false,
            device: self.device.clone(),
            main: main.then(|| self.plane(size, &self.resident)).transpose()?,
            province: province
                .then(|| self.plane(size, &self.resident))
                .transpose()?,
        })
    }
    /// Allocate before decoding without sending a second image full of zeroes.
    /// The opaque reservation exposes an Image only after every requested plane
    /// has received a complete upload. Existing public zero-filled images keep
    /// using reserve_upload/prepare_upload.
    pub fn begin_upload(
        &self,
        source: Option<&Image>,
        size: Size,
        main: bool,
        province: bool,
    ) -> Result<crate::PendingUpload> {
        if size.width == 0
            || size.height == 0
            || size.width > i32::MAX as u32
            || size.height > i32::MAX as u32
            || size.rgba_bytes().is_none()
        {
            return Err(Error::Message("invalid upload reservation dimensions"));
        }
        if let Some(source) = source {
            self.check_image(source)?;
            if !main && source.size != size {
                return Err(Error::Message("province image size mismatch"));
            }
        }
        Ok(crate::PendingUpload {
            image: Image {
                size,
                canvas: false,
                text: !main && source.is_some_and(|image| image.text),
                device: self.device.clone(),
                main: if main {
                    Some(self.overwrite_plane(size, &self.resident)?)
                } else {
                    source.and_then(|s| s.main.clone())
                },
                province: province
                    .then(|| self.overwrite_plane(size, &self.resident))
                    .transpose()?,
            },
            main,
            province,
        })
    }
    pub(crate) fn canvas_storage(&self, size: Size, source: Option<&Image>) -> Size {
        // Text may move from a line into a paragraph with a different scene
        // offset. Their compact grids cannot preserve the same logical strokes
        // at every offset. Sample text only when composing the final scene.
        // Small canvases also commonly hold individual image-based glyphs.
        if self.canvas_limit.is_none()
            || source.is_some_and(|image| image.text)
            || (u64::from(size.width) * u64::from(size.height)
                <= u64::from(self.small_canvas_edge).pow(2)
                && size.width.max(size.height) <= self.small_canvas_edge.saturating_mul(4))
        {
            return size;
        }
        let scale = self.canvas_scale.get();
        let (sx, sy) = source
            .and_then(|image| {
                image.stored_size().map(|stored| {
                    if image.canvas && stored.width == 1 && stored.height == 1 {
                        // A uniform canvas uses one texel until its first edit;
                        // this is not a lower source-image sampling density.
                        return (1.0, 1.0);
                    }
                    (
                        f64::from(stored.width) / f64::from(image.size.width),
                        f64::from(stored.height) / f64::from(image.size.height),
                    )
                })
            })
            .unwrap_or((1.0, 1.0));
        Size {
            width: (f64::from(size.width) * sx.min(scale)).round().max(1.0) as u32,
            height: (f64::from(size.height) * sy.min(scale)).round().max(1.0) as u32,
        }
    }
    fn reserve_canvas(
        &self,
        size: Size,
        main: bool,
        province: bool,
        source: Option<&Image>,
    ) -> Result<Image> {
        // Validate logical dimensions even when their physical storage is small.
        if size.width == 0
            || size.height == 0
            || size.width > i32::MAX as u32
            || size.height > i32::MAX as u32
        {
            return Err(Error::Message("invalid logical image dimensions"));
        }
        let stored = self.canvas_storage(size, source);
        Ok(Image {
            size,
            canvas: true,
            text: source.is_some_and(|image| image.text),
            device: self.device.clone(),
            main: main
                // Both callers (create_image and resize) fill the entire main
                // plane before exposing it. Province can be copied partially.
                .then(|| self.overwrite_plane(stored, &self.resident))
                .transpose()?,
            province: province
                .then(|| self.plane(size, &self.resident))
                .transpose()?,
        })
    }
    pub fn create_image(&self, size: Size, color: u32) -> Result<Image> {
        let stored = self.solid_storage(size);
        if size.width != 0
            && size.height != 0
            && size.width <= i32::MAX as u32
            && size.height <= i32::MAX as u32
            && let Some(plane) = self
                .solid_images
                .borrow_mut()
                .get(self.canvas_storage(stored, None), color)
        {
            return Ok(Image {
                size,
                canvas: true,
                text: false,
                device: self.device.clone(),
                main: Some(plane),
                province: None,
            });
        }
        let mut image = self.reserve_canvas(stored, true, false, None)?;
        self.fill(
            &mut image,
            &[krkr_protocol::graphics::Fill {
                rectangle: stored.rect(),
                color,
                face: DrawFace::Alpha,
                hold_alpha: false,
            }],
        )?;
        image.size = size;
        Ok(image)
    }
    fn solid_storage(&self, size: Size) -> Size {
        if self.canvas_limit.is_some()
            && size.width > 0
            && size.height > 0
            && size.width <= i32::MAX as u32
            && size.height <= i32::MAX as u32
        {
            Size {
                width: 1,
                height: 1,
            }
        } else {
            size
        }
    }
    pub fn enable_image(&self, source: Option<&Image>, size: Size, color: u32) -> Result<Image> {
        let mut image = self.create_image(size, color)?;
        if let Some(source) = source {
            self.check_image(source)?;
            if source.has_province() {
                if source.size == size {
                    image.province = source.province.clone();
                } else {
                    image.province = Some(self.plane(size, &self.resident)?);
                    self.copy_rect(
                        &mut image,
                        source,
                        source.size.rect(),
                        0,
                        0,
                        size.rect(),
                        DrawFace::Province,
                        false,
                    )?;
                }
            }
        }
        Ok(image)
    }
    pub fn create_province(&self, size: Size) -> Result<Image> {
        self.reserve_upload(size, false, true)
    }
    pub fn prepare_upload(
        &self,
        source: &Image,
        size: Size,
        main: bool,
        province: bool,
    ) -> Result<Image> {
        self.check_image(source)?;
        if !main && size != source.size {
            return Err(Error::Message(
                "province upload dimensions differ from image",
            ));
        }
        let mut next = self.reserve_upload(size, main, province)?;
        if !main {
            next.main = source.main.clone();
        }
        Ok(next)
    }
    pub fn logical_image(&self, mut source: Image, logical: Size) -> Result<Image> {
        self.check_image(&source)?;
        if logical.width == 0
            || logical.height == 0
            || logical.width > i32::MAX as u32
            || logical.height > i32::MAX as u32
        {
            return Err(Error::Message("invalid logical image dimensions"));
        }
        if source.has_province() || !source.has_main() {
            return Err(Error::Message("compact image must have only a main plane"));
        }
        source.size = logical;
        source.canvas = true;
        Ok(source)
    }
    pub(crate) fn materialize(&self, image: &mut Image) -> Result<()> {
        if !image
            .main
            .as_ref()
            .is_some_and(|plane| plane.size != image.size)
        {
            return Ok(());
        }
        let mut next = self.reserve_full_upload(image.size)?;
        self.copy_rect(
            &mut next,
            image,
            image.size.rect(),
            0,
            0,
            image.size.rect(),
            DrawFace::Alpha,
            false,
        )?;
        image.main = next.main;
        Ok(())
    }
    /// Exact logical-pixel operations (province and neighborhood algorithms).
    /// Display-density draws use writable_compact instead.
    pub(crate) fn writable(
        &self,
        image: &mut Image,
        rectangle: Rect,
        province: bool,
    ) -> Result<()> {
        self.check_image(image)?;
        if !province {
            self.materialize(image)?;
        } else if image.province.is_none() {
            image.province = Some(self.plane(image.size, &self.resident)?);
        }
        self.detach_tiles(image, rectangle, province)
    }
    /// A complete upload supplies every pixel of this plane. Shared snapshots
    /// need new storage, but copying their old contents first serves no purpose.
    pub(crate) fn writable_upload(&self, image: &mut Image, province: bool) -> Result<()> {
        let old = image.plane(province)?;
        let replacement = if old.size != image.size {
            Some(self.allocate_plane(image.size, &old.budget, self.tile_edge, false, None)?)
        } else {
            None
        };
        let mut tiles = Vec::new();
        if replacement.is_none() {
            for (index, tile) in old.tiles.iter().enumerate() {
                if !tile.renderable()
                    || Rc::strong_count(old) > 1
                    || Rc::strong_count(&tile.texture) > 1
                {
                    tiles.push((index, self.device.sample_texture(tile.size(), &old.budget)?));
                }
            }
        }
        // Publish replacement storage only after every allocation succeeds.
        // A budget failure must leave the previous shared image readable.
        let slot = if province {
            &mut image.province
        } else {
            &mut image.main
        };
        if let Some(replacement) = replacement {
            *slot = Some(replacement);
            return Ok(());
        }
        if tiles.is_empty() {
            return Ok(());
        }
        let plane = Rc::make_mut(slot.as_mut().expect("validated upload plane"));
        for (index, texture) in tiles {
            plane.tiles[index].texture = texture;
            plane.tiles[index].backing = None;
        }
        Ok(())
    }
    pub(crate) fn writable_compact(
        &self,
        image: &mut Image,
        rectangle: Rect,
        province: bool,
    ) -> Result<()> {
        if province || !image.canvas || self.canvas_limit.is_none() {
            return self.writable(image, rectangle, province);
        }
        self.check_image(image)?;
        let stored = image.plane(false)?.size;
        let desired = self.canvas_storage(image.size, Some(image));
        if desired != stored {
            if let Some(plane) = self.partition_canvas(image, desired, rectangle)? {
                image.main = Some(plane);
            } else {
                let mut next = self.reserve_full_upload(desired)?;
                next.size = image.size;
                next.canvas = true;
                self.copy_rect(
                    &mut next,
                    image,
                    image.size.rect(),
                    0,
                    0,
                    image.size.rect(),
                    DrawFace::Alpha,
                    false,
                )?;
                image.main = next.main;
            }
        }
        let stored = image.plane(false)?.size;
        if self.write_solid_regions(image, rectangle, false)? {
            return Ok(());
        }
        if let Some(area) =
            crate::scene::raster::Raster::new(image.size, stored, (0, 0))?.rect(rectangle)
        {
            self.detach_canvas_bands(image, area)?;
            self.detach_tiles(image, area, false)?;
        }
        Ok(())
    }
    /// Fill never reads other pixels. A tile completely replaced by one of
    /// these clears needs new ownership, but no copy of its previous contents.
    pub(crate) fn writable_fill_main(
        &self,
        image: &mut Image,
        bounds: Rect,
        fills: &[Fill],
    ) -> Result<()> {
        self.check_image(image)?;
        if fills.len() == 1 && self.fill_cropped_regions(image, &fills[0])? {
            return Ok(());
        }
        if fills.len() == 1 && self.fill_solid_regions(image, &fills[0])? {
            return Ok(());
        }
        let desired = self.fill_main_size(image);
        let raster = crate::scene::raster::Raster::new(image.size, desired, (0, 0))?;
        let logical = image.size.rect();
        let area = |fill: &Fill| {
            fill.rectangle
                .intersection(logical)
                .and_then(|r| raster.rect(r))
        };
        let replaces = |rectangle: Rect| {
            fills.iter().any(|fill| {
                crate::fills::mask(fill) == [true; 4]
                    && area(fill).and_then(|r| r.intersection(rectangle)) == Some(rectangle)
            })
        };
        let touches = |rectangle: Rect| {
            fills
                .iter()
                .any(|fill| area(fill).and_then(|r| r.intersection(rectangle)).is_some())
        };
        let unchanged: Vec<Rect> = image
            .plane(false)?
            .tiles
            .iter()
            .filter(|t| {
                fills
                    .iter()
                    .all(|fill| crate::fills::unchanged(t, image.size, raster, fill))
            })
            .map(|t| t.rectangle)
            .collect();
        self.writable_main_regions(
            image,
            bounds,
            |r| touches(r) && !unchanged.contains(&r),
            replaces,
        )
    }
    /// A clipped, all-channel copy initializes the destination just like a
    /// clear. Preserve old pixels only in tiles partially covered by the copy.
    pub(crate) fn writable_copy_main(&self, image: &mut Image, bounds: Rect) -> Result<()> {
        self.check_image(image)?;
        if self.write_solid_regions(image, bounds, true)? {
            return Ok(());
        }
        let raster =
            crate::scene::raster::Raster::new(image.size, self.fill_main_size(image), (0, 0))?;
        let area = raster.rect(bounds);
        self.writable_main_regions(
            image,
            bounds,
            |r| area.and_then(|a| a.intersection(r)).is_some(),
            |r| area.and_then(|a| a.intersection(r)) == Some(r),
        )
    }
    fn writable_main_regions(
        &self,
        image: &mut Image,
        bounds: Rect,
        touches: impl Fn(Rect) -> bool,
        replaces: impl Fn(Rect) -> bool,
    ) -> Result<()> {
        let desired = self.fill_main_size(image);
        let old = image.plane(false)?;
        if desired != old.size {
            if replaces(desired.rect()) {
                image.main =
                    Some(self.allocate_plane(desired, &old.budget, self.tile_edge, false, None)?);
                return Ok(());
            }
            // Partial writes still need the original resampling to preserve
            // pixels outside the clear and any channels excluded by its mask.
            return self.writable_compact(image, bounds, false);
        }
        let mut replacements = Vec::new();
        for (index, tile) in old.tiles.iter().enumerate() {
            if (!tile.renderable()
                || Rc::strong_count(old) > 1
                || Rc::strong_count(&tile.texture) > 1)
                && touches(tile.rectangle)
            {
                let next = self.device.sample_texture(tile.size(), &old.budget)?;
                if !replaces(tile.rectangle) {
                    self.copy_tile_storage(tile, &next, &old.budget)?;
                }
                replacements.push((index, next));
            }
        }
        // Allocate/copy every replacement before publishing any uninitialized
        // tile. Allocation failure leaves both the image and its aliases intact.
        if !replacements.is_empty() {
            let plane = Rc::make_mut(image.main.as_mut().expect("validated main plane"));
            for (index, texture) in replacements {
                plane.tiles[index].texture = texture;
                plane.tiles[index].backing = None;
            }
        }
        Ok(())
    }
    fn detach_tiles(&self, image: &mut Image, rectangle: Rect, province: bool) -> Result<()> {
        let plane = if province {
            &mut image.province
        } else {
            &mut image.main
        };
        let plane = Rc::make_mut(
            plane
                .as_mut()
                .ok_or(Error::Message("image plane is absent"))?,
        );
        for tile in &mut plane.tiles {
            if tile.rectangle.intersection(rectangle).is_some()
                && (!tile.renderable() || Rc::strong_count(&tile.texture) > 1)
            {
                // The full copy initializes every texel before publishing it.
                // Clearing first adds a zero upload and a work-surface switch.
                let allocation = self.device.sample_texture(tile.size(), &plane.budget)?;
                self.copy_tile_storage(tile, &allocation, &plane.budget)?;
                tile.texture = allocation;
                tile.backing = None;
            }
        }
        Ok(())
    }
    pub fn independ(&self, image: &mut Image, province: bool, copy: bool) -> Result<()> {
        self.check_image(image)?;
        let existing = if province {
            &image.province
        } else {
            &image.main
        };
        let Some(existing) = existing else {
            return Ok(());
        };
        let plane = if copy {
            let mut tiles = Vec::with_capacity(existing.tiles.len());
            for old in &existing.tiles {
                // An independent constant tile still needs only one texel;
                // later writes expand its exact rectangle before drawing.
                let texture = self.device.sample_texture(
                    if old.backing.is_some() {
                        old.size()
                    } else {
                        old.texture.size
                    },
                    &self.resident,
                )?;
                self.copy_tile_storage(old, &texture, &self.resident)?;
                tiles.push(Tile {
                    backing: None,
                    rectangle: old.rectangle,
                    texture,
                });
            }
            Rc::new(Plane {
                size: existing.size,
                budget: self.resident.clone(),
                tiles,
            })
        } else {
            self.allocate_plane(image.size, &self.resident, self.tile_edge, true, None)?
        };
        if province {
            image.province = Some(plane);
        } else {
            image.main = Some(plane);
        }
        Ok(())
    }
    pub fn resize(&self, source: &Image, size: Size, color: u32) -> Result<Image> {
        let _profile = krkr_protocol::profile::span_detail("gpu.resize", || {
            format!(
                "logical={:?} stored={:?} next={size:?}",
                source.size,
                source.stored_size()
            )
        });
        self.check_image(source)?;
        if source.size == size {
            return Ok(source.shared());
        }
        let uniform = self.solid_images.borrow().color(source) == Some(color);
        if uniform && self.canvas_limit.is_some() && !source.has_province() {
            return self.create_image(size, color);
        }
        if let Some(stored) = self.shared_resize_storage(source, size) {
            return self.grow_shared_canvas(source, size, stored, color);
        }
        if source.has_main()
            && !source.has_province()
            && self.canvas_limit.is_some()
            && let Some(bounds) = source.size.rect().intersection(size.rect())
        {
            let mut next = self.create_image(size, color)?;
            next.text = source.text;
            if let Some(plane) =
                self.partition_canvas(&next, self.canvas_storage(size, Some(source)), bounds)?
            {
                next.main = Some(plane);
                self.copy_rect(
                    &mut next,
                    source,
                    bounds,
                    0,
                    0,
                    size.rect(),
                    DrawFace::Alpha,
                    false,
                )?;
                return Ok(next);
            }
        }
        let shared = if uniform {
            self.solid_images
                .borrow_mut()
                .get(self.canvas_storage(size, Some(source)), color)
        } else {
            None
        };
        let mut next = self.reserve_canvas(
            size,
            source.has_main() && shared.is_none(),
            source.has_province(),
            Some(source),
        )?;
        if let Some(plane) = shared {
            next.main = Some(plane);
        } else if source.has_main() {
            // This resize already allocated the final density to preserve
            // source pixels and padding. Initialize it directly, without
            // replacing it with a virtual solid and allocating it a second time.
            self.fill_main_batch(
                &mut next,
                &[krkr_protocol::graphics::Fill {
                    rectangle: size.rect(),
                    color,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }],
            )?;
            if !uniform {
                self.copy_rect(
                    &mut next,
                    source,
                    source.size.rect(),
                    0,
                    0,
                    size.rect(),
                    DrawFace::Alpha,
                    false,
                )?;
            }
        }
        if source.has_province() {
            self.copy_rect(
                &mut next,
                source,
                source.size.rect(),
                0,
                0,
                size.rect(),
                DrawFace::Province,
                false,
            )?;
        }
        Ok(next)
    }
}

//! Immutable generated main planes. Logical pixels survive clipping; only the
//! requested region is cached as a texture. Source owners enforce versioned COW.
use crate::{
    blend::{PARAMETER_BYTES, Parameters},
    copy::{ImageSource, copy},
    gpu::{Allocation, FORMAT, Gpu, Image, rgba},
};
use krkr_protocol::{
    graphics::{Rect, Size},
    transform::{Filter, ImageOperation, Sampling, Transform},
};
use krkr_render::{
    Error, Result,
    budget::Permit,
    transform::{Mapping, validate_source},
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wgpu::util::DeviceExt;

// Metadata has a fixed depth bound. Checkpointing uses the ordinary admitted
// raster path; full replacements discard their predecessor instead of growing
// an operation log. This threshold is a storage policy, not a logical size cap.
const MAX_DEPTH: usize = 16;
const DENSE_PIXELS: u64 = 2048 * 2048;
const TILE: u32 = 256;

pub(crate) enum Content {
    Solid(u32),
    Resize {
        source: Image,
        preserved: Rect,
        color: u32,
    },
    Affine {
        source: Image,
        rectangle: Rect,
        bounds: Rect,
        mapping: Mapping,
        linear: bool,
        color: u32,
    },
    Tiles {
        source: Image,
        tiles: BTreeMap<(u32, u32), Image>,
        _permit: Permit,
    },
}
pub(crate) struct Deferred {
    content: Content,
    depth: usize,
    cache: Mutex<Option<Region>>,
    _permit: Permit,
}
#[derive(Clone)]
pub(crate) struct Region {
    pub allocation: Arc<Allocation>,
    pub owners: Arc<()>,
    /// Logical coordinates covered by this physical allocation.
    pub rectangle: Rect,
}
impl Image {
    pub(crate) fn prefers_deferred(size: Size) -> bool {
        u64::from(size.width) * u64::from(size.height) > DENSE_PIXELS
    }
    pub(crate) fn has_main(&self) -> bool {
        self.main.is_some() || self.deferred.is_some()
    }
    fn depth(&self) -> usize {
        self.deferred.as_ref().map_or(0, |d| d.depth)
    }
    pub fn create_write_bytes(size: Size) -> usize {
        if Self::prefers_deferred(size) {
            0
        } else {
            size.rgba_bytes().unwrap_or(usize::MAX)
        }
    }
    pub fn transform_write_bytes(
        &self,
        transform: Transform,
        operation: ImageOperation,
        clip: Rect,
        clear: Option<u32>,
    ) -> usize {
        if self.defers_transform(transform, operation, clip, clear) {
            0
        } else {
            self.main_write_bytes()
        }
    }
    fn defers_transform(
        &self,
        transform: Transform,
        operation: ImageOperation,
        clip: Rect,
        clear: Option<u32>,
    ) -> bool {
        Self::prefers_deferred(self.size)
            && matches!(transform, Transform::Affine(_))
            && matches!(operation, ImageOperation::Copy { hold_alpha: false })
            && clear.is_some()
            && clip.intersection(self.size.rect()) == Some(self.size.rect())
    }
}
impl ImageSource {
    pub(crate) fn snapshot_main(&self) -> Image {
        let mut image = Image::new(self.main.clone(), None, self.size);
        image.deferred = self.deferred.clone();
        image.main_owners = self.main_owners.upgrade().unwrap_or_default();
        image
    }
}
impl Gpu {
    /// Keep the offline texture at its stored size. Compressed textures can
    /// round up to a power of two and exceed the logical image in one axis;
    /// existing region resolution still uses logical coordinates.
    pub fn logical_image(&self, source: Image, size: Size) -> Result<Image> {
        if source.size == size {
            return Ok(source);
        }
        if source.has_province() {
            return Err(Error::Message(
                "invalid compact image dimensions or province plane",
            ));
        }
        let rectangle = source.size.rect();
        let transform = Transform::Stretch(krkr_protocol::transform::StretchRect {
            left: 0,
            top: 0,
            width: size.width as i32,
            height: size.height as i32,
        });
        let mapping = Mapping::new(rectangle, transform, size.rect())?
            .ok_or(Error::Message("empty compact image mapping"))?;
        self.mixer()?;
        self.generated(
            size,
            Content::Affine {
                source,
                rectangle,
                bounds: rectangle,
                mapping,
                linear: false,
                color: 0,
            },
            2,
        )
    }

    fn generated(&self, size: Size, content: Content, depth: usize) -> Result<Image> {
        let permit = self.staging.reserve(std::mem::size_of::<Deferred>() + 64)?;
        let deferred = Arc::new(Deferred {
            content,
            depth,
            cache: Mutex::new(None),
            _permit: permit,
        });
        let mut registry = self.generated.lock().unwrap();
        registry.retain(|d| d.strong_count() != 0);
        // Index capacity (including Vec growth) is charged in each recipe's
        // metadata permit. It holds no content owners and prunes dead versions.
        registry
            .try_reserve(1)
            .map_err(|_| Error::Message("generated image index allocation failed"))?;
        registry.push(Arc::downgrade(&deferred));
        let mut image = Image::new(None, None, size);
        image.deferred = Some(deferred);
        Ok(image)
    }
    pub(crate) fn trim_generated(&self) {
        self.generated.lock().unwrap().retain(|weak| {
            if let Some(image) = weak.upgrade() {
                image.cache.lock().unwrap().take();
                true
            } else {
                false
            }
        });
    }
    pub(crate) fn generated_solid(&self, size: Size, color: u32) -> Result<Image> {
        self.generated(size, Content::Solid(color), 1)
    }
    pub(crate) fn replace_solid(&self, image: &mut Image, color: u32) -> Result<()> {
        let replacement = self.generated_solid(image.size, color)?;
        image.main = None;
        image.deferred = replacement.deferred;
        image.main_owners = replacement.main_owners;
        Ok(())
    }
    fn bounded_source(&self, mut source: Image) -> Result<Image> {
        if source.depth() >= MAX_DEPTH {
            self.materialize(&mut source)?;
        }
        Ok(source)
    }
    pub(crate) fn defer_resize(&self, image: &mut Image, size: Size, color: u32) -> Result<bool> {
        if image.has_province() || !(image.deferred.is_some() || Image::prefers_deferred(size)) {
            return Ok(false);
        }
        if !image.has_main() {
            return Err(Error::Message("image has no main plane"));
        }
        let overlap = Size {
            width: size.width.min(image.size.width),
            height: size.height.min(image.size.height),
        }
        .rect();
        let (source, preserved) = match image.deferred.as_deref().map(|d| &d.content) {
            Some(Content::Solid(old)) if *old == color => {
                *image = self.generated_solid(size, color)?;
                return Ok(true);
            }
            Some(Content::Resize {
                source,
                preserved,
                color: old,
            }) if *old == color => (
                source.shared_main(),
                preserved.intersection(overlap).expect("origin overlap"),
            ),
            _ => (image.shared_main(), overlap),
        };
        let source = self.bounded_source(source)?;
        let depth = source.depth() + 1;
        *image = self.generated(
            size,
            Content::Resize {
                source,
                preserved,
                color,
            },
            depth,
        )?;
        Ok(true)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn defer_transform(
        &self,
        image: &mut Image,
        source: &ImageSource,
        rectangle: Rect,
        transform: Transform,
        sampling: Sampling,
        operation: ImageOperation,
        clip: Rect,
        clear: Option<u32>,
    ) -> Result<bool> {
        if !image.defers_transform(transform, operation, clip, clear)
            || rectangle.width == 0
            || rectangle.height == 0
        {
            return Ok(false);
        }
        if !image.has_main() || (source.main.is_none() && source.deferred.is_none()) {
            return Err(Error::Message("affine image has no main plane"));
        }
        validate_source(rectangle, source.size)?;
        let color = clear.expect("full affine clear");
        let generated =
            if let Some(mapping) = Mapping::new(rectangle, transform, image.size.rect())? {
                let source = self.bounded_source(source.snapshot_main())?;
                let depth = source.depth() + 1;
                let bounds = if sampling.no_clip {
                    source.size.rect()
                } else {
                    rectangle
                };
                // Prepare the exact kernel at the originating call, before admission.
                self.mixer()?;
                self.generated(
                    image.size,
                    Content::Affine {
                        source,
                        rectangle,
                        bounds,
                        mapping,
                        linear: sampling.filter != Filter::Nearest,
                        color,
                    },
                    depth,
                )?
            } else {
                self.generated_solid(image.size, color)?
            };
        image.main = generated.main;
        image.deferred = generated.deferred;
        image.main_owners = generated.main_owners;
        Ok(true)
    }
    /// Resolve only requested logical pixels; cached coverage may be larger.
    pub(crate) fn resolve_main(&self, image: &Image, rectangle: Rect) -> Result<Region> {
        validate_source(rectangle, image.size)?;
        if let Some(main) = &image.main {
            return Ok(Region {
                allocation: main.clone(),
                owners: image.main_owners.clone(),
                rectangle: image.size.rect(),
            });
        }
        let deferred = image
            .deferred
            .as_ref()
            .ok_or(Error::Message("image has no main plane"))?;
        {
            let mut cache = deferred.cache.lock().unwrap();
            if let Some(region) = cache
                .as_ref()
                .filter(|r| r.rectangle.intersection(rectangle) == Some(rectangle))
            {
                return Ok(region.clone());
            }
            cache.take();
        }
        let size = Size {
            width: rectangle.width,
            height: rectangle.height,
        };
        let allocation = self.allocation(size, FORMAT, &self.resident)?;
        match &deferred.content {
            Content::Solid(color) => {
                let mut encoder = self.device.create_command_encoder(&Default::default());
                self.clear(&mut encoder, &allocation, rgba(*color));
                self.submit(encoder, allocation.clone());
            }
            Content::Resize {
                source,
                preserved,
                color,
            } => {
                let input = preserved
                    .intersection(rectangle)
                    .map(|r| self.resolve_main(source, r).map(|s| (r, s)))
                    .transpose()?;
                let mut encoder = self.device.create_command_encoder(&Default::default());
                self.clear(&mut encoder, &allocation, rgba(*color));
                if let Some((area, input)) = &input {
                    copy(
                        &mut encoder,
                        &input.allocation,
                        &allocation,
                        Rect {
                            left: area.left - input.rectangle.left,
                            top: area.top - input.rectangle.top,
                            ..*area
                        },
                        (area.left - rectangle.left) as u32,
                        (area.top - rectangle.top) as u32,
                    );
                }
                self.submit(
                    encoder,
                    (allocation.clone(), input.map(|(_, s)| s.allocation)),
                );
            }
            Content::Affine {
                source,
                rectangle: source_rect,
                bounds,
                mapping,
                linear,
                color,
            } => {
                if let Some(visible) = mapping.bounds.intersection(rectangle) {
                    let footprint = Mapping {
                        inverse: mapping.inverse,
                        bounds: visible,
                    }
                    .source_region(*source_rect, *bounds, *linear);
                    let input = self.resolve_main(source, footprint)?;
                    let destination = Rect {
                        left: visible.left - rectangle.left,
                        top: visible.top - rectangle.top,
                        ..visible
                    };
                    let mut p = Parameters::affine(
                        destination,
                        mapping,
                        *source_rect,
                        *bounds,
                        *linear,
                        ImageOperation::Copy { hold_alpha: false },
                        Some(*color),
                    );
                    p.0[0] = input.rectangle.left;
                    p.0[1] = input.rectangle.top;
                    p.0[15] = (rectangle.left as f32).to_bits() as i32;
                    p.0[19] = (rectangle.top as f32).to_bits() as i32;
                    let permit = self.staging.reserve(PARAMETER_BYTES * 2)?;
                    let buffer =
                        self.device
                            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                                label: Some("generated affine region"),
                                contents: bytemuck::cast_slice(&p.0),
                                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::STORAGE,
                            });
                    let mut encoder = self.device.create_command_encoder(&Default::default());
                    self.clear(&mut encoder, &allocation, rgba(*color));
                    self.mixer()?.draw(
                        self,
                        &mut encoder,
                        &allocation,
                        &input.allocation,
                        None,
                        &buffer,
                        0,
                        destination,
                        false,
                        None,
                    );
                    self.submit(encoder, (allocation.clone(), input.allocation, permit));
                } else {
                    let mut encoder = self.device.create_command_encoder(&Default::default());
                    self.clear(&mut encoder, &allocation, rgba(*color));
                    self.submit(encoder, allocation.clone());
                }
            }
            Content::Tiles { source, tiles, .. } => {
                let base = self.resolve_main(source, rectangle)?;
                let mut encoder = self.device.create_command_encoder(&Default::default());
                copy(
                    &mut encoder,
                    &base.allocation,
                    &allocation,
                    Rect {
                        left: rectangle.left - base.rectangle.left,
                        top: rectangle.top - base.rectangle.top,
                        ..rectangle
                    },
                    0,
                    0,
                );
                let mut pins = vec![base.allocation];
                for (&(x, y), tile) in tiles {
                    let origin = (x * TILE, y * TILE);
                    let area = Rect {
                        left: origin.0 as i32,
                        top: origin.1 as i32,
                        ..tile.size.rect()
                    };
                    if let Some(part) = rectangle.intersection(area) {
                        copy(
                            &mut encoder,
                            tile.main()?,
                            &allocation,
                            Rect {
                                left: part.left - area.left,
                                top: part.top - area.top,
                                ..part
                            },
                            (part.left - rectangle.left) as u32,
                            (part.top - rectangle.top) as u32,
                        );
                        pins.push(tile.main()?.clone());
                    }
                }
                self.submit(encoder, (allocation.clone(), pins));
            }
        }
        self.check()?;
        let region = Region {
            allocation,
            owners: Arc::new(()),
            rectangle,
        };
        *deferred.cache.lock().unwrap() = Some(region.clone());
        Ok(region)
    }
    /// Kernels which really consume/write the full plane retain the ordinary
    /// exact raster path and its resource admission; no pixels are discarded.
    pub(crate) fn materialize(&self, image: &mut Image) -> Result<()> {
        if image.deferred.is_none() {
            return Ok(());
        }
        let region = self.resolve_main(image, image.size.rect())?;
        // Transfer cached storage into the writable version. Other recipes can
        // regenerate it; actual frozen raster owners still trigger normal COW.
        image
            .deferred
            .as_ref()
            .unwrap()
            .cache
            .lock()
            .unwrap()
            .take();
        image.main = Some(region.allocation);
        image.main_owners = region.owners;
        image.deferred = None;
        self.independent(image, true, false)
    }
    pub(crate) fn materialized_source(&self, source: &ImageSource) -> Result<Option<ImageSource>> {
        if source.deferred.is_none() {
            return Ok(None);
        }
        let region = self.resolve_main(&source.snapshot_main(), source.size.rect())?;
        let mut image = Image::new(
            Some(region.allocation),
            source.province.clone(),
            source.size,
        );
        image.main_owners = region.owners;
        Ok(Some(image.source()))
    }
    /// Raster writes override fixed tiles, not a growing command history. An
    /// existing tile is replaced; copies of the image keep their own versions.
    pub(crate) fn edit_generated(
        &self,
        image: &mut Image,
        rectangle: Rect,
        mut edit: impl FnMut(&mut Image, Rect, (i32, i32)) -> Result<()>,
    ) -> Result<bool> {
        let Some(deferred) = &image.deferred else {
            return Ok(false);
        };
        let Some(area) = rectangle.intersection(image.size.rect()) else {
            return Ok(true);
        };
        let old_tiles = match &deferred.content {
            Content::Tiles { tiles, .. } => Some(tiles),
            _ => None,
        };
        let xs = area.left as u32 / TILE..(area.left as u32 + area.width).div_ceil(TILE);
        let ys = area.top as u32 / TILE..(area.top as u32 + area.height).div_ceil(TILE);
        let added = ys
            .clone()
            .flat_map(|y| xs.clone().map(move |x| (x, y)))
            .filter(|key| old_tiles.is_none_or(|tiles| !tiles.contains_key(key)))
            .count();
        let count = old_tiles.map_or(0, BTreeMap::len) + added;
        // BTree nodes and logical plane references, including cloning overhead.
        let permit = self
            .staging
            .reserve(count * (std::mem::size_of::<Image>() + 128) * 2)?;
        let (source, mut tiles) = match &deferred.content {
            Content::Tiles { source, tiles, .. } => (
                source.shared_main(),
                tiles
                    .iter()
                    .map(|(key, tile)| (*key, tile.shared_main()))
                    .collect(),
            ),
            _ => (self.bounded_source(image.shared_main())?, BTreeMap::new()),
        };
        for y in ys {
            for x in xs.clone() {
                let origin = (x * TILE, y * TILE);
                let size = Size {
                    width: TILE.min(image.size.width - origin.0),
                    height: TILE.min(image.size.height - origin.1),
                };
                let tile_rect = Rect {
                    left: origin.0 as i32,
                    top: origin.1 as i32,
                    ..size.rect()
                };
                let input = self.resolve_main(image, tile_rect)?;
                let main = self.allocation(size, FORMAT, &self.resident)?;
                let mut encoder = self.device.create_command_encoder(&Default::default());
                copy(
                    &mut encoder,
                    &input.allocation,
                    &main,
                    Rect {
                        left: tile_rect.left - input.rectangle.left,
                        top: tile_rect.top - input.rectangle.top,
                        ..tile_rect
                    },
                    0,
                    0,
                );
                self.submit(encoder, (input.allocation, main.clone()));
                let mut tile = Image::new(Some(main), None, size);
                let part = area.intersection(tile_rect).expect("covered tile");
                edit(
                    &mut tile,
                    Rect {
                        left: part.left - tile_rect.left,
                        top: part.top - tile_rect.top,
                        ..part
                    },
                    (tile_rect.left, tile_rect.top),
                )?;
                tiles.insert((x, y), tile);
            }
        }
        let depth = source.depth() + 1;
        let next = self.generated(
            image.size,
            Content::Tiles {
                source,
                tiles,
                _permit: permit,
            },
            depth,
        )?;
        image.main = None;
        image.deferred = next.deferred;
        image.main_owners = next.main_owners;
        Ok(true)
    }
}

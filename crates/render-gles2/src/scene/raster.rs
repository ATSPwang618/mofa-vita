use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{Draw, Sample, face},
    image::{Plane, Tile},
};
use krkr_protocol::graphics::{Blend, BlendOptions, DrawFace, Rect, Size};
use std::rc::Rc;

#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[path = "../../tests/internal/scene_sampling.rs"]
mod tests;

/// Script coordinates stay integral and unchanged. Only raster storage and
/// pixel coverage use the display size; every group shares the same grid.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Raster {
    logical: Size,
    physical: Size,
    origin: (i32, i32),
}
impl Raster {
    pub fn new(logical: Size, physical: Size, origin: (i32, i32)) -> Result<Self> {
        if [
            logical.width,
            logical.height,
            physical.width,
            physical.height,
        ]
        .iter()
        .any(|&v| v == 0 || v > i32::MAX as u32)
        {
            return Err(Error::Message("invalid scene raster dimensions"));
        }
        Ok(Self {
            logical,
            physical,
            origin,
        })
    }
    pub fn rect(self, area: Rect) -> Option<Rect> {
        let edge = |at: i64, origin: i32, logical: u32, physical: u32| {
            // A physical pixel belongs to a logical interval iff its center
            // samples that interval. Round both shared edges identically.
            let offset = at - i64::from(origin);
            if offset <= 0 {
                return 0;
            }
            if offset >= i64::from(logical) {
                return physical as i32;
            }
            if logical == physical {
                return offset as i32;
            }
            // ceil((2*p-logical)/(2*logical)) equals
            // floor((p+(logical-1)/2)/logical) for integral p. Clipping
            // first bounds the product by the validated i32 dimensions.
            ((offset as u64 * u64::from(physical) + u64::from((logical - 1) / 2))
                / u64::from(logical)) as i32
        };
        let x = |v| edge(v, self.origin.0, self.logical.width, self.physical.width);
        let y = |v| edge(v, self.origin.1, self.logical.height, self.physical.height);
        let (left, top) = (x(i64::from(area.left)), y(i64::from(area.top)));
        let right = x(i64::from(area.left) + i64::from(area.width));
        let bottom = y(i64::from(area.top) + i64::from(area.height));
        (left < right && top < bottom).then_some(Rect {
            left,
            top,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        })
    }
    pub fn clip(self) -> Rect {
        Rect {
            left: self.origin.0,
            top: self.origin.1,
            ..self.logical.rect()
        }
    }
    pub fn extent(self, logical: Size) -> Size {
        let axis = |value: u32, n: u32, d: u32| {
            if n >= d {
                return value.max(1);
            }
            ((u64::from(value) * u64::from(n)).div_ceil(u64::from(d)))
                .clamp(1, u64::from(value.min(n).max(1))) as u32
        };
        Size {
            width: axis(logical.width, self.physical.width, self.logical.width),
            height: axis(logical.height, self.physical.height, self.logical.height),
        }
    }
    pub(crate) fn mapping(self, output: (i64, i64), source: (i64, i64)) -> [f32; 6] {
        let sx = f64::from(self.logical.width) / f64::from(self.physical.width);
        let sy = f64::from(self.logical.height) / f64::from(self.physical.height);
        [
            sx as f32,
            0.,
            ((output.0 as f64 + 0.5) * sx + f64::from(self.origin.0) - source.0 as f64 - 0.5)
                as f32,
            0.,
            sy as f32,
            ((output.1 as f64 + 0.5) * sy + f64::from(self.origin.1) - source.1 as f64 - 0.5)
                as f32,
        ]
    }
}

// Scene sampling maps directly to stored texel centers. Quantizing through
// the logical grid first loses thin strokes even for pre-scaled UI images.
pub(crate) fn stored_mapping(source: &Image, mut map: [f32; 6]) -> Result<[f32; 6]> {
    let stored = source.plane(false)?.size;
    for (offset, scale) in [
        (0, stored.width as f64 / source.size.width as f64),
        (3, stored.height as f64 / source.size.height as f64),
    ] {
        map[offset] = (f64::from(map[offset]) * scale) as f32;
        map[offset + 1] = (f64::from(map[offset + 1]) * scale) as f32;
        map[offset + 2] = ((f64::from(map[offset + 2]) + 0.5) * scale - 0.5) as f32;
    }
    Ok(map)
}
fn needs_filter(map: [f32; 6]) -> bool {
    (map[0] - 1.).abs() > 0.000001
        || (map[4] - 1.).abs() > 0.000001
        || (map[2] - map[2].round()).abs() > 0.0001
        || (map[5] - map[5].round()).abs() > 0.0001
}
impl Raster {
    pub(crate) fn bitmap_filtered(
        self,
        source: &Image,
        output: (i64, i64),
        origin: (i64, i64),
    ) -> Result<bool> {
        Ok(needs_filter(stored_mapping(
            source,
            self.mapping(output, origin),
        )?))
    }
}
impl Gpu {
    pub(crate) fn sharpen_bitmap(&self, source: &Image, mapping: [f32; 6]) -> bool {
        self.effect_sharpen
            && source.canvas
            && !source.text
            && mapping[0].hypot(mapping[3]) < 0.999
            && mapping[1].hypot(mapping[4]) < 0.999
    }

    /// Use exactly the sampling and rounding proof used by the draw itself.
    /// Geometric coverage alone is insufficient at fractional compact edges.
    pub(crate) fn scene_bitmap_overwrites(
        &self,
        source: &Image,
        area: Rect,
        raster: Raster,
        output_origin: (i64, i64),
        source_origin: (i64, i64),
    ) -> Result<bool> {
        let plane = source.plane(false)?;
        let mapping = stored_mapping(source, raster.mapping(output_origin, source_origin))?;
        let mut draw = Draw::copy(mapping, [true; 4]);
        if needs_filter(mapping) {
            draw.sampling = Some(Sample {
                region: plane.size.rect(),
                bounds: plane.size.rect(),
                linear: true,
                display: true,
                sharpen: false,
                clear: false,
                scale: None,
            });
        }
        Ok(crate::drawing::overwrites(&draw, Some(plane), area))
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn scene_bitmap(
        &self,
        target: &mut Image,
        source: &Image,
        area: Rect,
        raster: Raster,
        output_origin: (i64, i64),
        source_origin: (i64, i64),
        options: BlendOptions,
        raw: bool,
    ) -> Result<()> {
        self.writable(target, area, false)?;
        let draw = Draw {
            kind: if raw {
                0.
            } else if options.mode == Blend::Opaque && options.opacity == 255 {
                if options.face == DrawFace::Opaque {
                    0.
                } else {
                    6.
                }
            } else {
                // Display composition can use the fixed blend unit. Script
                // image operations retain the legacy byte-domain arithmetic.
                // This avoids copying the destination out of the tile buffer
                // just to draw ordinary text/UI over a moving background.
                self.display_blend_kind(options).unwrap_or(1.)
            },
            color: [0.; 4],
            operation: [
                options.mode as i32 as f32,
                face(options.face),
                f32::from(options.opacity),
                f32::from(options.hold_alpha),
            ],
            mask: [true; 4],
            mapping: raster.mapping(output_origin, source_origin),
            sampling: None,
        };
        self.display_bitmap(target.plane(false)?, source, area, draw)
    }
    fn display_bitmap(
        &self,
        target: &Plane,
        source: &Image,
        area: Rect,
        mut draw: Draw,
    ) -> Result<()> {
        draw.mapping = stored_mapping(source, draw.mapping)?;
        if !needs_filter(draw.mapping) {
            return self.draw(target, Some(source.plane(false)?), area, &draw);
        }
        // The affine partitioner gathers only tile seam neighborhoods. Each
        // bilinear footprint must see adjacent texels, never a clamped seam.
        let mut stored = source.shared_main();
        stored.size = source.plane(false)?.size;
        draw.sampling = Some(Sample {
            region: stored.size.rect(),
            bounds: stored.size.rect(),
            linear: true,
            display: true,
            sharpen: false,
            clear: false,
            scale: None,
        });
        draw.sampling.as_mut().unwrap().sharpen = self.sharpen_bitmap(source, draw.mapping);
        let plane = stored.plane(false)?;
        // Gather the displayed footprint once instead of sampling every seam.
        // A narrow composition band must not copy the whole source canvas.
        if plane.tiles.len() >= 8
            && plane.size.width <= self.tile_edge
            && plane.size.height <= self.tile_edge
            && plane
                .size
                .rgba_bytes()
                .is_some_and(|bytes| bytes <= 2 * 1024 * 1024)
        {
            let (_, footprint) =
                crate::transform::affine_footprint(&draw, area, plane.size, [1., 1.]);
            if footprint.width != 0
                && footprint.height != 0
                && ((Size {
                    width: footprint.width,
                    height: footprint.height,
                })
                .rgba_bytes()
                .is_some_and(|bytes| bytes <= self.scratch_capacity())
                    || self.flattened_images.borrow().covers(&stored, footprint))
            {
                let input = self.display_gather(&stored, footprint)?;
                draw.sampling.as_mut().unwrap().scale = Some([1., 1.]);
                return self.draw(target, Some(&input), area, &draw);
            }
        }
        self.affine_parts(target, &stored, area, &draw, &mut None)
    }

    fn display_gather(&self, source: &Image, area: Rect) -> Result<Plane> {
        let cached = self.flattened_images.borrow_mut().take(source, area);
        if let Some(mut entry) = cached {
            if entry.source.matches(source) {
                let _profile = krkr_protocol::profile::span("gpu.scene.flatten.reuse");
                let plane = entry.plane.clone();
                self.flattened_images.borrow_mut().put(entry);
                return Ok(plane);
            }
            let _profile = krkr_protocol::profile::span("gpu.scene.flatten.update");
            let version = crate::scene_damage::Version::capture(source)?;
            if let Some(area) = version
                .damage(&entry.source)
                .and_then(|damage| damage.intersection(entry.plane.tiles[0].rectangle))
            {
                // The cached input is sampled immediately, never published as
                // an image. GL orders earlier reads before this partial write.
                let mut target = entry.plane.clone();
                let tile = &mut target.tiles[0];
                tile.rectangle = Rect {
                    left: tile.rectangle.left,
                    top: tile.rectangle.top,
                    ..tile.texture.size.rect()
                };
                tile.backing = None;
                self.draw(
                    &target,
                    Some(source.plane(false)?),
                    area,
                    &Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
                )?;
            }
            entry.source = version;
            let plane = entry.plane.clone();
            self.flattened_images.borrow_mut().put(entry);
            return Ok(plane);
        }
        let _profile = krkr_protocol::profile::span("gpu.scene.flatten");
        let source_plane = source.plane(false)?;
        let bytes = Size {
            width: area.width,
            height: area.height,
        }
        .rgba_bytes()
        .unwrap();
        self.flattened_images.borrow_mut().make_room(bytes);
        // Retained gathers are expendable when another surface needs the budget.
        if bytes > self.scratch_capacity() {
            self.flattened_images.borrow_mut().clear();
        }
        let plane = self.gather_affine(source_plane, source_plane.size, area, &mut None)?;
        if let Some(entry) = crate::scene_flatten::Entry::new(source, plane.clone(), &self.staging)
        {
            self.flattened_images.borrow_mut().put(entry);
        }
        Ok(plane)
    }

    pub(crate) fn gather_raster(
        &self,
        source: &Image,
        area: Rect,
        raster: Raster,
        province: bool,
        scratch: &mut Option<Rc<Texture>>,
    ) -> Result<Plane> {
        if scratch.as_ref().is_none_or(|texture| {
            texture.size.width < area.width || texture.size.height < area.height
        }) {
            let mut size = Size {
                width: area.width,
                height: area.height,
            };
            if let Some(old) = scratch.take() {
                size.width = size.width.max(old.size.width);
                size.height = size.height.max(old.size.height);
            }
            *scratch = Some(self.device.sample_texture(size, &self.scratch)?);
        }
        let texture = scratch.as_ref().unwrap().clone();
        let target = Plane {
            size: raster.physical,
            budget: self.scratch.clone(),
            tiles: vec![Tile {
                backing: None,
                rectangle: Rect {
                    width: texture.size.width,
                    height: texture.size.height,
                    ..area
                },
                texture,
            }],
        };
        let input = source.plane(province)?;
        let mut draw = Draw::copy(raster.mapping((0, 0), (0, 0)), [true; 4]);
        if !province {
            self.display_bitmap(&target, source, area, draw)?;
            return Ok(target);
        }
        draw.sampling = Some(Sample {
            region: source.size.rect(),
            bounds: source.size.rect(),
            linear: false,
            display: false,
            sharpen: false,
            clear: false,
            scale: Some([
                input.size.width as f32 / source.size.width as f32,
                input.size.height as f32 / source.size.height as f32,
            ]),
        });
        self.draw(&target, Some(input), area, &draw)?;
        Ok(target)
    }
}

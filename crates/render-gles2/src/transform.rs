use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{Draw, Sample, face, rgba},
    image::{Plane, Tile},
};
use krkr_protocol::{
    graphics::{DrawFace, Fill, Rect, Size},
    transform::{Filter, ImageOperation, Sampling, Transform},
};
use krkr_render::transform::{Mapping, validate_source};
use std::rc::Rc;

impl Gpu {
    fn transform_clear_fill(
        &self,
        image: &Image,
        clip: Rect,
        operation: ImageOperation,
        clear: Option<u32>,
    ) -> Option<Fill> {
        let color = clear?;
        (matches!(operation, ImageOperation::Copy { hold_alpha: false })
            && (clip != image.size.rect() || self.canvas_is_fragmented(image)))
        .then_some(Fill {
            rectangle: clip,
            color,
            face: DrawFace::Alpha,
            hold_alpha: false,
        })
    }

    /// Match clear partitioning before reclaiming textures for an affine draw.
    /// The untouched outside views need no pixel allocation.
    pub fn transform_write_bytes(
        &self,
        image: &Image,
        bounds: Rect,
        clip: Rect,
        operation: ImageOperation,
        clear: Option<u32>,
        snapshot: bool,
    ) -> usize {
        if let Some(fill) = self.transform_clear_fill(image, clip, operation, clear)
            && let Some(area) = self.crop_fill_area(image, &fill)
        {
            // Even if sparse materialization reaches its region limit, it
            // expands only the cleared grid, never the preserved outer tiles.
            return (area.width as usize)
                .saturating_mul(area.height as usize)
                .saturating_mul(4)
                .saturating_add(4);
        }
        let area = self.transform_write_area(image, bounds, clip, operation, clear);
        self.canvas_region_write_bytes(image, area, false, snapshot)
    }

    /// Affine copy may clear a large clip around a tiny sprite. When those
    /// outside pixels already have the requested color, only rasterize and
    /// allocate the actual transformed bounds. Preserve masked alpha exactly.
    pub fn transform_write_area(
        &self,
        image: &Image,
        bounds: Rect,
        clip: Rect,
        operation: ImageOperation,
        clear: Option<u32>,
    ) -> Rect {
        let Some(color) = clear else {
            return bounds;
        };
        if let ImageOperation::Copy { hold_alpha } = operation {
            let fills = crate::canvas_tiles::outside(clip, bounds).map(|rectangle| Fill {
                rectangle,
                color,
                hold_alpha,
                face: if hold_alpha {
                    DrawFace::Opaque
                } else {
                    DrawFace::Alpha
                },
            });
            if self.fills_unchanged(image, &fills) {
                return bounds;
            }
        }
        clip
    }
    /// Materialize seam neighborhoods on GPU. Each footprint is fully copied
    /// before it can be sampled.
    pub(crate) fn gather_affine(
        &self,
        source: &Plane,
        canvas: Size,
        area: Rect,
        scratch: &mut Option<Rc<Texture>>,
    ) -> Result<Plane> {
        let _profile = krkr_protocol::profile::span_detail("gpu.affine.gather", || {
            format!("source={:?} canvas={canvas:?} area={area:?}", source.size)
        });
        let required = Size {
            width: area.width,
            height: area.height,
        };
        // A seam gather must not evict the composition's pending work pixels.
        // Use a fixed size so the retired-texture pool can reuse the native
        // target between operations without pinning another live allocation.
        // Every previous sample precedes the next overwrite in the GL stream.
        let edge = self.tile_edge.min(512);
        if scratch.is_none()
            && self.device.streamed_uploads()
            && required.width <= edge
            && required.height <= edge
        {
            let size = Size {
                width: edge,
                height: edge,
            };
            if size
                .rgba_bytes()
                .is_some_and(|bytes| bytes <= self.scratch_capacity())
            {
                *scratch = Some(self.device.render_texture(size, &self.scratch)?);
            }
        }
        if scratch.as_ref().is_none_or(|texture| {
            texture.size.width < required.width || texture.size.height < required.height
        }) {
            let mut size = required;
            if let Some(previous) = scratch.take() {
                let grown = Size {
                    width: required.width.max(previous.size.width),
                    height: required.height.max(previous.size.height),
                };
                let capacity = self
                    .scratch_capacity()
                    .saturating_add(previous.allocation_bytes());
                if grown.rgba_bytes().is_some_and(|bytes| bytes <= capacity) {
                    size = grown;
                }
            }
            // Animation changes footprints by a few pixels per frame. A
            // small allocation grid lets consecutive transforms reuse storage;
            // the cropped tile below still exposes only initialized texels.
            let aligned = Size {
                width: if size.width >= 64 {
                    size.width.div_ceil(16) * 16
                } else {
                    size.width
                },
                height: if size.height >= 64 {
                    size.height.div_ceil(16) * 16
                } else {
                    size.height
                },
            };
            if aligned.width <= self.device.max_texture
                && aligned.height <= self.device.max_texture
                && aligned
                    .rgba_bytes()
                    .is_some_and(|bytes| bytes <= self.scratch_capacity())
            {
                size = aligned;
            }
            *scratch = Some(self.device.sample_texture(size, &self.scratch)?);
        }
        let texture = scratch.as_ref().unwrap();
        let rectangle = Rect {
            left: area.left,
            top: area.top,
            ..texture.size.rect()
        };
        let mut target = Plane {
            size: canvas,
            budget: self.scratch.clone(),
            tiles: vec![Tile {
                backing: None,
                rectangle,
                texture: texture.clone(),
            }],
        };
        let sx = source.size.width as f32 / canvas.width as f32;
        let sy = source.size.height as f32 / canvas.height as f32;
        self.draw(
            &target,
            Some(source),
            area,
            &Draw::copy(
                [sx, 0., (sx - 1.) * 0.5, 0., sy, (sy - 1.) * 0.5],
                [true; 4],
            ),
        )?;
        // Only this footprint has been initialized. Keep the full texture's
        // sampling grid while hiding the unused tail from subsequent draws.
        target.tiles[0] = target.tiles[0].cropped(area);
        Ok(target)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn transform(
        &self,
        image: &mut Image,
        source: &Image,
        rectangle: Rect,
        transform: Transform,
        sampling: Sampling,
        operation: ImageOperation,
        clip: Rect,
        clear: Option<u32>,
    ) -> Result<()> {
        let _draw_state = self.device.draw_state.scope();
        let _profile = krkr_protocol::profile::span_detail("gpu.transform", || {
            format!(
                "source={:?}/{:?} target={:?}/{:?} tiles={} rectangle={rectangle:?} transform={transform:?} operation={operation:?} clear={clear:?} clip={clip:?}",
                source.size,
                source.stored_size(),
                image.size,
                image.stored_size(),
                image.main.as_ref().map_or(0, |plane| plane.tiles.len())
            )
        });
        self.check_image(image)?;
        self.check_image(source)?;
        source.plane(false)?;
        image.plane(false)?;
        if rectangle.width == 0 || rectangle.height == 0 || operation.is_noop() {
            return Ok(());
        }
        let Some(clip) = clip.intersection(image.size.rect()) else {
            return Ok(());
        };
        if let ImageOperation::Blend(options) = operation
            && !options.accepts_face()
        {
            return Err(Error::Message("blend mode does not accept the draw face"));
        }
        // StretchBlt's unscaled fast path is a normal clipped copy, including
        // source rectangles extending outside the bitmap. Match the desktop
        // backend and legacy dispatch before affine source validation.
        if let Transform::Stretch(dest) = transform
            && i64::from(dest.width) == i64::from(rectangle.width)
            && i64::from(dest.height) == i64::from(rectangle.height)
            && {
                let destination = Rect {
                    left: dest.left,
                    top: dest.top,
                    ..rectangle
                };
                destination.intersection(clip) == Some(destination)
            }
        {
            return self.apply_operation(
                image, source, rectangle, dest.left, dest.top, clip, operation,
            );
        }
        validate_source(rectangle, source.size)?;
        if let Transform::Stretch(dest) = transform
            && !matches!(sampling.filter, Filter::Nearest | Filter::FastLinear)
        {
            return self.resample(image, source, rectangle, dest, sampling, operation, clip);
        }
        if !operation.affine_supported() {
            return Ok(());
        }
        let Some(mapping) = Mapping::new(rectangle, transform, clip)? else {
            if let Some(color) = clear {
                let hold_alpha = matches!(operation, ImageOperation::Copy { hold_alpha: true });
                self.fill(
                    image,
                    &[Fill {
                        rectangle: clip,
                        color,
                        face: if hold_alpha {
                            DrawFace::Opaque
                        } else {
                            DrawFace::Alpha
                        },
                        hold_alpha,
                    }],
                )?;
            }
            return Ok(());
        };
        if let Some(fill) = self.transform_clear_fill(image, clip, operation, clear) {
            // The whole clip is replaced by this operation. Reset its tile
            // partition together: clearing four moving border bands leaves
            // old intersections inside the sprite and grows them every frame.
            // For shared canvases this also retains only outside views, rather
            // than copying old border tiles before overwriting their interior.
            self.fill_cropped_regions(image, &fill)?;
        }
        let write_area = self.transform_write_area(image, mapping.bounds, clip, operation, clear);
        // A full overwrite can skip copying old pixels when detaching storage.
        // Reuse already writable storage and preserve sparse clear-colored
        // margins; neither case needs a new full-image allocation.
        let replaced = if image.canvas
            && clip == image.size.rect()
            && write_area == clip
            && clear.is_some()
            && matches!(operation, ImageOperation::Copy { hold_alpha: false })
            && self.canvas_write_bytes(image, false) != 0
        {
            let stored = self.canvas_storage(image.size, Some(image));
            match self.overwrite_plane(stored, &self.resident) {
                Ok(plane) => {
                    image.main = Some(plane);
                    true
                }
                Err(Error::Budget(_)) => false,
                Err(error) => return Err(error),
            }
        } else {
            false
        };
        let bounds = if sampling.no_clip {
            source.size.rect()
        } else {
            rectangle
        };
        let mut area = if replaced {
            // The new plane has no previous clear-colored border to preserve.
            clip
        } else {
            write_area
        };
        if !replaced
            && area != mapping.bounds
            && let Some(color) = clear
            && let ImageOperation::Copy { hold_alpha } = operation
        {
            // No source sample outside the transformed bounding rectangle
            // contributes to a copy. Clear these bands directly instead of
            // recursively gathering/filtering source tiles just to reject them.
            let fills = crate::canvas_tiles::outside(area, mapping.bounds).map(|rectangle| Fill {
                rectangle,
                color,
                face: if hold_alpha {
                    DrawFace::Opaque
                } else {
                    DrawFace::Alpha
                },
                hold_alpha,
            });
            self.fill(image, &fills)?;
            area = mapping.bounds;
        }
        let linear = sampling.filter != Filter::Nearest && operation.affine_linear();
        let mut draw = operation_draw(mapping.inverse, operation);
        draw.color = rgba(clear.unwrap_or(0));
        draw.sampling = Some(Sample {
            region: rectangle,
            bounds,
            linear,
            display: false,
            sharpen: false,
            clear: clear.is_some(),
            scale: None,
        });
        if !replaced {
            if clear.is_some() && matches!(operation, ImageOperation::Copy { hold_alpha: false }) {
                // The affine shader also writes the clear color outside the
                // rotated shape. Snapshot detachment needs no old pixels in
                // tiles wholly covered by this output rectangle.
                self.writable_copy_main(image, area)?;
            } else {
                self.writable_compact(image, area, false)?;
            }
        }
        // Edited canvases contain samples on their stored grid. Interpolating
        // rounded logical neighbors first repeats texels at fractional density,
        // producing a regular staircase when a blurred sprite is enlarged.
        let stored = source.plane(false)?;
        if linear && self.canvas_limit.is_some() && source.canvas && stored.size != source.size {
            let raster = crate::scene::raster::Raster::new(source.size, stored.size, (0, 0))?;
            if let (Some(region), Some(bounds)) = (raster.rect(rectangle), raster.rect(bounds)) {
                let sx = stored.size.width as f32 / source.size.width as f32;
                let sy = stored.size.height as f32 / source.size.height as f32;
                let [a, b, tx, c, d, ty] = draw.mapping;
                draw.mapping = [
                    a * sx,
                    b * sx,
                    (tx + 0.5) * sx - 0.5,
                    c * sy,
                    d * sy,
                    (ty + 0.5) * sy - 0.5,
                ];
                let sample = draw.sampling.as_mut().unwrap();
                sample.region = region;
                sample.bounds = bounds;
                let mut physical = source.shared_main();
                physical.size = stored.size;
                if stored.tiles.len() == 1 {
                    return self.draw_image(image, false, Some(stored), area, &draw);
                }
                let target = image.plane(false)?;
                if let Some((area, draw)) =
                    self.raster_draw(image.size, target.size, area, &draw)?
                {
                    self.affine_parts(target, &physical, area, &draw, &mut None)?;
                }
                return Ok(());
            }
        }
        // A single source texture already contains every bilinear neighbor.
        // Sample its stored texels directly, preserving logical rounding, and
        // avoid recursively gathering hundreds of temporary source blocks
        // when an animation shrinks or rotates a large image.
        if (!linear && clear.is_none()) || source.plane(false)?.tiles.len() == 1 {
            let stored = source.plane(false)?;
            draw.sampling.as_mut().unwrap().scale = Some([
                stored.size.width as f32 / source.size.width as f32,
                stored.size.height as f32 / source.size.height as f32,
            ]);
            return self.draw_image(image, false, Some(stored), area, &draw);
        }
        // Split large or steep transforms until their source neighborhood fits
        // an ES2 texture and the scratch budget. Even a one-pixel destination
        // then needs at most four logical samples. Reuse the bounded gather
        // target when available, or a local texture under memory pressure.
        let target = image.plane(false)?;
        if let Some((area, draw)) = self.raster_draw(image.size, target.size, area, &draw)? {
            self.affine_parts(target, source, area, &draw, &mut None)?;
        }
        Ok(())
    }
    pub(crate) fn affine_parts(
        &self,
        target: &Plane,
        source: &Image,
        area: Rect,
        draw: &Draw,
        scratch: &mut Option<Rc<Texture>>,
    ) -> Result<()> {
        let stored = source.plane(false)?;
        let ratio = [
            stored.size.width as f32 / source.size.width as f32,
            stored.size.height as f32 / source.size.height as f32,
        ];
        let (footprint, physical) = affine_footprint(draw, area, stored.size, ratio);
        if let Some(tile) = stored
            .tiles
            .iter()
            .find(|tile| tile.rectangle.intersection(physical) == Some(physical))
        {
            let input = Plane {
                size: stored.size,
                budget: stored.budget.clone(),
                tiles: vec![tile.clone()],
            };
            let mut direct = draw.clone();
            direct.sampling.as_mut().unwrap().scale = Some(ratio);
            return self.draw(target, Some(&input), area, &direct);
        }
        // Axis-aligned scaling only needs gathers along tile seams. Peel off
        // a sizeable tile interior before the bounded gather fallback, rather
        // than copying a whole half-screen because one edge crosses a seam.
        if let Some(interior) = affine_interior(draw, area, stored, ratio) {
            self.affine_parts(target, source, interior, draw, scratch)?;
            for remainder in crate::canvas_tiles::outside(area, interior) {
                if remainder.width != 0 && remainder.height != 0 {
                    self.affine_parts(target, source, remainder, draw, scratch)?;
                }
            }
            return Ok(());
        }
        let limit = self.tile_edge.min(512);
        // Dense preprocessed assets can have more stored pixels than logical
        // pixels. Keep the smaller logical gather in that case.
        let compact = physical.width <= footprint.width
            && physical.height <= footprint.height
            && stored.size != source.size;
        let gathered_footprint = if compact { physical } else { footprint };
        let gather_bytes = Size {
            width: gathered_footprint.width,
            height: gathered_footprint.height,
        }
        .rgba_bytes()
        .unwrap_or(usize::MAX);
        // The previous gather is replaceable, but remains charged until the
        // device can safely recycle it. Include it in admission, not free bytes.
        let capacity = self.scratch_capacity().saturating_add(
            scratch
                .as_ref()
                .map_or(0, |texture| texture.allocation_bytes()),
        );
        if (gathered_footprint.width > limit
            || gathered_footprint.height > limit
            || area.width > limit
            || area.height > limit
            || gather_bytes > capacity)
            && (area.width > 1 || area.height > 1)
        {
            let horizontal = area.width >= area.height;
            let split = if horizontal {
                area.width / 2
            } else {
                area.height / 2
            };
            let first = if horizontal {
                Rect {
                    width: split,
                    ..area
                }
            } else {
                Rect {
                    height: split,
                    ..area
                }
            };
            let second = if horizontal {
                Rect {
                    left: area.left + split as i32,
                    width: area.width - split,
                    ..area
                }
            } else {
                Rect {
                    top: area.top + split as i32,
                    height: area.height - split,
                    ..area
                }
            };
            self.affine_parts(target, source, first, draw, scratch)?;
            return self.affine_parts(target, source, second, draw, scratch);
        }
        // Gather stored texels without expanding compact images back to their
        // logical resolution. The shader rounds each logical neighbor before
        // scaling it, preserving the original bilinear interpolation at seams.
        let canvas = if compact { stored.size } else { source.size };
        let input = self.gather_affine(stored, canvas, gathered_footprint, scratch)?;
        let mut gathered = draw.clone();
        if compact {
            gathered.sampling.as_mut().unwrap().scale = Some(ratio);
        }
        self.draw(target, Some(&input), area, &gathered)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_operation(
        &self,
        image: &mut Image,
        source: &Image,
        rect: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        operation: ImageOperation,
    ) -> Result<()> {
        match operation {
            ImageOperation::Copy { hold_alpha } => self.copy_rect(
                image,
                source,
                rect,
                x,
                y,
                clip,
                if hold_alpha {
                    DrawFace::Opaque
                } else {
                    DrawFace::Alpha
                },
                hold_alpha,
            ),
            ImageOperation::Blend(options) => {
                self.operate(image, source, rect, x, y, clip, options)
            }
        }
    }
}
pub(crate) fn affine_footprint(
    draw: &Draw,
    area: Rect,
    stored: Size,
    ratio: [f32; 2],
) -> (Rect, Rect) {
    let sample = draw.sampling.as_ref().unwrap();
    let footprint = Mapping {
        inverse: draw.mapping,
        bounds: area,
    }
    .source_region(sample.region, sample.bounds, sample.linear);
    let axis = |start: i32, length: u32, scale: f32, limit: u32| {
        let lo = (((start as f32 + 0.5) * scale).floor() as i64 - 1).clamp(0, i64::from(limit));
        let hi = (((start as f32 + length as f32 - 0.5) * scale).floor() as i64 + 2)
            .clamp(0, i64::from(limit));
        (lo as i32, hi.saturating_sub(lo) as u32)
    };
    let (left, width) = axis(footprint.left, footprint.width, ratio[0], stored.width);
    let (top, height) = axis(footprint.top, footprint.height, ratio[1], stored.height);
    (
        footprint,
        Rect {
            left,
            top,
            width,
            height,
        },
    )
}

fn affine_interior(draw: &Draw, area: Rect, stored: &Plane, ratio: [f32; 2]) -> Option<Rect> {
    let [a, b, tx, c, d, ty] = draw.mapping;
    if b != 0.
        || c != 0.
        || a == 0.
        || d == 0.
        || !draw.mapping.iter().all(|v| v.is_finite())
        || stored.tiles.len() > 64
    {
        return None;
    }
    let pixels = |r: Rect| u64::from(r.width) * u64::from(r.height);
    let axis = |start: i32,
                length: u32,
                edge: i32,
                span: u32,
                limit: u32,
                scale: f32,
                step: f32,
                offset: f32| {
        let end = f64::from(start) + f64::from(length);
        // Leave interpolation and rounding guards inside the tile. Image-edge
        // clamping needs no guard beyond the outside edge of the entire plane.
        let lower = if edge == 0 {
            f64::NEG_INFINITY
        } else {
            (f64::from(edge) + 2.) / f64::from(scale) + 2.
        };
        let upper = if i64::from(edge) + i64::from(span) == i64::from(limit) {
            f64::INFINITY
        } else {
            (f64::from(edge) + f64::from(span) - 2.) / f64::from(scale) - 2.
        };
        if lower > upper {
            return (start, 0);
        }
        // Mirrored sprites have decreasing source coordinates. Invert both
        // bounds before ordering them; the footprint check below also covers
        // interpolation and rounding at the reversed seam.
        let first = (lower - f64::from(offset)) / f64::from(step);
        let last = (upper - f64::from(offset)) / f64::from(step);
        let lo = first.min(last).ceil().clamp(f64::from(start), end);
        let hi = (first.max(last).floor() + 1.).clamp(lo, end);
        (lo as i32, (hi - lo) as u32)
    };
    stored
        .tiles
        .iter()
        .filter_map(|tile| {
            let r = tile.rectangle;
            let (left, width) = axis(
                area.left,
                area.width,
                r.left,
                r.width,
                stored.size.width,
                ratio[0],
                a,
                tx,
            );
            let (top, height) = axis(
                area.top,
                area.height,
                r.top,
                r.height,
                stored.size.height,
                ratio[1],
                d,
                ty,
            );
            let candidate = Rect {
                left,
                top,
                width,
                height,
            };
            if width == 0
                || height == 0
                || candidate == area
                || pixels(candidate) < 4096
                || pixels(candidate) * 8 < pixels(area)
            {
                return None;
            }
            // The same footprint proof as the direct draw is authoritative. If
            // cancellation or compact rounding crosses an edge, keep the gather.
            let (_, physical) = affine_footprint(draw, candidate, stored.size, ratio);
            (r.intersection(physical) == Some(physical)).then_some(candidate)
        })
        .max_by_key(|&r| pixels(r))
}

pub(crate) fn operation_draw(mapping: [f32; 6], operation: ImageOperation) -> Draw {
    match operation {
        ImageOperation::Copy { hold_alpha } => Draw::copy(mapping, [true, true, true, !hold_alpha]),
        ImageOperation::Blend(options) => Draw {
            kind: if options.mode == krkr_protocol::graphics::Blend::Opaque
                && options.opacity == 255
            {
                if options.face == DrawFace::Opaque {
                    0.
                } else {
                    6.
                }
            } else {
                1.
            },
            mask: [
                true,
                true,
                true,
                !(options.mode == krkr_protocol::graphics::Blend::Opaque
                    && options.opacity == 255
                    && options.face == DrawFace::Opaque
                    && options.hold_alpha),
            ],
            operation: [
                options.mode as i32 as f32,
                face(options.face),
                f32::from(options.opacity),
                f32::from(options.hold_alpha),
            ],
            ..Draw::copy(mapping, [true; 4])
        },
    }
}

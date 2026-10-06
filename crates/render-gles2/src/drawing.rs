use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    image::{Plane, Tile},
};
use glow::HasContext;
use krkr_protocol::graphics::{BlendOptions, DrawFace, Fill, Rect, Size};
use krkr_render::blit;

pub(crate) fn rgba(color: u32) -> [f32; 4] {
    [
        (color >> 16 & 255) as f32,
        (color >> 8 & 255) as f32,
        (color & 255) as f32,
        (color >> 24) as f32,
    ]
}
pub(crate) fn color_fill(
    rectangle: Rect,
    color: u32,
    opacity: i16,
    face: DrawFace,
) -> Option<Fill> {
    let (color, hold_alpha, face) = match face {
        DrawFace::Mask | DrawFace::Province => (color, false, face),
        _ if opacity == 255 => (color | 0xff000000, face == DrawFace::Opaque, face),
        DrawFace::Alpha if opacity == -255 => (0, false, DrawFace::Mask),
        _ => return None,
    };
    Some(Fill {
        rectangle,
        color,
        hold_alpha,
        face,
    })
}
pub(crate) fn face(face: DrawFace) -> f32 {
    match face {
        DrawFace::Alpha => 0.,
        DrawFace::Opaque => 1.,
        DrawFace::Mask => 2.,
        DrawFace::Province => 3.,
        DrawFace::AddAlpha => 4.,
    }
}
pub(crate) fn rect(rect: Rect) -> [f32; 4] {
    [
        rect.left as f32,
        rect.top as f32,
        rect.width as f32,
        rect.height as f32,
    ]
}

/// A clipped identity copy can still share its source if the excluded pixels
/// already agree. Prove this from shared tiles or tracked solid margins; never
/// read textures back to discover equality.
fn equal_outside(a: &Plane, b: &Plane, area: Rect) -> bool {
    let solid = (b.size
        == Size {
            width: 1,
            height: 1,
        })
    .then(|| b.tiles.first()?.texture.solid_color())
    .flatten();
    let right = area.left as u32 + area.width;
    let bottom = area.top as u32 + area.height;
    let bands = [
        Rect {
            left: 0,
            top: 0,
            width: a.size.width,
            height: area.top as u32,
        },
        Rect {
            left: 0,
            top: bottom as i32,
            width: a.size.width,
            height: a.size.height - bottom,
        },
        Rect {
            left: 0,
            top: area.top,
            width: area.left as u32,
            height: area.height,
        },
        Rect {
            left: right as i32,
            top: area.top,
            width: a.size.width - right,
            height: area.height,
        },
    ];
    for band in bands.into_iter().filter(|r| r.width != 0 && r.height != 0) {
        let mut covered = 0u64;
        for left in &a.tiles {
            let Some(part) = left.rectangle.intersection(band) else {
                continue;
            };
            if let Some(color) = solid {
                if left.solid_region(Rect {
                    left: part.left - left.rectangle.left,
                    top: part.top - left.rectangle.top,
                    ..part
                }) != Some(color)
                {
                    return false;
                }
                covered += u64::from(part.width) * u64::from(part.height);
                continue;
            }
            for right in &b.tiles {
                let Some(part) = right.rectangle.intersection(part) else {
                    continue;
                };
                if left.rectangle != right.rectangle
                    || left.backing != right.backing
                    || !std::rc::Rc::ptr_eq(&left.texture, &right.texture)
                {
                    let local = |tile: &Tile| Rect {
                        left: part.left - tile.rectangle.left,
                        top: part.top - tile.rectangle.top,
                        ..part
                    };
                    let color = left.solid_region(local(left));
                    if color.is_none() || color != right.solid_region(local(right)) {
                        return false;
                    }
                }
                covered += u64::from(part.width) * u64::from(part.height);
            }
        }
        if covered != u64::from(band.width) * u64::from(band.height) {
            return false;
        }
    }
    true
}
#[derive(Clone)]
pub(crate) struct Draw {
    pub kind: f32,
    pub color: [f32; 4],
    pub operation: [f32; 4],
    pub mask: [bool; 4],
    pub mapping: [f32; 6],
    pub sampling: Option<Sample>,
}
#[derive(Clone)]
pub(crate) struct Sample {
    pub region: Rect,
    pub bounds: Rect,
    pub linear: bool,
    /// Continuous stored-pixel filtering for display composition. Script
    /// transforms keep their integer-compatible interpolation separately.
    pub display: bool,
    pub sharpen: bool,
    pub clear: bool,
    /// Logical samples map straight to compact storage. A linear neighborhood
    /// crossing storage tiles still uses a gathered texture.
    pub scale: Option<[f32; 2]>,
}
impl Draw {
    pub fn copy(mapping: [f32; 6], mask: [bool; 4]) -> Self {
        Self {
            kind: 0.,
            color: [0.; 4],
            operation: [0.; 4],
            mask,
            mapping,
            sampling: None,
        }
    }
}
fn source_footprint(
    area: Rect,
    mapping: [f32; 6],
    size: Size,
    scale: Option<[f32; 2]>,
) -> Option<Rect> {
    let [a, b, tx, c, d, ty] = mapping.map(f64::from);
    let x0 = f64::from(area.left);
    let y0 = f64::from(area.top);
    let x1 = x0 + f64::from(area.width) - 1.;
    let y1 = y0 + f64::from(area.height) - 1.;
    let points = [[x0, y0], [x1, y0], [x0, y1], [x1, y1]]
        .map(|[x, y]| [a * x + b * y + tx, c * x + d * y + ty]);
    // Bound dot-product rounding before nearest logical sampling, then map
    // that interval to stored texels. A logical draw must not visit every tile
    // of a long compact strip just because it uses a different sampling grid.
    let axis = |i: usize, limit: u32| {
        let coefficients = if i == 0 { [a, b, tx] } else { [c, d, ty] };
        let magnitude = coefficients[0].abs() * x0.abs().max(x1.abs())
            + coefficients[1].abs() * y0.abs().max(y1.abs())
            + coefficients[2].abs();
        let error = 1. + magnitude * 8. * f64::from(f32::EPSILON);
        let mut lo = points.iter().map(|p| p[i]).fold(f64::INFINITY, f64::min) - error;
        let mut hi = points
            .iter()
            .map(|p| p[i])
            .fold(f64::NEG_INFINITY, f64::max)
            + error;
        if let Some(scale) = scale {
            lo = ((lo + 0.5).floor() + 0.5) * f64::from(scale[i]);
            hi = ((hi + 0.5).floor() + 0.5) * f64::from(scale[i]);
        }
        let lo = lo.floor() - 1.;
        let hi = hi.ceil() + 2.;
        (
            lo.clamp(0., f64::from(limit)) as i32,
            hi.clamp(0., f64::from(limit)) as i32,
        )
    };
    let (left, right) = axis(0, size.width);
    let (top, bottom) = axis(1, size.height);
    (right > left && bottom > top).then_some(Rect {
        left,
        top,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}
/// Prove coverage before discarding destination pixels. In particular, a raw
/// affine draw can discard fragments outside its source, and channel-masked
/// copies must preserve the other channels. Keep those on the loading path.
pub(crate) fn overwrites(draw: &Draw, source: Option<&Plane>, area: Rect) -> bool {
    if draw.mask != [true; 4] {
        return false;
    }
    if draw.kind == 3. {
        return true;
    }
    if !matches!(draw.kind, 0. | 6. | 8.) {
        return false;
    }
    let Some(source) = source else { return false };
    // Affine copy with a clear color writes every destination fragment:
    // out-of-image samples become that color instead of being discarded.
    // Loading the old target before each filtered block serves no purpose.
    if source.tiles.len() == 1
        && draw
            .sampling
            .as_ref()
            .is_some_and(|sample| sample.clear && !sample.display)
    {
        return true;
    }
    // Gathered regions have one offset tile in a larger virtual plane.
    let stored = if source.tiles.len() == 1 {
        source.tiles[0].rectangle
    } else {
        source.size.rect()
    };
    let (bounds, scale) = match &draw.sampling {
        None => (stored, None),
        // Filtered shaders only reject samples outside the logical region.
        // A single texture (including a seam gather) supplies every neighbor;
        // interpolation itself does not require the previous destination.
        Some(sample) if source.tiles.len() == 1 && (sample.linear || sample.display) => {
            (sample.region, None)
        }
        Some(sample) if sample.linear || sample.clear => return false,
        Some(sample) if sample.scale.is_some() => (sample.region, sample.scale),
        Some(_) => return false,
    };
    let [a, b, tx, c, d, ty] = draw.mapping;
    let x = area.left as f32;
    let y = area.top as f32;
    let right = x + area.width as f32 - 1.;
    let bottom = y + area.height as f32 - 1.;
    [[x, y], [right, y], [x, bottom], [right, bottom]]
        .into_iter()
        .all(|[x, y]| {
            let q = [a * x + b * y + tx, c * x + d * y + ty];
            // Bound rounding by operand magnitude, including cancellation in
            // large translations. Propagate the interval through nearest
            // sampling, since floor(q + 0.5) can cross a pixel boundary.
            let magnitudes = [
                (a * x).abs() + (b * y).abs() + tx.abs(),
                (c * x).abs() + (d * y).abs() + ty.abs(),
            ];
            (0..2).all(|axis| {
                let (start, length, stored_start, stored_length) = if axis == 0 {
                    (bounds.left, bounds.width, stored.left, stored.width)
                } else {
                    (bounds.top, bounds.height, stored.top, stored.height)
                };
                let error = 0.001
                    + (f64::from(magnitudes[axis]) + f64::from(start).abs() + f64::from(length))
                        * 8.
                        * f64::from(f32::EPSILON);
                let low = f64::from(q[axis]) - error;
                let high = f64::from(q[axis]) + error;
                if !(low >= f64::from(start) - 0.5
                    && high < f64::from(start) + f64::from(length) - 0.5)
                {
                    return false;
                }
                scale.is_none_or(|scale| {
                    let scale = f64::from(scale[axis]);
                    if !scale.is_finite() || scale <= 0. {
                        return false;
                    }
                    let low = ((low + 0.5).floor() + 0.5) * scale;
                    let high = ((high + 0.5).floor() + 0.5) * scale;
                    let error = 0.001
                        + (low.abs()
                            + high.abs()
                            + f64::from(stored_start).abs()
                            + f64::from(stored_length))
                            * 8.
                            * f64::from(f32::EPSILON);
                    low - error >= f64::from(stored_start)
                        && high + error < f64::from(stored_start) + f64::from(stored_length)
                })
            })
        })
}
impl Gpu {
    pub fn fill(&self, image: &mut Image, fills: &[Fill]) -> Result<()> {
        self.check_image(image)?;
        if self.fills_unchanged(image, fills) {
            return Ok(());
        }
        if fills.len() > 1 && fills.iter().all(|f| f.face != DrawFace::Province) {
            return self.fill_main_batch(image, fills);
        }
        for fill in fills {
            let constant = self.constant_channel_fill(image, fill);
            let fill = constant.as_ref().unwrap_or(fill);
            let Some(area) = image.size.rect().intersection(fill.rectangle) else {
                continue;
            };
            let province = fill.face == DrawFace::Province;
            let uniform = image.canvas
                && (area == image.size.rect() || self.fill_restores_solid(image, fill))
                && !matches!(fill.face, DrawFace::Province | DrawFace::Mask)
                && !(fill.face == DrawFace::Opaque && fill.hold_alpha);
            if uniform
                && self.canvas_limit.is_some()
                && (image.size.width > 1 || image.size.height > 1)
            {
                image.main = self.create_image(image.size, fill.color)?.main;
                continue;
            }
            if uniform
                && let Some(plane) = self
                    .solid_images
                    .borrow_mut()
                    .get(self.canvas_storage(image.size, Some(image)), fill.color)
            {
                image.main = Some(plane);
                continue;
            }
            if province {
                self.writable_compact(image, area, true)?;
            } else {
                self.writable_fill_main(image, area, std::slice::from_ref(fill))?;
            }
            let (mask, color) = match fill.face {
                DrawFace::Province => (
                    [true, false, false, false],
                    [(fill.color & 255) as f32, 0., 0., 0.],
                ),
                DrawFace::Mask => (
                    [false, false, false, true],
                    [0., 0., 0., (fill.color & 255) as f32],
                ),
                DrawFace::Opaque if fill.hold_alpha => {
                    ([true, true, true, false], rgba(fill.color))
                }
                _ => ([true; 4], rgba(fill.color)),
            };
            let plane = image.plane(province)?;
            let raster = crate::scene::raster::Raster::new(image.size, plane.size, (0, 0))?;
            let Some(area) = raster.rect(area) else {
                continue;
            };
            for tile in &plane.tiles {
                if crate::fills::unchanged(tile, image.size, raster, fill) {
                    continue;
                }
                if let Some(part) = area.intersection(tile.rectangle) {
                    let local = Rect {
                        left: part.left - tile.rectangle.left,
                        top: part.top - tile.rectangle.top,
                        ..part
                    };
                    let framebuffer = if mask == [true; 4] {
                        tile.texture.overwrite_framebuffer(local)?
                    } else {
                        tile.texture.framebuffer_region(local)?
                    };
                    unsafe {
                        let gl = &self.device.gl;
                        if gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32
                            != framebuffer.0.get()
                        {
                            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                        }
                        gl.enable(glow::SCISSOR_TEST);
                        gl.scissor(
                            part.left - tile.rectangle.left,
                            part.top - tile.rectangle.top,
                            part.width as i32,
                            part.height as i32,
                        );
                        gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
                        gl.clear_color(
                            color[0] / 255.,
                            color[1] / 255.,
                            color[2] / 255.,
                            color[3] / 255.,
                        );
                        gl.clear(glow::COLOR_BUFFER_BIT);
                    }
                    if mask == [true; 4] {
                        tile.texture.cleared(local, fill.color);
                    }
                }
            }
            self.device.check()?;
            self.share_solid_canvas(image);
        }
        Ok(())
    }
    fn share_solid_canvas(&self, image: &mut Image) {
        if !image.canvas {
            return;
        }
        let Some(plane) = image.main.as_ref() else {
            return;
        };
        let Some(color) = plane.tiles.first().and_then(|t| t.texture.solid_color()) else {
            return;
        };
        if !plane
            .tiles
            .iter()
            .all(|t| t.texture.solid_color() == Some(color))
        {
            return;
        }
        let existing = self.solid_images.borrow_mut().get(plane.size, color);
        if let Some(existing) = existing {
            image.main = Some(existing);
        } else {
            self.solid_images
                .borrow_mut()
                .insert(image, color, &self.resident, &self.staging);
        }
    }
    pub(crate) fn fill_main_batch(&self, image: &mut Image, fills: &[Fill]) -> Result<()> {
        let bounds = fills
            .iter()
            .filter_map(|fill| fill.rectangle.intersection(image.size.rect()))
            .reduce(|a, b| crate::scene_damage::union(Some(a), b));
        let Some(bounds) = bounds else { return Ok(()) };
        self.writable_fill_main(image, bounds, fills)?;
        let plane = image.plane(false)?;
        let raster = crate::scene::raster::Raster::new(image.size, plane.size, (0, 0))?;
        let mask = crate::fills::mask(&fills[0]);
        let pixels = fills
            .iter()
            .filter_map(|fill| {
                fill.rectangle
                    .intersection(image.size.rect())
                    .and_then(|r| raster.rect(r))
            })
            .fold(0u64, |sum, area| {
                sum.saturating_add(u64::from(area.width) * u64::from(area.height))
            });
        // Large clears already use an efficient driver path. Vertex batching
        // pays off for many tiny rectangles; avoid creating another program
        // and geometry stream for a handful of large background fills.
        if fills.len() >= 8
            && pixels <= (fills.len() as u64).saturating_mul(256)
            && fills.iter().all(|fill| crate::fills::mask(fill) == mask)
        {
            return self.fill_rectangles(plane, image.size, raster, fills, mask);
        }
        let gl = &self.device.gl;
        // No texture allocation or plane changes inside this loop. Work-surface
        // loads/stores preserve the clear state, so small ordered fills need
        // only their changed scissor/color plus Clear, not a full GL setup.
        let mut bound = None;
        let mut previous_mask = None;
        let mut previous_color = None;
        let mut scissor = false;
        for tile in &plane.tiles {
            for fill in fills {
                if crate::fills::unchanged(tile, image.size, raster, fill) {
                    continue;
                }
                let Some(part) = fill
                    .rectangle
                    .intersection(image.size.rect())
                    .and_then(|r| raster.rect(r))
                    .and_then(|r| r.intersection(tile.rectangle))
                else {
                    continue;
                };
                let (mask, color) = match fill.face {
                    DrawFace::Mask => (
                        [false, false, false, true],
                        [0., 0., 0., (fill.color & 255) as f32],
                    ),
                    DrawFace::Opaque if fill.hold_alpha => {
                        ([true, true, true, false], rgba(fill.color))
                    }
                    _ => ([true; 4], rgba(fill.color)),
                };
                let local = Rect {
                    left: part.left - tile.rectangle.left,
                    top: part.top - tile.rectangle.top,
                    ..part
                };
                let framebuffer = if mask == [true; 4] {
                    tile.texture.overwrite_framebuffer(local)?
                } else {
                    tile.texture.framebuffer_region(local)?
                };
                unsafe {
                    if bound != Some(framebuffer) {
                        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                        bound = Some(framebuffer);
                    }
                    if !scissor {
                        gl.enable(glow::SCISSOR_TEST);
                        scissor = true;
                    }
                    if previous_mask != Some(mask) {
                        gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
                        previous_mask = Some(mask);
                    }
                    if previous_color != Some(color) {
                        gl.clear_color(
                            color[0] / 255.,
                            color[1] / 255.,
                            color[2] / 255.,
                            color[3] / 255.,
                        );
                        previous_color = Some(color);
                    }
                    gl.scissor(
                        local.left,
                        local.top,
                        local.width as i32,
                        local.height as i32,
                    );
                    gl.clear(glow::COLOR_BUFFER_BIT);
                }
                if mask == [true; 4] {
                    tile.texture.cleared(local, fill.color);
                }
            }
        }
        self.device.check()?;
        self.share_solid_canvas(image);
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn copy_rect(
        &self,
        image: &mut Image,
        source: &Image,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        face: DrawFace,
        hold_alpha: bool,
    ) -> Result<()> {
        self.check_image(source)?;
        self.check_image(image)?;
        let Some((src, dst)) = blit::region(source.size, image.size, clip, rectangle, x, y) else {
            return Ok(());
        };
        let province = face == DrawFace::Province;
        if province && !source.has_province() {
            return self.fill(
                image,
                &[Fill {
                    rectangle: dst,
                    color: 0,
                    face,
                    hold_alpha,
                }],
            );
        }
        let plane = source.plane(province)?;
        if source.size == image.size
            && src == dst
            && image
                .plane(province)
                .is_ok_and(|target| std::rc::Rc::ptr_eq(plane, target))
        {
            // Shared assignments and self copies already contain these pixels.
            // Avoid detaching the whole tile just to copy it back unchanged.
            return Ok(());
        }
        let mask = match face {
            DrawFace::Province => [true, false, false, false],
            DrawFace::Mask => [false, false, false, true],
            DrawFace::Opaque if hold_alpha => [true, true, true, false],
            _ => [true; 4],
        };
        if !province {
            image.text |= source.text;
        }
        if !province
            && mask == [true; 4]
            && (self.share_full_copy(image, source, src, dst)
                || self.share_inset_copy(image, source, src, dst)?)
        {
            return Ok(());
        }
        if !province
            && mask == [true; 4]
            && image.size == source.size
            && src == dst
            && let Ok(current) = image.plane(false)
            && (current.size == plane.size
                || current.size
                    == Size {
                        width: 1,
                        height: 1,
                    }
                || dst == image.size.rect())
            && ((image.canvas
                && self.canvas_limit.is_some()
                && current.size == plane.size
                && plane.size
                    == Size {
                        width: 1,
                        height: 1,
                    })
                || plane.size
                    == if image.canvas && self.canvas_limit.is_some() {
                        self.canvas_storage(image.size, Some(image))
                    } else {
                        image.size
                    })
            && plane
                .tiles
                .iter()
                .all(|tile| tile.texture.belongs_to(&current.budget))
            && crate::scene::raster::Raster::new(image.size, plane.size, (0, 0))?
                .rect(dst)
                .is_some_and(|area| equal_outside(plane, current, area))
        {
            // An aligned full-plane replacement needs no rasterization.
            // Preserve the target's province and canvas policy; later writes
            // to either image detach only the touched tiles. Equal storage
            // density is required to keep the original nearest-sample grid.
            // A clip is harmless when both planes have identical outer margins.
            image.main = Some(plane.clone());
            return Ok(());
        }
        let memo = if !province && mask == [true; 4] && plane.tiles.len() <= 64 {
            let color = self.solid_images.borrow().color(image);
            let shared = image.main.as_ref().is_some_and(|plane| {
                plane.tiles.len() <= 64
                    && (std::rc::Rc::strong_count(plane) > 1
                        || plane
                            .tiles
                            .iter()
                            .any(|tile| std::rc::Rc::strong_count(&tile.texture) > 1))
            });
            (color.is_some() || shared).then_some(crate::copy_cache::Key {
                color,
                logical: image.size,
                stored: self.fill_main_size(image),
                source: src,
                destination: dst,
            })
        } else {
            None
        };
        if let Some(key) = memo
            && let Some(result) = self.copied_images.borrow_mut().get(source, image, key)
            && result
                .tiles
                .iter()
                .all(|t| t.texture.belongs_to(&image.plane(false).unwrap().budget))
        {
            image.main = Some(result);
            return Ok(());
        }
        let version = memo
            .map(|_| crate::scene_damage::Version::capture(source))
            .transpose()?;
        let target_version = memo
            .filter(|key| key.color.is_none())
            .map(|_| crate::scene_damage::Version::capture(image))
            .transpose()?;
        if !province && mask == [true; 4] {
            self.writable_copy_main(image, dst)?;
        } else {
            self.writable_compact(image, dst, province)?;
        }
        let ratio = [
            plane.size.width as f32 / source.size.width as f32,
            plane.size.height as f32 / source.size.height as f32,
        ];
        let mapping = [
            ratio[0],
            0.,
            (src.left - dst.left) as f32 * ratio[0] + (ratio[0] - 1.) * 0.5,
            0.,
            ratio[1],
            (src.top - dst.top) as f32 * ratio[1] + (ratio[1] - 1.) * 0.5,
        ];
        let mut draw = Draw::copy(mapping, mask);
        let target = image.plane(province)?;
        let density_changes = u64::from(plane.size.width) * u64::from(image.size.width)
            != u64::from(target.size.width) * u64::from(source.size.width)
            || u64::from(plane.size.height) * u64::from(image.size.height)
                != u64::from(target.size.height) * u64::from(source.size.height);
        let filtered = !province
            && mask == [true; 4]
            && !image.text
            && self.canvas_limit.is_some()
            && image.canvas
            && density_changes;
        if filtered
            && let Some(bounds) =
                crate::scene::raster::Raster::new(source.size, plane.size, (0, 0))?.rect(src)
        {
            // Converting an uploaded sprite into a reduced effect canvas is a
            // resample, not an integer bitmap copy. Keep straight-alpha edges
            // weighted so invisible RGB cannot contaminate the new pixels.
            draw.operation[0] = krkr_protocol::graphics::Blend::Alpha as i32 as f32;
            draw.sampling = Some(Sample {
                region: plane.size.rect(),
                bounds,
                linear: true,
                display: true,
                sharpen: false,
                clear: false,
                scale: None,
            });
            if plane.tiles.len() == 1 {
                self.draw_image(image, false, Some(plane), dst, &draw)?;
            } else {
                let mut stored = source.shared_main();
                stored.size = plane.size;
                if let Some((area, draw)) = self.raster_draw(image.size, target.size, dst, &draw)? {
                    self.affine_parts(target, &stored, area, &draw, &mut None)?;
                }
            }
        } else {
            self.draw_image(image, province, Some(plane), dst, &draw)?;
        }
        if !province
            && mask == [true; 4]
            && src == source.size.rect()
            && dst == image.size.rect()
            && let Some(color) = plane.tiles.first().and_then(|t| t.texture.solid_color())
            && plane
                .tiles
                .iter()
                .all(|t| t.texture.solid_color() == Some(color))
        {
            // Materializing a virtual solid resamples one texel across the
            // canvas. Preserve that known background for later clipped clears.
            for tile in &image.plane(false)?.tiles {
                tile.texture.cleared(tile.texture.size.rect(), color);
            }
        }
        if let (Some(key), Some(version)) = (memo, version) {
            self.copied_images.borrow_mut().insert(
                version,
                target_version,
                key,
                image,
                &self.staging,
            );
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn operate(
        &self,
        image: &mut Image,
        source: &Image,
        rectangle: Rect,
        x: i32,
        y: i32,
        clip: Rect,
        options: BlendOptions,
    ) -> Result<()> {
        self.check_image(source)?;
        self.check_image(image)?;
        if options.is_noop() {
            return Ok(());
        }
        if !options.accepts_face() {
            return Err(Error::Message("blend mode does not accept the draw face"));
        }
        if options.mode == krkr_protocol::graphics::Blend::Opaque
            && options.opacity == 255
            && options.face == DrawFace::Opaque
        {
            return self.copy_rect(
                image,
                source,
                rectangle,
                x,
                y,
                clip,
                options.face,
                options.hold_alpha,
            );
        }
        let Some((src, dst)) = blit::region(source.size, image.size, clip, rectangle, x, y) else {
            return Ok(());
        };
        let plane = source.plane(false)?;
        image.text |= source.text;
        self.writable_compact(image, dst, false)?;
        let sx = plane.size.width as f32 / source.size.width as f32;
        let sy = plane.size.height as f32 / source.size.height as f32;
        // A drawn canvas must use the same logical-pixel sampling as its
        // scene node. Otherwise committing a compact dialogue line into its
        // parent picks different rows at fractional display offsets.
        let canvas_sample = source.canvas && image.canvas && plane.size != source.size;
        self.draw_image(
            image,
            false,
            Some(plane),
            dst,
            &Draw {
                kind: if options.mode == krkr_protocol::graphics::Blend::Opaque
                    && options.opacity == 255
                {
                    if options.face == DrawFace::Opaque {
                        0.
                    } else {
                        6.
                    }
                } else {
                    self.display_blend_kind(options).unwrap_or(1.)
                },
                color: [0.; 4],
                operation: [
                    options.mode as i32 as f32,
                    face(options.face),
                    options.opacity as f32,
                    f32::from(options.hold_alpha),
                ],
                mask: [
                    true,
                    true,
                    true,
                    !(options.mode == krkr_protocol::graphics::Blend::Opaque
                        && options.opacity == 255
                        && options.face == DrawFace::Opaque
                        && options.hold_alpha),
                ],
                mapping: if canvas_sample {
                    [
                        1.,
                        0.,
                        (src.left - dst.left) as f32,
                        0.,
                        1.,
                        (src.top - dst.top) as f32,
                    ]
                } else {
                    [
                        sx,
                        0.,
                        (src.left - dst.left) as f32 * sx + (sx - 1.) * 0.5,
                        0.,
                        sy,
                        (src.top - dst.top) as f32 * sy + (sy - 1.) * 0.5,
                    ]
                },
                sampling: canvas_sample.then_some(Sample {
                    region: source.size.rect(),
                    bounds: src,
                    linear: false,
                    display: false,
                    sharpen: false,
                    clear: false,
                    scale: Some([sx, sy]),
                }),
            },
        )
    }
    /// Admission follows the clipped clear or blend actually used by colorRect.
    pub fn color_write_bytes(
        &self,
        image: &Image,
        rectangle: Rect,
        color: u32,
        opacity: i16,
        face: DrawFace,
    ) -> usize {
        if let Some(fill) = color_fill(rectangle, color, opacity, face) {
            self.fill_write_bytes(image, &[fill])
        } else if opacity == 0 {
            0
        } else {
            self.canvas_blend_write_bytes(image, rectangle, false)
        }
    }
    pub fn color(
        &self,
        image: &mut Image,
        rectangle: Rect,
        color: u32,
        opacity: i16,
        draw_face: DrawFace,
    ) -> Result<()> {
        self.check_image(image)?;
        if let Some(fill) = color_fill(rectangle, color, opacity, draw_face) {
            return self.fill(image, &[fill]);
        }
        if opacity == 0 {
            return Ok(());
        }
        if !(-255..=255).contains(&opacity) {
            return Err(Error::Message("color opacity is outside byte range"));
        }
        let Some(area) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        self.writable_compact(image, area, false)?;
        self.draw_image(
            image,
            false,
            None,
            area,
            &Draw {
                kind: 2.,
                color: rgba(color),
                operation: [0., face(draw_face), opacity as f32, 0.],
                mask: [true; 4],
                mapping: [0.; 6],
                sampling: None,
            },
        )
    }
    pub(crate) fn draw_image(
        &self,
        image: &Image,
        province: bool,
        source: Option<&Plane>,
        area: Rect,
        draw: &Draw,
    ) -> Result<()> {
        let plane = image.plane(province)?;
        if let Some((area, draw)) = self.raster_draw(image.size, plane.size, area, draw)? {
            self.draw(plane, source, area, &draw)?;
        }
        Ok(())
    }
    pub(crate) fn raster_draw(
        &self,
        logical: Size,
        stored: Size,
        area: Rect,
        draw: &Draw,
    ) -> Result<Option<(Rect, Draw)>> {
        if stored == logical {
            return Ok(Some((area, draw.clone())));
        }
        let Some(area) = crate::scene::raster::Raster::new(logical, stored, (0, 0))?.rect(area)
        else {
            return Ok(None);
        };
        let sx = logical.width as f32 / stored.width as f32;
        let sy = logical.height as f32 / stored.height as f32;
        let mut draw = draw.clone();
        let [a, b, tx, c, d, ty] = draw.mapping;
        draw.mapping = [
            a * sx,
            b * sy,
            tx + a * (sx - 1.) * 0.5 + b * (sy - 1.) * 0.5,
            c * sx,
            d * sy,
            ty + c * (sx - 1.) * 0.5 + d * (sy - 1.) * 0.5,
        ];
        Ok(Some((area, draw)))
    }
    pub(crate) fn draw(
        &self,
        target: &Plane,
        source: Option<&Plane>,
        area: Rect,
        draw: &Draw,
    ) -> Result<()> {
        let _draw_state = self.device.draw_state.scope();
        let mut programs = [[None, None], [None, None]];
        for tile in &target.tiles {
            let Some(part) = area.intersection(tile.rectangle) else {
                continue;
            };
            // Clears and conservative write bounds let us replace the previous
            // pixel with an exact byte constant. No resolve, scratch copy or
            // backdrop texture fetch is needed until this region is modified.
            let color = matches!(draw.kind, 1. | 2. | 5.)
                .then(|| {
                    tile.solid_region(Rect {
                        left: part.left - tile.rectangle.left,
                        top: part.top - tile.rectangle.top,
                        ..part
                    })
                })
                .flatten();
            // SGX classifies any fragment-killing shader as punch-through.
            // One fully covered raw source needs no kill: retain the opaque
            // HSR path. Multi-tile seams and uncertain edges keep rejection.
            let covered =
                source.is_some_and(|s| s.tiles.len() == 1) && overwrites(draw, source, part);
            let program = &mut programs[usize::from(color.is_some())][usize::from(covered)];
            if program.is_none() {
                *program = Some(
                    self.program
                        .select_covered(draw, color.is_some(), covered)?,
                );
            }
            let program = program.as_ref().unwrap();
            let reads_destination = color.is_none() && matches!(draw.kind, 1. | 2. | 4. | 5.);
            let backing = if reads_destination {
                self.device.backdrop(
                    &tile.texture,
                    Rect {
                        left: part.left - tile.rectangle.left,
                        top: part.top - tile.rectangle.top,
                        ..part
                    },
                )?
            } else {
                None
            };
            let copy = if reads_destination && backing.is_none() {
                let copy = self.device.texture(
                    Size {
                        width: part.width,
                        height: part.height,
                    },
                    &self.scratch,
                )?;
                self.device.copy_region(
                    &tile.texture,
                    Rect {
                        left: part.left - tile.rectangle.left,
                        top: part.top - tile.rectangle.top,
                        ..part
                    },
                    &copy,
                )?;
                Some(copy)
            } else {
                None
            };
            let overwrite = self.device.streamed_uploads() && overwrites(draw, source, part);
            self.bind_target(program, tile, part, draw, overwrite)?;
            if let Some(color) = color {
                program.four("u_backdrop_color", rgba(color));
            }
            if reads_destination {
                let previous = if let Some(backing) = backing {
                    // backdrop() resolved the old contents before bind_target()
                    // marked this pass dirty. Do not resolve the upcoming write.
                    unsafe {
                        self.device.gl.active_texture(glow::TEXTURE1);
                        self.device.gl.bind_texture(glow::TEXTURE_2D, Some(backing));
                    }
                    tile.rectangle
                } else {
                    self.bind_texture(1, copy.as_deref().unwrap_or(&self.lookup))?;
                    part
                };
                program.two(
                    "u_backdrop_origin",
                    previous.left as f32,
                    previous.top as f32,
                );
                program.two(
                    "u_backdrop_size",
                    previous.width as f32,
                    previous.height as f32,
                );
            }
            if let Some(source) = source {
                let all_tiles = source.tiles.len() == 1
                    || draw.kind == 7.
                    || draw
                        .sampling
                        .as_ref()
                        .is_some_and(|sample| sample.scale.is_none());
                let footprint = (!all_tiles)
                    .then(|| {
                        source_footprint(
                            part,
                            draw.mapping,
                            source.size,
                            draw.sampling.as_ref().and_then(|sample| sample.scale),
                        )
                    })
                    .flatten();
                for input in &source.tiles {
                    if all_tiles
                        || footprint
                            .is_some_and(|area| area.intersection(input.rectangle).is_some())
                    {
                        if source.tiles.len() > 1
                            && draw.kind != 7.
                            && draw
                                .sampling
                                .as_ref()
                                .is_none_or(|s| s.scale.is_some() && !s.linear && !s.clear)
                        {
                            let Some(coverage) = crate::draw_bounds::tile_area(
                                part,
                                draw.mapping,
                                draw.sampling.as_ref().and_then(|s| s.scale),
                                input.rectangle,
                            ) else {
                                continue;
                            };
                            program.four("u_rectangle", rect(coverage));
                        }
                        self.draw_source(
                            program,
                            input,
                            draw.sampling.as_ref().is_some_and(|s| s.display),
                        )?;
                    }
                }
            } else {
                if program.uses("u_source") {
                    self.bind_texture(0, &self.lookup)?;
                }
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
            }
            self.device.check()?;
        }
        Ok(())
    }
    pub(crate) fn bind_target(
        &self,
        program: &crate::shader::Program,
        tile: &Tile,
        area: Rect,
        draw: &Draw,
        overwrite: bool,
    ) -> Result<()> {
        program.bind();
        let local = Rect {
            left: area.left - tile.rectangle.left,
            top: area.top - tile.rectangle.top,
            ..area
        };
        let framebuffer = if overwrite {
            tile.texture.overwrite_framebuffer(local)?
        } else {
            tile.texture.framebuffer_region(local)?
        };
        unsafe {
            let gl = &self.device.gl;
            // PVR submits the old surface even on a same-name bind. Multiple
            // draws into one canvas must stay in the same tile render.
            if gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) as u32 != framebuffer.0.get() {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            }
            gl.viewport(
                0,
                0,
                tile.rectangle.width as i32,
                tile.rectangle.height as i32,
            );
            gl.disable(glow::SCISSOR_TEST);
            if matches!(draw.kind, 9. | 10. | 11.) {
                gl.enable(glow::BLEND);
                gl.blend_equation(glow::FUNC_ADD);
                if draw.kind == 10. {
                    // AddAlpha stores premultiplied RGB. On an opaque draw
                    // face the legacy operation replaces alpha with source
                    // alpha, rather than applying source-over to that channel.
                    gl.blend_func_separate(
                        glow::ONE,
                        glow::ONE_MINUS_SRC_ALPHA,
                        glow::ONE,
                        glow::ZERO,
                    );
                } else if draw.kind == 11. {
                    gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
                } else {
                    gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                }
            } else {
                gl.disable(glow::BLEND);
            }
            gl.color_mask(draw.mask[0], draw.mask[1], draw.mask[2], draw.mask[3]);
        }
        program.four("u_target", rect(tile.rectangle));
        program.four("u_rectangle", rect(area));
        program.one("u_flip", 1.);
        program.one("u_kind", draw.kind);
        program.four("u_operation", draw.operation);
        program.four("u_color", draw.color);
        program.three("u_map_x", draw.mapping[..3].try_into().unwrap());
        program.three("u_map_y", draw.mapping[3..].try_into().unwrap());
        if let Some(sample) = &draw.sampling {
            program.four(
                "u_sampling",
                [
                    if sample.scale.is_some() { 2. } else { 1. },
                    f32::from(sample.linear),
                    f32::from(sample.clear),
                    f32::from(draw.mapping[3] == 0.),
                ],
            );
            program.four("u_region", edges(sample.region));
            program.four("u_sample_bounds", edges(sample.bounds));
            if let Some(scale) = sample.scale {
                program.two("u_source_scale", scale[0], scale[1]);
            }
        } else {
            program.four("u_sampling", [0.; 4]);
        }
        if program.uses("u_lookup") {
            self.bind_texture(2, &self.lookup)?;
        }
        Ok(())
    }
    fn draw_source(
        &self,
        program: &crate::shader::Program,
        tile: &Tile,
        linear: bool,
    ) -> Result<()> {
        self.bind_texture(0, &tile.texture)?;
        if linear {
            self.source_filter(glow::LINEAR);
        }
        let source = tile.sample_rectangle();
        program.four("u_source_visible", rect(tile.rectangle));
        program.two("u_source_origin", source.left as f32, source.top as f32);
        program.two("u_source_size", source.width as f32, source.height as f32);
        unsafe {
            self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        if linear {
            // ES2 filtering belongs to the texture object. Do not change a
            // later script copy, lookup, or province sample through an alias.
            self.source_filter(glow::NEAREST);
        }
        Ok(())
    }
    fn source_filter(&self, filter: u32) {
        unsafe {
            for parameter in [glow::TEXTURE_MIN_FILTER, glow::TEXTURE_MAG_FILTER] {
                self.device
                    .gl
                    .tex_parameter_i32(glow::TEXTURE_2D, parameter, filter as i32);
            }
        }
    }
    pub(crate) fn bind_texture(&self, unit: u32, texture: &Texture) -> Result<()> {
        self.device.before_sample(texture)?;
        unsafe {
            self.device.gl.active_texture(glow::TEXTURE0 + unit);
            self.device
                .gl
                .bind_texture(glow::TEXTURE_2D, Some(texture.name()));
        }
        Ok(())
    }
    /// Draw a logical image into the host-provided physical viewport. Internal
    /// FBOs store row zero at GL row zero; only display presentation flips Y.
    pub fn present(&self, image: &Image, physical: Size, destination: Rect) -> Result<()> {
        self.clear_display(physical)?;
        self.present_window(image, physical, destination, physical.rect())?;
        self.flush()
    }
    pub fn clear_display(&self, physical: Size) -> Result<()> {
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, physical.width as i32, physical.height as i32);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.clear_color(0., 0., 0., 1.);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
        self.device.check()
    }
    /// Paint one client canvas over previous windows, with display-space clipping.
    /// The caller clears once and flushes after the final window or cursor.
    pub fn present_window(
        &self,
        image: &Image,
        physical: Size,
        destination: Rect,
        clip: Rect,
    ) -> Result<()> {
        self.present_overlay(image, physical, destination, clip, false)
    }
    pub fn present_cursor(&self, image: &Image, physical: Size, destination: Rect) -> Result<()> {
        self.present_overlay(image, physical, destination, physical.rect(), true)
    }
    fn present_overlay(
        &self,
        image: &Image,
        physical: Size,
        destination: Rect,
        clip: Rect,
        alpha: bool,
    ) -> Result<()> {
        self.check_image(image)?;
        if physical.width == 0 || physical.height == 0 {
            return Ok(());
        }
        let source = image.plane(false)?;
        if destination.width == 0 || destination.height == 0 {
            return Ok(());
        }
        let Some(clip) = physical.rect().intersection(clip) else {
            return Ok(());
        };
        let program = self.program.raw();
        program.bind();
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.viewport(0, 0, physical.width as i32, physical.height as i32);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(
                clip.left,
                physical.height as i32 - clip.top - clip.height as i32,
                clip.width as i32,
                clip.height as i32,
            );
            if alpha {
                gl.enable(glow::BLEND);
                gl.blend_equation(glow::FUNC_ADD);
                gl.blend_func_separate(
                    glow::SRC_ALPHA,
                    glow::ONE_MINUS_SRC_ALPHA,
                    glow::ONE,
                    glow::ONE_MINUS_SRC_ALPHA,
                );
            } else {
                gl.disable(glow::BLEND);
            }
            gl.color_mask(true, true, true, true);
        }
        program.four("u_target", rect(physical.rect()));
        program.four("u_rectangle", rect(destination));
        program.one("u_flip", -1.);
        program.one("u_kind", 0.);
        program.four("u_sampling", [0.; 4]);
        let sx = source.size.width as f32 / destination.width as f32;
        let sy = source.size.height as f32 / destination.height as f32;
        let mapping = [
            sx,
            0.,
            -destination.left as f32 * sx + (sx - 1.) * 0.5,
            0.,
            sy,
            -destination.top as f32 * sy + (sy - 1.) * 0.5,
        ];
        program.three("u_map_x", mapping[..3].try_into().unwrap());
        program.three("u_map_y", mapping[3..].try_into().unwrap());
        // The raw presentation program has only one active sampler. Unused
        // lookup/backdrop bindings do not need per-window driver validation.
        for tile in &source.tiles {
            if source.tiles.len() > 1 {
                let Some(area) = destination.intersection(clip).and_then(|area| {
                    crate::draw_bounds::tile_area(area, mapping, None, tile.rectangle)
                }) else {
                    continue;
                };
                program.four("u_rectangle", rect(area));
            }
            self.draw_source(program, tile, false)?;
        }
        self.device.check()
    }
}
fn edges(rect: Rect) -> [f32; 4] {
    [
        rect.left as f32,
        rect.top as f32,
        (i64::from(rect.left) + i64::from(rect.width)) as f32,
        (i64::from(rect.top) + i64::from(rect.height)) as f32,
    ]
}

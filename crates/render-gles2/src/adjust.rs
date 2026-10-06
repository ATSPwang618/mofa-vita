use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{Draw, rect, rgba},
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::graphics::{Adjustment, Rect, Size};
use std::{
    rc::Rc,
    sync::{Arc, Weak},
};

type GammaTable = [[u32; 4]; 256];
const GAMMA_BYTES: usize = 256 * 3 * 4;

fn same_gamma(owner: &Weak<GammaTable>, table: &Arc<GammaTable>) -> bool {
    owner
        .upgrade()
        .is_some_and(|old| Arc::ptr_eq(&old, table) || old == *table)
}

#[derive(Default)]
pub(crate) struct Renderer {
    programs: [Option<Program>; 5],
    gamma: Option<(Weak<GammaTable>, Rc<Texture>)>,
}

enum PointTile {
    Solid,
    InPlace,
    Source(Rc<Texture>),
}

fn word(value: u32) -> [f32; 4] {
    value.to_le_bytes().map(f32::from)
}

// Evaluate only metadata-proven constant tiles. These integer formulas match
// Point shaders, including transparent RGB and additive-alpha gamma overflow.
fn solid_point(color: u32, operation: &Adjustment) -> Option<u32> {
    let a = color >> 24;
    let mut rgb = [color >> 16 & 255, color >> 8 & 255, color & 255];
    match operation {
        Adjustment::GrayScale => {
            let gray = (rgb[0] * 54 + rgb[1] * 183 + rgb[2] * 19) >> 8;
            rgb = [gray; 3];
        }
        Adjustment::Gamma { table, additive } => {
            if !additive && a == 0 {
                return Some(color);
            }
            for (channel, value) in rgb.iter_mut().enumerate() {
                *value = if !additive || a == 255 {
                    table[*value as usize][channel]
                } else {
                    let index = if *value <= a {
                        (((65536 / a.max(1)).min(65535) * *value) >> 8).min(255)
                    } else {
                        255
                    };
                    (table[index as usize][channel].wrapping_mul(a + a / 128) >> 8)
                        + value.saturating_sub(a)
                }
                .min(255);
            }
        }
        _ => return None,
    }
    Some(a << 24 | rgb[0] << 16 | rgb[1] << 8 | rgb[2])
}

impl Gpu {
    fn direct_point(&self, image: &Image, area: Rect, operation: &Adjustment) -> bool {
        self.device.streamed_uploads()
            && area == image.size.rect()
            && matches!(operation, Adjustment::GrayScale | Adjustment::Gamma { .. })
            && image.main.as_ref().is_some_and(|p| {
                p.tiles.iter().all(|t| {
                    t.texture.solid_color().is_some()
                        || (t.backing.is_none() && t.texture.size == t.size())
                })
            })
    }
    fn point_write_bytes(&self, image: &Image, area: Rect, operation: &Adjustment) -> usize {
        if !self.direct_point(image, area, operation) {
            return image.stored_write_bytes(false);
        }
        let plane = image.main.as_ref().unwrap();
        plane.tiles.iter().fold(0usize, |bytes, tile| {
            bytes.saturating_add(if let Some(color) = tile.texture.solid_color() {
                if solid_point(color, operation) == Some(color) {
                    0
                } else {
                    4
                }
            } else if !tile.renderable()
                || Rc::strong_count(plane) > 1
                || Rc::strong_count(&tile.texture) > 1
            {
                tile.size().rgba_bytes().unwrap_or(usize::MAX)
            } else {
                0
            })
        })
    }
    // Keep constant margins virtual, detach only real pixels, and allocate all
    // replacements before publishing. A source alias must retain its old colors.
    fn writable_point(
        &self,
        image: &mut Image,
        area: Rect,
        operation: &Adjustment,
    ) -> Result<Option<Vec<PointTile>>> {
        if !self.direct_point(image, area, operation) {
            self.writable(image, area, false)?;
            return Ok(None);
        }
        let plane = image.plane(false)?;
        let mut replacements = Vec::new();
        let mut sources = Vec::with_capacity(plane.tiles.len());
        for (index, tile) in plane.tiles.iter().enumerate() {
            if let Some(color) = tile.texture.solid_color() {
                sources.push(PointTile::Solid);
                let result = solid_point(color, operation).unwrap();
                if result != color {
                    replacements.push((index, self.canvas_solid_tile(tile.size(), result, true)?));
                }
            } else if !tile.renderable()
                || Rc::strong_count(plane) > 1
                || Rc::strong_count(&tile.texture) > 1
            {
                let texture = self.device.sample_texture(tile.size(), &plane.budget)?;
                // Every output pixel is produced by the point kernel.
                // Sample the immutable original directly instead of first
                // copying it into the replacement only to overwrite it.
                sources.push(PointTile::Source(tile.texture.clone()));
                replacements.push((index, texture));
            } else {
                sources.push(PointTile::InPlace);
            }
        }
        if !replacements.is_empty() {
            let plane = Rc::make_mut(image.main.as_mut().unwrap());
            for (index, texture) in replacements {
                plane.tiles[index].texture = texture;
                plane.tiles[index].backing = None;
            }
        }
        Ok(Some(sources))
    }
    fn point_canvas_size(&self, image: &Image, area: Rect, operation: &Adjustment) -> Option<Size> {
        use krkr_protocol::filter::Kind;
        if !image.canvas
            || self.canvas_limit.is_none()
            || image.stored_size()
                == Some(Size {
                    width: 1,
                    height: 1,
                })
            || area != image.size.rect()
            || !matches!(
                operation,
                Adjustment::Gamma { .. }
                    | Adjustment::GrayScale
                    | Adjustment::Filter(krkr_protocol::filter::Filter {
                        kind: Kind::Lookup
                            | Kind::Colorize { .. }
                            | Kind::Modulate { .. }
                            | Kind::Xor { .. },
                        ..
                    })
            )
        {
            return None;
        }
        let size = self.canvas_storage(image.size, Some(image));
        (image.stored_size() != Some(size)).then_some(size)
    }
    // Whole-image point kernels commute with nearest-neighbor expansion.
    // Keep converted assets at their stored size; clipped or spatial kernels
    // still need logical pixels to preserve their boundaries and coordinates.
    fn compact_adjustment(&self, image: &Image, area: Rect, operation: &Adjustment) -> bool {
        use krkr_protocol::filter::Kind;
        area.intersection(image.size.rect()) == Some(image.size.rect())
            && image.stored_size().is_some_and(|size| {
                size != image.size
                    && size.width <= image.size.width
                    && size.height <= image.size.height
            })
            && matches!(
                operation,
                Adjustment::Gamma { .. }
                    | Adjustment::GrayScale
                    | Adjustment::Filter(krkr_protocol::filter::Filter {
                        kind: Kind::Lookup
                            | Kind::Colorize { .. }
                            | Kind::Modulate { .. }
                            | Kind::Xor { .. },
                        ..
                    })
            )
    }

    pub fn adjust_write_bytes(&self, image: &Image, area: Rect, operation: &Adjustment) -> usize {
        let Some(area) = area.intersection(image.size.rect()) else {
            return 0;
        };
        if let Some(key) = crate::adjust_cache::Key::new(operation)
            && self
                .adjusted_images
                .borrow_mut()
                .get(image, area, &key)
                .is_some()
        {
            return 0;
        }
        if let Some(size) = self.point_canvas_size(image, area, operation) {
            size.rgba_bytes().unwrap_or(usize::MAX)
        } else if self.compact_adjustment(image, area, operation)
            || (self.direct_point(image, area, operation)
                && image.stored_size() == Some(image.size))
        {
            self.point_write_bytes(image, area, operation)
        } else if matches!(operation, Adjustment::Flip { .. }) {
            if area == image.size.rect()
                && !image.has_province()
                && self.solid_images.borrow().color(image).is_some()
            {
                return 0;
            }
            let size = if area == image.size.rect() && image.canvas {
                self.canvas_storage(image.size, Some(image))
            } else {
                image.size
            };
            size.rgba_bytes()
                .unwrap_or(usize::MAX)
                .saturating_add(if image.has_province() {
                    image.size.rgba_bytes().unwrap_or(usize::MAX)
                } else {
                    0
                })
        } else if let Adjustment::BoxBlur { radius, .. } = operation {
            if let Some(plan) = self.stream_blur_plan(image, area, *radius) {
                return plan.bytes;
            }
            self.box_output_bytes(image, area, *radius)
        } else {
            image.write_bytes(false)
        }
    }

    pub fn adjust_upload_bytes(&self, operation: &Adjustment) -> usize {
        if let Adjustment::Filter(filter) = operation {
            return self.filter_upload_bytes(filter);
        }
        let Adjustment::Gamma { table, .. } = operation else {
            return 0;
        };
        if self
            .adjustments
            .borrow()
            .gamma
            .as_ref()
            .is_some_and(|(owner, _)| same_gamma(owner, table))
        {
            0
        } else {
            GAMMA_BYTES
        }
    }

    pub fn collect_adjustment_tables(&self) {
        self.adjusted_images.borrow_mut().trim();
        self.collect_filter_tables();
        let mut renderer = self.adjustments.borrow_mut();
        if renderer
            .gamma
            .as_ref()
            .is_some_and(|(owner, _)| owner.strong_count() == 0)
        {
            renderer.gamma.take();
        }
    }

    pub fn adjust(&self, image: &mut Image, rectangle: Rect, operation: &Adjustment) -> Result<()> {
        let _profile = krkr_protocol::profile::span_detail("gpu.adjust", || {
            let kind = match operation {
                Adjustment::BoxBlur { radius, alpha } => {
                    format!("BoxBlur radius={radius:?} alpha={alpha}")
                }
                Adjustment::Gamma { additive, .. } => format!("Gamma additive={additive}"),
                Adjustment::Filter(filter) => format!("Filter {:?}", filter.kind),
                Adjustment::GrayScale => "GrayScale".into(),
                Adjustment::Flip { horizontal } => format!("Flip horizontal={horizontal}"),
                Adjustment::Lines(_) => "Lines".into(),
                Adjustment::Gradient { .. } => "Gradient".into(),
                Adjustment::ColorField { .. } => "ColorField".into(),
            };
            format!(
                "{kind} logical={:?} stored={:?} area={rectangle:?}",
                image.size,
                image.stored_size()
            )
        });
        self.check_image(image)?;
        image.plane(false)?;
        let Some(rectangle) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        let key = (image.plane(false)?.tiles.len() <= 64)
            .then(|| crate::adjust_cache::Key::new(operation))
            .flatten();
        if let Some(key) = key.as_ref()
            && let Some(plane) = self.adjusted_images.borrow_mut().get(image, rectangle, key)
        {
            image.main = Some(plane);
            return Ok(());
        }
        let source = key
            .as_ref()
            .map(|_| crate::scene_damage::Version::capture(image))
            .transpose()?;
        self.adjust_inner(image, rectangle, operation)?;
        if let (Some(key), Some(source)) = (key, source) {
            self.adjusted_images
                .borrow_mut()
                .insert(key, rectangle, source, image, &self.staging);
        }
        Ok(())
    }
    fn adjust_inner(
        &self,
        image: &mut Image,
        rectangle: Rect,
        operation: &Adjustment,
    ) -> Result<()> {
        if let Some(size) = self.point_canvas_size(image, rectangle, operation) {
            let mut next = self.reserve_full_upload(size)?;
            next.size = image.size;
            next.canvas = true;
            let source = image.plane(false)?;
            let sx = source.size.width as f32 / size.width as f32;
            let sy = source.size.height as f32 / size.height as f32;
            self.draw(
                next.plane(false)?,
                Some(source),
                size.rect(),
                &Draw::copy(
                    [sx, 0., (sx - 1.) * 0.5, 0., sy, (sy - 1.) * 0.5],
                    [true; 4],
                ),
            )?;
            // Apply the point function to these exact stored samples, then
            // restore the unchanged logical coordinates and province plane.
            let logical = next.size;
            next.size = size;
            next.canvas = false;
            self.adjust_inner(&mut next, size.rect(), operation)?;
            next.size = logical;
            image.main = next.main;
            return Ok(());
        }
        if self.compact_adjustment(image, rectangle, operation) {
            let logical = image.size;
            let canvas = image.canvas;
            image.size = image.stored_size().unwrap();
            image.canvas = false;
            let result = self.adjust_inner(image, image.size.rect(), operation);
            image.size = logical;
            image.canvas = canvas;
            return result;
        }
        let Some(area) = rectangle.intersection(image.size.rect()) else {
            return Ok(());
        };
        if let Adjustment::Flip { horizontal } = operation {
            return self.flip_image(image, area, *horizontal);
        }
        if let Adjustment::Lines(lines) = operation {
            return self.draw_lines(image, area, lines);
        }
        if let Adjustment::BoxBlur { radius, alpha } = operation {
            return self.box_blur(image, area, *radius, *alpha);
        }
        if let Adjustment::Filter(filter) = operation {
            return self.image_filter(image, area, filter);
        }
        let (kind, reads_old) = match operation {
            Adjustment::GrayScale => (0, true),
            Adjustment::Gamma { additive, .. } => (if *additive { 4 } else { 3 }, true),
            Adjustment::Gradient { blend, .. } => (1, *blend),
            Adjustment::ColorField { values, hsv, .. } => {
                if *hsv && values.iter().any(|v| !(*v as f32).is_finite()) {
                    return Err(Error::Message("non-finite HSV field constant"));
                }
                (2, false)
            }
            _ => {
                return Err(Error::Message(
                    "GLES adjustment pass has not been connected",
                ));
            }
        };
        let mut renderer = self.adjustments.borrow_mut();
        if renderer.programs[kind].is_none() {
            renderer.programs[kind] = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &format!(
                    "#define ADJUST_KIND {kind}\n{}\n{}",
                    include_str!("integer.glsl"),
                    include_str!("adjust.frag")
                ),
            )?);
        }
        if let Adjustment::Gamma { table, .. } = operation
            && !renderer
                .gamma
                .as_ref()
                .is_some_and(|(owner, _)| same_gamma(owner, table))
        {
            renderer.gamma.take();
            let texture = self.device.sample_texture(
                Size {
                    width: 256,
                    height: 3,
                },
                &self.resident,
            )?;
            let staging = self.staging.reserve(GAMMA_BYTES)?;
            let mut bytes = [0; GAMMA_BYTES];
            for channel in 0..3 {
                for (index, entry) in table.iter().enumerate() {
                    let at = (channel * 256 + index) * 4;
                    bytes[at..at + 4].copy_from_slice(&entry[channel].to_le_bytes());
                }
            }
            self.device.upload(&texture, &bytes)?;
            drop(staging);
            renderer.gamma = Some((Arc::downgrade(table), texture));
        }
        if let Adjustment::Gamma { table, .. } = operation {
            renderer.gamma.as_mut().unwrap().0 = Arc::downgrade(table);
        }
        // A point operation needs only the old pixels it changes. Reuse one
        // texture across tiles and never allocate a full-image source copy.
        // The work framebuffer is distinct from the backing texture. Point
        // kernels can sample that backing directly after publishing old writes,
        // avoiding a tile-sized scratch image and its copy for every pass.
        let sources = self.writable_point(image, area, operation)?;
        let mut extent = Size {
            width: 0,
            height: 0,
        };
        if reads_old {
            for (index, tile) in image.plane(false)?.tiles.iter().enumerate() {
                if sources
                    .as_ref()
                    .is_some_and(|s| !matches!(s[index], PointTile::InPlace))
                    || (self.device.streamed_uploads()
                        && self.device.supports_work_draw(&tile.texture))
                {
                    continue;
                }
                if let Some(part) = area.intersection(tile.rectangle) {
                    extent.width = extent.width.max(part.width);
                    extent.height = extent.height.max(part.height);
                }
            }
        }
        // Reserve the single copy before drawing any tile. Shared immutable
        // sources and work backings need no temporary allocation.
        let previous = (extent.width != 0)
            .then(|| self.device.sample_texture(extent, &self.scratch))
            .transpose()?;
        let program = renderer.programs[kind].as_ref().unwrap();
        for (index, tile) in image.plane(false)?.tiles.iter().enumerate() {
            if sources
                .as_ref()
                .is_some_and(|s| matches!(s[index], PointTile::Solid))
            {
                continue;
            }
            let source = sources.as_ref().and_then(|s| match &s[index] {
                PointTile::Source(texture) => Some(texture.as_ref()),
                _ => None,
            });
            let Some(part) = area.intersection(tile.rectangle) else {
                continue;
            };
            let work_source =
                self.device.streamed_uploads() && self.device.supports_work_draw(&tile.texture);
            if reads_old && source.is_none() && !work_source {
                self.device.copy_region_at(
                    &tile.texture,
                    Rect {
                        left: part.left - tile.rectangle.left,
                        top: part.top - tile.rectangle.top,
                        ..part
                    },
                    previous.as_ref().unwrap(),
                    0,
                    0,
                )?;
            }
            let local = Rect {
                left: part.left - tile.rectangle.left,
                top: part.top - tile.rectangle.top,
                ..part
            };
            let background = (local == tile.texture.size.rect())
                .then(|| source.unwrap_or(&tile.texture).constant_background())
                .flatten()
                .and_then(|(color, damage)| solid_point(color, operation).map(|c| (c, damage)));
            let backing = if work_source && reads_old && source.is_none() {
                self.device.backdrop(&tile.texture, local)?
            } else {
                None
            };
            // Old pixels are sampled from a distinct texture/backdrop. Every
            // channel in this rectangle is replaced, so loading its previous
            // contents into the work surface adds only bandwidth.
            let framebuffer = tile.texture.overwrite_framebuffer(local)?;
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
                gl.disable(glow::BLEND);
                gl.disable(glow::SCISSOR_TEST);
                gl.color_mask(true, true, true, true);
            }
            program.four("u_target", rect(tile.rectangle));
            program.four("u_rectangle", rect(part));
            program.one("u_flip", 1.);
            let (old, origin) = if let Some(source) = source {
                self.bind_texture(0, source)?;
                (source, tile.rectangle)
            } else if let Some(backing) = backing {
                // overwrite_framebuffer marked the upcoming write. Bind the old
                // texture name without resolving those not-yet-drawn pixels.
                unsafe {
                    self.device.gl.active_texture(glow::TEXTURE0);
                    self.device.gl.bind_texture(glow::TEXTURE_2D, Some(backing));
                }
                (tile.texture.as_ref(), tile.rectangle)
            } else {
                let old = previous.as_deref().unwrap_or(&self.lookup);
                self.bind_texture(0, old)?;
                (old, part)
            };
            program.two("u_source_origin", origin.left as f32, origin.top as f32);
            program.two(
                "u_source_size",
                old.size.width as f32,
                old.size.height as f32,
            );
            self.bind_texture(
                2,
                renderer
                    .gamma
                    .as_ref()
                    .map_or(self.lookup.as_ref(), |(_, t)| t),
            )?;
            match operation {
                Adjustment::Gamma { .. } | Adjustment::GrayScale => {}
                Adjustment::Gradient {
                    bounds,
                    from,
                    to,
                    vertical,
                    blend,
                } => {
                    let (length, origin, tile_origin) = if *vertical {
                        (bounds.height, bounds.top, tile.rectangle.top)
                    } else {
                        (bounds.width, bounds.left, tile.rectangle.left)
                    };
                    program.two("u_operation", f32::from(*vertical), f32::from(*blend));
                    program.four("u_data0", rgba(*from));
                    program.four("u_data1", rgba(*to));
                    program.four("u_data2", word(length.wrapping_sub(1).max(1)));
                    program.four("u_data3", word(tile_origin.wrapping_sub(origin) as u32));
                    program.four("u_region", rect(area));
                }
                Adjustment::ColorField {
                    size,
                    hsv,
                    axes,
                    values,
                } => {
                    program.one("u_kind", f32::from(*hsv));
                    program.two("u_canvas", size.width as f32, size.height as f32);
                    program.three("u_axis", axes.map(|v| v as f32));
                    program.three(
                        "u_color",
                        values.map(|v| {
                            if *hsv {
                                v as f32
                            } else {
                                (v as i32 as u8) as f32
                            }
                        }),
                    );
                    program.four("u_data0", word(size.width.wrapping_sub(1).max(1)));
                    program.four("u_data1", word(size.height.wrapping_sub(1).max(1)));
                    program.four(
                        "u_data2",
                        word((tile.rectangle.left as u32).wrapping_mul(255)),
                    );
                    program.four(
                        "u_data3",
                        word(
                            size.height
                                .wrapping_sub(1)
                                .wrapping_sub(tile.rectangle.top as u32)
                                .wrapping_mul(255),
                        ),
                    );
                }
                _ => unreachable!(),
            }
            unsafe {
                self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            self.device.check()?;
            if let Some((color, damage)) = background {
                tile.texture.point_background(color, damage);
            }
        }
        Ok(())
    }

    fn flip_image(&self, image: &mut Image, area: Rect, horizontal: bool) -> Result<()> {
        if area == image.size.rect()
            && !image.has_province()
            && self.solid_images.borrow().color(image).is_some()
        {
            return Ok(());
        }
        // Both planes read the same immutable version. Commit after all writes,
        // so a failed province allocation cannot publish a half-flipped image.
        let mut next = image.shared();
        if area == image.size.rect() {
            let size = if image.canvas {
                self.canvas_storage(image.size, Some(image))
            } else {
                image.size
            };
            next.main = Some(self.overwrite_plane(size, &self.resident)?);
        } else {
            self.writable(&mut next, area, false)?;
        }
        if image.has_province() {
            if area == image.size.rect() {
                next.province = Some(self.overwrite_plane(image.size, &self.resident)?);
            } else {
                self.writable(&mut next, area, true)?;
            }
        }
        for province in [false, true] {
            if province && !image.has_province() {
                continue;
            }
            let source = image.plane(province)?;
            if area == image.size.rect() {
                let size = next.plane(province)?.size;
                let sx = source.size.width as f32 / size.width as f32;
                let sy = source.size.height as f32 / size.height as f32;
                let mapping = if horizontal {
                    [
                        -sx,
                        0.,
                        source.size.width as f32 - (sx + 1.) * 0.5,
                        0.,
                        sy,
                        (sy - 1.) * 0.5,
                    ]
                } else {
                    [
                        sx,
                        0.,
                        (sx - 1.) * 0.5,
                        0.,
                        -sy,
                        source.size.height as f32 - (sy + 1.) * 0.5,
                    ]
                };
                self.draw(
                    next.plane(province)?,
                    Some(source),
                    size.rect(),
                    &Draw::copy(mapping, [true; 4]),
                )?;
                continue;
            }
            let sx = source.size.width as f32 / image.size.width as f32;
            let sy = source.size.height as f32 / image.size.height as f32;
            let mapping = if horizontal {
                [
                    -sx,
                    0.,
                    (image.size.width as f32 - 0.5) * sx - 0.5,
                    0.,
                    sy,
                    (sy - 1.) * 0.5,
                ]
            } else {
                [
                    sx,
                    0.,
                    (sx - 1.) * 0.5,
                    0.,
                    -sy,
                    (image.size.height as f32 - 0.5) * sy - 0.5,
                ]
            };
            self.draw_image(
                &next,
                province,
                Some(source),
                area,
                &Draw::copy(mapping, [true; 4]),
            )?;
        }
        *image = next;
        Ok(())
    }
}

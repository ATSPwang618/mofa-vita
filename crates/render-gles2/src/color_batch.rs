use crate::{
    Error, Gpu, Image, Result,
    drawing::{Draw, color_fill, face, rect, rgba},
    image::Tile,
};
use glow::HasContext;
use krkr_protocol::graphics::{Color, DrawFace, Fill, PreparedDraw, Rect};

impl Gpu {
    /// Unique materialized regions accept writes without copy-on-write.
    /// Untouched constant/shared tiles remain outside the grant.
    pub fn color_batch_regions(&self, image: &Image) -> Option<Vec<Rect>> {
        let plane = image.plane(false).ok()?;
        if std::rc::Rc::strong_count(plane) != 1
            || plane.size
                != if image.canvas && self.canvas_limit.is_some() {
                    self.canvas_storage(image.size, Some(image))
                } else {
                    image.size
                }
            || plane.tiles.len() > krkr_protocol::graphics::DRAW_BATCH_CAPACITY
        {
            return None;
        }
        let mut regions = Vec::new();
        regions.try_reserve_exact(plane.tiles.len()).ok()?;
        regions.extend(
            plane
                .tiles
                .iter()
                .filter(|tile| {
                    tile.renderable()
                        && std::rc::Rc::strong_count(&tile.texture) == 1
                        && self.device.supports_work_draw(&tile.texture)
                })
                .map(|tile| tile.rectangle),
        );
        (!regions.is_empty()).then_some(regions)
    }
    pub fn prepared_draws(&self, image: &mut Image, draws: &[PreparedDraw]) -> Result<()> {
        use krkr_protocol::graphics::DRAW_BATCH_CAPACITY;
        if draws.len() > DRAW_BATCH_CAPACITY {
            return Err(Error::Message("prepared draw batch is too large"));
        }
        match draws.first() {
            None => Ok(()),
            Some(PreparedDraw::Fill(_)) => {
                let mut fills = [Fill {
                    rectangle: Rect::default(),
                    color: 0,
                    face: DrawFace::Alpha,
                    hold_alpha: false,
                }; DRAW_BATCH_CAPACITY];
                for (slot, draw) in fills.iter_mut().zip(draws) {
                    let PreparedDraw::Fill(fill) = draw else {
                        return Err(Error::Message(
                            "prepared fill batch contains another operation",
                        ));
                    };
                    if fill.face == DrawFace::Province {
                        return Err(Error::Message(
                            "prepared fill cannot allocate a province plane",
                        ));
                    }
                    *slot = *fill;
                }
                self.fill(image, &fills[..draws.len()])
            }
            Some(PreparedDraw::Color(_)) => {
                let mut colors = [Color {
                    rectangle: Rect::default(),
                    color: 0,
                    opacity: 0,
                    face: DrawFace::Alpha,
                }; DRAW_BATCH_CAPACITY];
                for (slot, draw) in colors.iter_mut().zip(draws) {
                    let PreparedDraw::Color(color) = draw else {
                        return Err(Error::Message(
                            "prepared color batch contains another operation",
                        ));
                    };
                    *slot = *color;
                }
                self.colors(image, &colors[..draws.len()])
            }
            _ => Err(Error::Message(
                "GLES did not issue a source-reading draw grant",
            )),
        }
    }
    pub fn colors(&self, image: &mut Image, colors: &[Color]) -> Result<()> {
        // Mixed clears and shared/cropped targets keep the ordinary path.
        // A clear can change the plane's representation, so reevaluate it
        // rather than retaining texture references across a fill.
        self.check_image(image)?;
        if colors.is_empty() {
            return Ok(());
        }
        if colors.len() > krkr_protocol::graphics::DRAW_BATCH_CAPACITY {
            for chunk in colors.chunks(krkr_protocol::graphics::DRAW_BATCH_CAPACITY) {
                self.colors(image, chunk)?;
            }
            return Ok(());
        }
        let raster =
            crate::scene::raster::Raster::new(image.size, image.plane(false)?.size, (0, 0))?;
        let area = |c: &Color| {
            (c.opacity != 0)
                .then(|| {
                    c.rectangle
                        .intersection(image.size.rect())
                        .and_then(|r| raster.rect(r))
                })
                .flatten()
        };
        let mut areas = [None; krkr_protocol::graphics::DRAW_BATCH_CAPACITY];
        for (slot, color) in areas.iter_mut().zip(colors) {
            *slot = area(color);
        }
        let areas = &areas[..colors.len()];
        if self.color_batch_regions(image).is_none()
            || colors.iter().any(|c| {
                !(-255..=255).contains(&c.opacity)
                    || self.color_write_bytes(image, c.rectangle, c.color, c.opacity, c.face) != 0
                    || color_fill(c.rectangle, c.color, c.opacity, c.face).is_some()
            })
            || image.plane(false)?.tiles.iter().any(|tile| {
                (!tile.renderable() || !self.device.supports_work_draw(&tile.texture))
                    && areas
                        .iter()
                        .flatten()
                        .any(|r| r.intersection(tile.rectangle).is_some())
            })
        {
            for c in colors {
                self.color(image, c.rectangle, c.color, c.opacity, c.face)?;
            }
            return Ok(());
        }
        let draw = |c: &Color| Draw {
            kind: 2.,
            color: rgba(c.color),
            operation: [0., face(c.face), c.opacity as f32, 0.],
            mask: [true; 4],
            mapping: [0.; 6],
            sampling: None,
        };
        let mut programs = [None, None, None, None, None];
        for tile in &image.plane(false)?.tiles {
            let bounds = areas
                .iter()
                .filter_map(|r| r.and_then(|r| r.intersection(tile.rectangle)))
                .reduce(|a, b| crate::scene_damage::union(Some(a), b));
            let Some(bounds) = bounds else { continue };
            let local = |r: Rect| Rect {
                left: r.left - tile.rectangle.left,
                top: r.top - tile.rectangle.top,
                ..r
            };
            let framebuffer = self
                .device
                .prepare_work_draw(&tile.texture, local(bounds))?
                .ok_or(Error::Message("color batch requires a work surface"))?;
            self.device.backdrop(&tile.texture, local(bounds))?;
            if self.color_strips(tile, colors, areas, framebuffer)? {
                continue;
            }
            for (c, area) in colors.iter().zip(areas) {
                let Some(part) = area.and_then(|r| r.intersection(tile.rectangle)) else {
                    continue;
                };
                if !(-255..=255).contains(&c.opacity) {
                    return Err(Error::Message("color opacity is outside byte range"));
                }
                let draw = draw(c);
                let program = &mut programs[face(c.face) as usize];
                if program.is_none() {
                    *program = Some(self.program.select(&draw)?);
                }
                let program = program.as_ref().unwrap();
                // Independent columns sample the same backing. Only a real
                // overlap publishes earlier writes, preserving byte-exact
                // alpha arithmetic and script order even after raster scaling.
                self.device.backdrop(&tile.texture, local(part))?;
                self.bind_target(program, tile, part, &draw, false)?;
                unsafe {
                    let gl = &self.device.gl;
                    gl.active_texture(glow::TEXTURE1);
                    gl.bind_texture(glow::TEXTURE_2D, Some(tile.texture.name()));
                }
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

    fn color_strips(
        &self,
        tile: &Tile,
        colors: &[Color],
        areas: &[Option<Rect>],
        framebuffer: glow::NativeFramebuffer,
    ) -> Result<bool> {
        use krkr_protocol::graphics::DRAW_BATCH_CAPACITY;
        let mut parts = [Rect::default(); DRAW_BATCH_CAPACITY];
        let mut count = 0;
        let mut first = None;
        for (color, area) in colors.iter().zip(areas) {
            let Some(part) = area.and_then(|r| r.intersection(tile.rectangle)) else {
                continue;
            };
            if first.is_some_and(|face| face != color.face) {
                return Ok(false);
            }
            first = Some(color.face);
            parts[count] = part;
            count += 1;
        }
        if count < 2 {
            return Ok(false);
        }
        // Check strips in raster coordinates. Logical rectangles can touch
        // the same pixel after scaling; those draws must retain their order.
        let strips = &mut parts[..count];
        let vertical = strips
            .iter()
            .all(|r| r.top == strips[0].top && r.height == strips[0].height);
        let horizontal = strips
            .iter()
            .all(|r| r.left == strips[0].left && r.width == strips[0].width);
        if !vertical && !horizontal {
            return Ok(false);
        }
        strips.sort_unstable_by_key(|r| if vertical { r.left } else { r.top });
        if strips.windows(2).any(|pair| {
            let end = if vertical {
                i64::from(pair[0].left) + i64::from(pair[0].width)
            } else {
                i64::from(pair[0].top) + i64::from(pair[0].height)
            };
            end > i64::from(if vertical { pair[1].left } else { pair[1].top })
        }) {
            return Ok(false);
        }
        let mut vertices = [[0_f32; 6]; DRAW_BATCH_CAPACITY * 6];
        let mut at = 0;
        for (color, area) in colors.iter().zip(areas) {
            let Some(part) = area.and_then(|r| r.intersection(tile.rectangle)) else {
                continue;
            };
            let [x, y, w, h] = rect(part);
            let [r, g, b, _] = rgba(color.color);
            for [px, py] in [
                [x, y],
                [x + w, y],
                [x, y + h],
                [x, y + h],
                [x + w, y],
                [x + w, y + h],
            ] {
                vertices[at] = [px, py, r, g, b, color.opacity as f32];
                at += 1;
            }
        }
        let buffer = match self.device.buffer(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(&vertices[..at]),
            &self.staging,
        ) {
            Ok(buffer) => buffer,
            Err(Error::Budget(_)) => return Ok(false),
            Err(error) => return Err(error),
        };
        let program = self.program.colors(face(first.unwrap()) as u8)?;
        program.bind_program();
        program.four("u_target", rect(tile.rectangle));
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
        if program.uses("u_lookup") {
            self.bind_texture(2, &self.lookup)?;
        }
        for part in strips {
            self.device.work_draw_region(
                &tile.texture,
                Rect {
                    left: part.left - tile.rectangle.left,
                    top: part.top - tile.rectangle.top,
                    ..*part
                },
            )?;
        }
        self.device.draw_state.invalidate_vertices();
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
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer.name));
            gl.enable_vertex_attrib_array(0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 24, 0);
            gl.vertex_attrib_pointer_f32(1, 4, glow::FLOAT, false, 24, 8);
            gl.draw_arrays(glow::TRIANGLES, 0, at as i32);
            gl.disable_vertex_attrib_array(1);
        }
        self.device.check()?;
        Ok(true)
    }
}

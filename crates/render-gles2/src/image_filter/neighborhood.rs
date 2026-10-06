use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{Draw, rect},
    image::{Plane, Tile},
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::{
    filter::{Filter, Kind},
    graphics::{DrawFace, Rect, Size},
    pixels::Bytes,
};
use std::rc::Rc;

enum Work {
    Smudge(Rc<Texture>),
    Gaussian {
        pool: [Rc<Texture>; 2],
        scales: Rc<Texture>,
        bytes: Bytes,
    },
}
fn halo(block: Size, size: Size) -> Size {
    Size {
        width: size.width.min(block.width + 2),
        height: size.height.min(block.height + 2),
    }
}
fn overhead(block: Size, size: Size, gaussian: bool) -> usize {
    if gaussian {
        block.rgba_bytes().unwrap() * 2 + block.width.max(block.height) as usize * 4
    } else {
        halo(block, size).rgba_bytes().unwrap()
    }
}

impl Gpu {
    pub(super) fn neighborhood_filter(
        &self,
        image: &mut Image,
        area: Rect,
        filter: &Filter,
    ) -> Result<()> {
        let (gaussian, passes, kind) = match filter.kind {
            Kind::Gaussian => (true, 2, 6),
            Kind::Smudge { passes } => (false, passes, 5),
            _ => unreachable!(),
        };
        if passes == 0 {
            return Ok(());
        }
        let _weights = self.staging.reserve(filter.table.as_slice().len())?;
        let weights: Vec<f32> = filter
            .table
            .as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .map(|&b| f32::from_le_bytes(b))
            .collect();
        if gaussian
            && (weights.iter().any(|v| !v.is_finite() || *v < 0.)
                || weights[weights.len() / 2] <= 0.
                || !weights.iter().sum::<f32>().is_finite())
        {
            return Err(Error::Message("invalid Gaussian kernel weights"));
        }
        let mut renderer = self.image_filters.borrow_mut();
        if renderer.programs[kind].is_none() {
            let fragment = if gaussian {
                format!(
                    "{}\n{}",
                    include_str!("../float.glsl"),
                    include_str!("../gaussian.frag")
                )
            } else {
                include_str!("../smudge.frag").to_owned()
            };
            renderer.programs[kind] = Some(Program::new(
                self.device.clone(),
                include_str!("../quad.vert"),
                &fragment,
            )?);
        }
        if gaussian {
            renderer.ensure_table(self, filter)?;
        }
        let program = renderer.programs[kind].as_ref().unwrap();
        let table = renderer.table.as_ref().map(|(_, t)| t.as_ref());
        let size = Size {
            width: area.width,
            height: area.height,
        };
        let base = size
            .rgba_bytes()
            .and_then(|v| v.checked_mul(1 + usize::from(passes > 1)))
            .ok_or(Error::Message("filter region byte size overflow"))?;
        let edge = self.tile_edge.min(256);
        let mut block = Size {
            width: size.width.min(edge),
            height: size.height.min(edge),
        };
        if base.saturating_add(overhead(block, size, gaussian)) > self.scratch.available() {
            self.collect()?;
        }
        while base.saturating_add(overhead(block, size, gaussian)) > self.scratch.available()
            && (block.width > 1 || block.height > 1)
        {
            if block.width >= block.height && block.width > 1 {
                block.width = block.width.div_ceil(2);
            } else {
                block.height = block.height.div_ceil(2);
            }
        }
        drop(
            self.scratch
                .reserve(base.saturating_add(overhead(block, size, gaussian)))?,
        );
        let mut work = if gaussian {
            let scale_size = Size {
                width: block.width.max(block.height),
                height: 1,
            };
            Work::Gaussian {
                pool: [
                    self.device.texture(block, &self.scratch)?,
                    self.device.texture(block, &self.scratch)?,
                ],
                scales: self.device.texture(scale_size, &self.scratch)?,
                bytes: Bytes::zeroed(scale_size.rgba_bytes().unwrap(), &self.staging)?,
            }
        } else {
            Work::Smudge(self.device.texture(halo(block, size), &self.scratch)?)
        };
        let mut surfaces = vec![self.create_surface_image(size)?];
        if passes > 1 {
            surfaces.push(self.create_surface_image(size)?);
        }
        // All allocations precede pixel writes. Once the clipped source is
        // captured, only the final pass writes the caller's main plane.
        self.writable(image, area, false)?;
        self.copy_rect(
            &mut surfaces[0],
            image,
            area,
            0,
            0,
            size.rect(),
            DrawFace::Alpha,
            false,
        )?;
        for pass in 0..passes {
            let source = surfaces[(pass % 2) as usize].plane(false)?;
            let (target, destination, origin) = if pass + 1 == passes {
                (image.plane(false)?, area, [area.left, area.top])
            } else {
                (
                    surfaces[(1 - pass % 2) as usize].plane(false)?,
                    size.rect(),
                    [0, 0],
                )
            };
            for tile in &target.tiles {
                let Some(region) = tile.rectangle.intersection(destination) else {
                    continue;
                };
                for y in (0..region.height).step_by(block.height as usize) {
                    for x in (0..region.width).step_by(block.width as usize) {
                        let part = Rect {
                            left: region.left + x as i32,
                            top: region.top + y as i32,
                            width: block.width.min(region.width - x),
                            height: block.height.min(region.height - y),
                        };
                        match &mut work {
                            Work::Smudge(gather) => {
                                self.smudge_part(program, source, tile, part, origin, gather)?
                            }
                            Work::Gaussian {
                                pool,
                                scales,
                                bytes,
                            } => self.gaussian_part(
                                program,
                                source,
                                tile,
                                part,
                                origin,
                                pass != 0,
                                &weights,
                                table.unwrap(),
                                pool,
                                scales,
                                bytes,
                            )?,
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn neighbor_target(
        &self,
        program: &Program,
        texture: &Texture,
        origin: [i32; 2],
        part: Rect,
        mask: [bool; 4],
    ) -> Result<()> {
        program.bind();
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(texture.framebuffer()?));
            gl.viewport(0, 0, texture.size.width as i32, texture.size.height as i32);
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
        }
        program.four(
            "u_target",
            [
                origin[0] as f32,
                origin[1] as f32,
                texture.size.width as f32,
                texture.size.height as f32,
            ],
        );
        program.four("u_rectangle", rect(part));
        program.one("u_flip", 1.);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn smudge_part(
        &self,
        program: &Program,
        source: &Plane,
        target: &Tile,
        part: Rect,
        origin: [i32; 2],
        gather: &Rc<Texture>,
    ) -> Result<()> {
        let local = Rect {
            left: part.left - origin[0],
            top: part.top - origin[1],
            ..part
        };
        let footprint = Rect {
            left: local.left - 1,
            top: local.top - 1,
            width: local.width + 2,
            height: local.height + 2,
        }
        .intersection(source.size.rect())
        .unwrap();
        let plane = Plane {
            size: source.size,
            budget: self.scratch.clone(),
            tiles: vec![Tile {
                backing: None,
                rectangle: Rect {
                    left: footprint.left,
                    top: footprint.top,
                    ..gather.size.rect()
                },
                texture: gather.clone(),
            }],
        };
        self.draw(
            &plane,
            Some(source),
            footprint,
            &Draw::copy([1., 0., 0., 0., 1., 0.], [true; 4]),
        )?;
        self.neighbor_target(
            program,
            &target.texture,
            [target.rectangle.left, target.rectangle.top],
            part,
            [true; 4],
        )?;
        self.bind_texture(0, gather)?;
        program.two(
            "u_source_origin",
            footprint.left as f32,
            footprint.top as f32,
        );
        program.two(
            "u_source_size",
            gather.size.width as f32,
            gather.size.height as f32,
        );
        program.two(
            "u_canvas",
            source.size.width as f32,
            source.size.height as f32,
        );
        program.two("u_region", origin[0] as f32, origin[1] as f32);
        unsafe {
            self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
        }
        self.device.check()
    }

    #[allow(clippy::too_many_arguments)]
    fn gaussian_part(
        &self,
        program: &Program,
        source: &Plane,
        target: &Tile,
        part: Rect,
        origin: [i32; 2],
        vertical: bool,
        weights: &[f32],
        table: &Texture,
        pool: &[Rc<Texture>; 2],
        scales: &Texture,
        bytes: &mut Bytes,
    ) -> Result<()> {
        let length = weights.len() as i32;
        let middle = length / 2;
        let extent = if vertical {
            source.size.height
        } else {
            source.size.width
        } as i32;
        let start = if vertical {
            part.top - origin[1]
        } else {
            part.left - origin[0]
        };
        let rows = if vertical { part.height } else { part.width };
        for row in 0..rows as i32 {
            let at = start + row;
            let scale = if at < middle || at >= extent - middle || length > extent {
                let first = (middle - at).max(0) as usize;
                let end = (extent - at + middle).min(length) as usize;
                weights[first..end].iter().fold(0f32, |a, &b| a + b)
            } else {
                1.
            };
            bytes.as_mut_slice()[row as usize * 4..row as usize * 4 + 4]
                .copy_from_slice(&scale.to_le_bytes());
        }
        self.device.upload(scales, bytes.as_slice())?;
        program.bind();
        self.bind_texture(2, table)?;
        self.bind_texture(3, scales)?;
        program.two(
            "u_table_size",
            table.size.width as f32,
            table.size.height as f32,
        );
        program.two("u_weights_size", scales.size.width as f32, 1.);
        program.two(
            "u_backdrop_size",
            pool[0].size.width as f32,
            pool[0].size.height as f32,
        );
        program.two("u_frame", part.left as f32, part.top as f32);
        program.two("u_region", origin[0] as f32, origin[1] as f32);
        program.two(
            "u_canvas",
            source.size.width as f32,
            source.size.height as f32,
        );
        program.one("u_axis", f32::from(vertical));
        program.one("u_kind", length as f32);
        for channel in 0..4 {
            let mut previous = 0;
            unsafe {
                let gl = &self.device.gl;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(pool[0].framebuffer()?));
                gl.enable(glow::SCISSOR_TEST);
                gl.scissor(0, 0, pool[0].size.width as i32, pool[0].size.height as i32);
                gl.color_mask(true, true, true, true);
                gl.clear_color(0., 0., 0., 0.);
                gl.clear(glow::COLOR_BUFFER_BIT);
            }
            let mut select = [0.; 4];
            select[channel] = 1.;
            program.four("u_channel", select);
            program.one("u_resample_kind", 0.);
            // Batches and source tiles both advance monotonically along the
            // kernel, preserving float addition order across texture borders.
            for base in (0..length).step_by(64) {
                let count = (length - base).min(64);
                let mut footprint = Rect {
                    left: part.left - origin[0],
                    top: part.top - origin[1],
                    ..part
                };
                if vertical {
                    footprint.top += base - middle;
                    footprint.height += count as u32 - 1;
                } else {
                    footprint.left += base - middle;
                    footprint.width += count as u32 - 1;
                }
                let Some(footprint) = footprint.intersection(source.size.rect()) else {
                    continue;
                };
                program.one("u_offset", base as f32);
                for input in &source.tiles {
                    if input.rectangle.intersection(footprint).is_none() {
                        continue;
                    }
                    let next = 1 - previous;
                    self.neighbor_target(
                        program,
                        &pool[next],
                        [part.left, part.top],
                        part,
                        [true; 4],
                    )?;
                    self.bind_texture(0, &input.texture)?;
                    program.four("u_source_visible", crate::drawing::rect(input.rectangle));
                    self.bind_texture(1, &pool[previous])?;
                    program.two(
                        "u_source_origin",
                        input.sample_rectangle().left as f32,
                        input.sample_rectangle().top as f32,
                    );
                    program.two(
                        "u_source_size",
                        input.sample_rectangle().width as f32,
                        input.sample_rectangle().height as f32,
                    );
                    unsafe {
                        self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                    }
                    self.device.check()?;
                    previous = next;
                }
            }
            let mut mask = [false; 4];
            mask[channel] = true;
            self.neighbor_target(
                program,
                &target.texture,
                [target.rectangle.left, target.rectangle.top],
                part,
                mask,
            )?;
            self.bind_texture(1, &pool[previous])?;
            program.one("u_resample_kind", 1.);
            unsafe {
                self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            self.device.check()?;
        }
        Ok(())
    }
}

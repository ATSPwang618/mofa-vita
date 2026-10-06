use crate::{Gpu, Image, Result, drawing::rect, shader::Program};
use glow::HasContext;
use krkr_protocol::{
    graphics::{Rect, Size},
    transform::Perspective,
};
use krkr_render::perspective::Mapping;

impl Gpu {
    pub fn perspective(
        &self,
        target: &mut Image,
        source: &Image,
        mapping: Perspective,
        clip: Rect,
    ) -> Result<()> {
        self.check_image(target)?;
        self.check_image(source)?;
        let mapping = Mapping::new(mapping)?;
        let Some(clip) = clip.intersection(target.size.rect()) else {
            return Ok(());
        };
        let compact = self.compact_source(source)?;
        let source = compact.as_ref().unwrap_or(source);
        let input = source.plane(false)?;
        let mut cell = self.perspective_program.borrow_mut();
        if cell.is_none() {
            *cell = Some(Program::new(
                self.device.clone(),
                include_str!("perspective.vert"),
                &format!(
                    "{}\n{}",
                    include_str!("tiles.glsl"),
                    include_str!("perspective.frag")
                ),
            )?);
        }
        let program = cell.as_ref().unwrap();
        self.writable(target, clip, false)?;
        for tile in &target.plane(false)?.tiles {
            let Some(part) = tile.rectangle.intersection(clip) else {
                continue;
            };
            program.bind();
            unsafe {
                let gl = &self.device.gl;
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(tile.texture.framebuffer()?));
                gl.viewport(
                    0,
                    0,
                    tile.rectangle.width as i32,
                    tile.rectangle.height as i32,
                );
                gl.enable(glow::SCISSOR_TEST);
                gl.scissor(
                    part.left - tile.rectangle.left,
                    part.top - tile.rectangle.top,
                    part.width as i32,
                    part.height as i32,
                );
                gl.enable(glow::BLEND);
                gl.blend_equation(glow::FUNC_ADD);
                gl.blend_func_separate(
                    glow::SRC_ALPHA,
                    glow::ONE_MINUS_SRC_ALPHA,
                    glow::ONE,
                    glow::ONE_MINUS_SRC_ALPHA,
                );
                gl.color_mask(true, true, true, true);
            }
            program.four("u_target", rect(tile.rectangle));
            for (name, data) in ["u_data0", "u_data1", "u_data2", "u_data3"]
                .into_iter()
                .zip(mapping.points)
            {
                program.four(name, data);
            }
            for (name, row) in ["u_map_x", "u_map_y", "u_map_w"]
                .into_iter()
                .zip(mapping.inverse)
            {
                program.three(name, [row[0], row[1], row[2]]);
            }
            program.four(
                "u_extent",
                [
                    source.size.width as f32,
                    source.size.height as f32,
                    input.size.width as f32 / source.size.width as f32,
                    input.size.height as f32 / source.size.height as f32,
                ],
            );
            let footprint = stored_region(
                mapping.source_region(part, source.size),
                source.size,
                input.size,
            );
            for first in &input.tiles {
                if footprint.intersection(first.rectangle).is_none() {
                    continue;
                }
                self.bind_neighbours(program, input, first)?;
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
            }
            self.device.check()?;
        }
        Ok(())
    }
}
fn stored_region(region: Rect, logical: Size, stored: Size) -> Rect {
    let axis = |start: i32, length: u32, logical: u32, stored: u32| {
        let pixel = |value: u32| {
            ((u64::from(value) * 2 + 1) * u64::from(stored) / (u64::from(logical) * 2)) as u32
        };
        let left = pixel(start as u32);
        let right = pixel(start as u32 + length - 1) + 1;
        (left as i32, right - left)
    };
    let (left, width) = axis(region.left, region.width, logical.width, stored.width);
    let (top, height) = axis(region.top, region.height, logical.height, stored.height);
    Rect {
        left,
        top,
        width,
        height,
    }
}

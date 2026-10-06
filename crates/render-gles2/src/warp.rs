use crate::{Error, Gpu, Image, Result, device::Texture, drawing::rect, shader::Program};
use glow::HasContext;
use krkr_protocol::{
    graphics::{Rect, Size},
    pixels::Bytes,
    warp::Warp,
};
use std::{
    rc::Rc,
    sync::{Arc, Weak},
};

#[derive(Default)]
pub(crate) struct Renderer {
    programs: [Option<Program>; 3],
    lens: Option<(Weak<Bytes>, Rc<Texture>)>,
}
fn word(value: i32) -> [f32; 4] {
    value.to_le_bytes().map(f32::from)
}

// After one signed wrapping square/shift the scale lies in [-131072,131071].
// Exhaustive recurrence testing bounds every tail by 37 and every cycle by a
// divisor of eight. Preserve huge powers with at most 45 GPU iterations.
fn lens_iterations(power: u32) -> u32 {
    let count = power.saturating_sub(1);
    if count > 38 {
        38 + (count - 38) % 8
    } else {
        count
    }
}
impl Gpu {
    pub fn warp_upload_bytes(&self, effect: &Warp) -> usize {
        let Warp::Lens { table, .. } = effect else {
            return 0;
        };
        if self
            .warps
            .borrow()
            .lens
            .as_ref()
            .is_some_and(|(owner, _)| owner.upgrade().is_some_and(|old| Arc::ptr_eq(&old, table)))
        {
            0
        } else {
            8192 * 4
        }
    }
    pub fn collect_warp_tables(&self) {
        let mut renderer = self.warps.borrow_mut();
        if renderer
            .lens
            .as_ref()
            .is_some_and(|(owner, _)| owner.strong_count() == 0)
        {
            renderer.lens.take();
        }
    }
    pub fn warp(&self, target: &mut Image, source: &Image, effect: &Warp) -> Result<()> {
        self.check_image(target)?;
        self.check_image(source)?;
        target.plane(false)?;
        source.plane(false)?;
        let rectangle = match effect {
            Warp::Stretch { destination, .. } => *destination,
            _ => target.size.rect(),
        };
        let Some(area) = rectangle.intersection(target.size.rect()) else {
            return Ok(());
        };
        let (kind, parameter, iterations, linear, reads_old) = match effect {
            Warp::Lens {
                radius,
                power,
                table,
            } => {
                if !radius.is_finite() || table.as_slice().len() != 8192 * 4 {
                    return Err(Error::Message("invalid lens radius or table"));
                }
                (0, *radius, lens_iterations(*power) as f32, false, false)
            }
            Warp::Vortex { radians } => {
                if !radians.is_finite() {
                    return Err(Error::Message("invalid vortex angle"));
                }
                (1, *radians, 0., false, false)
            }
            Warp::Stretch {
                source,
                destination,
                opacity,
            } => {
                if source.width > i32::MAX as u32
                    || source.height > i32::MAX as u32
                    || destination.width > i32::MAX as u32
                    || destination.height > i32::MAX as u32
                {
                    return Err(Error::Message(
                        "warp dimensions exceed signed coordinate limits",
                    ));
                }
                let sx = (source.width as i32).wrapping_shl(8) / (destination.width as i32);
                let sy = (source.height as i32).wrapping_shl(8) / (destination.height as i32);
                (
                    2,
                    f32::from(*opacity < 255),
                    f32::from(sx <= 256 && sy <= 256),
                    sx <= 256 && sy <= 256,
                    *opacity < 255,
                )
            }
        };
        let compact = self.compact_source(source)?;
        let source = compact.as_ref().unwrap_or(source);
        let plane = source.plane(false)?;
        let mut renderer = self.warps.borrow_mut();
        if renderer.programs[kind].is_none() {
            renderer.programs[kind] = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &format!(
                    "#define WARP_KIND {kind}\n{}\n{}\n{}",
                    include_str!("integer.glsl"),
                    include_str!("tiles.glsl"),
                    include_str!("warp.frag")
                ),
            )?);
        }
        if let Warp::Lens { table, .. } = effect
            && !renderer.lens.as_ref().is_some_and(|(owner, _)| {
                owner.upgrade().is_some_and(|old| Arc::ptr_eq(&old, table))
            })
        {
            renderer.lens.take();
            let texture = self.device.sample_texture(
                Size {
                    width: 256,
                    height: 32,
                },
                &self.resident,
            )?;
            self.device.upload(&texture, table.as_slice())?;
            renderer.lens = Some((Arc::downgrade(table), texture));
        }
        let previous = if reads_old {
            let stored = target.plane(false)?;
            let extent = if stored.size != target.size {
                Size {
                    width: self.tile_edge.min(target.size.width),
                    height: self.tile_edge.min(target.size.height),
                }
            } else {
                Size {
                    width: stored
                        .tiles
                        .iter()
                        .map(|t| t.rectangle.width)
                        .max()
                        .unwrap(),
                    height: stored
                        .tiles
                        .iter()
                        .map(|t| t.rectangle.height)
                        .max()
                        .unwrap(),
                }
            };
            Some(self.device.texture(
                Size {
                    width: extent.width.min(area.width),
                    height: extent.height.min(area.height),
                },
                &self.scratch,
            )?)
        } else {
            None
        };
        self.writable(target, area, false)?;
        let program = renderer.programs[kind].as_ref().unwrap();
        for tile in &target.plane(false)?.tiles {
            let Some(part) = tile.rectangle.intersection(area) else {
                continue;
            };
            if let Some(previous) = &previous {
                self.device.copy_region_at(
                    &tile.texture,
                    Rect {
                        left: part.left - tile.rectangle.left,
                        top: part.top - tile.rectangle.top,
                        ..part
                    },
                    previous,
                    0,
                    0,
                )?;
            }
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
                gl.disable(glow::BLEND);
                gl.disable(glow::SCISSOR_TEST);
                gl.color_mask(true, true, true, true);
            }
            program.four("u_target", rect(tile.rectangle));
            program.four("u_rectangle", rect(part));
            program.one("u_flip", 1.);
            program.four(
                "u_extent",
                [
                    source.size.width as f32,
                    source.size.height as f32,
                    plane.size.width as f32 / source.size.width as f32,
                    plane.size.height as f32 / source.size.height as f32,
                ],
            );
            match effect {
                Warp::Lens { .. } => {
                    program.two(
                        "u_canvas",
                        (target.size.width / 2) as f32,
                        (target.size.height / 2) as f32,
                    );
                    program.two("u_table_size", 256., 32.);
                    self.bind_texture(4, &renderer.lens.as_ref().unwrap().1)?;
                }
                Warp::Vortex { .. } => {
                    let width = source.size.width as i32;
                    let height = source.size.height as i32;
                    let shorter = width.min(height);
                    let radius = (width / 2).max(height / 2);
                    program.two("u_canvas", (width / 2) as f32, (height / 2) as f32);
                    program.four("u_color", word(radius.wrapping_mul(radius)));
                    program.four(
                        "u_data0",
                        [
                            width.wrapping_mul(width) as f32,
                            shorter.wrapping_mul(shorter) as f32,
                            0.,
                            0.,
                        ],
                    );
                }
                Warp::Stretch {
                    source,
                    destination,
                    opacity,
                } => {
                    let step = [
                        (source.width as i32).wrapping_shl(8) / (destination.width as i32),
                        (source.height as i32).wrapping_shl(8) / (destination.height as i32),
                    ];
                    let origin = [source.left.wrapping_shl(8), source.top.wrapping_shl(8)];
                    let delta = [
                        tile.rectangle.left.wrapping_sub(destination.left),
                        tile.rectangle.top.wrapping_sub(destination.top),
                    ];
                    program.four(
                        "u_data0",
                        word(origin[0].wrapping_add(delta[0].wrapping_mul(step[0]))),
                    );
                    program.four(
                        "u_data1",
                        word(origin[1].wrapping_add(delta[1].wrapping_mul(step[1]))),
                    );
                    program.four("u_data2", word(step[0]));
                    program.four("u_data3", word(step[1]));
                    program.four("u_color", word(if *opacity < 255 { *opacity } else { 256 }));
                    let previous = previous.as_deref().unwrap_or(&self.lookup);
                    self.bind_texture(5, previous)?;
                    program.two(
                        "u_previous_size",
                        previous.size.width as f32,
                        previous.size.height as f32,
                    );
                    program.two("u_backdrop_origin", part.left as f32, part.top as f32);
                }
            }
            for (index, input) in plane.tiles.iter().enumerate() {
                program.four(
                    "u_frame",
                    [
                        kind as f32,
                        parameter,
                        if kind == 2 {
                            f32::from(linear)
                        } else {
                            iterations
                        },
                        index as f32,
                    ],
                );
                self.bind_neighbours(program, plane, input)?;
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
            }
            self.device.check()?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/warp/internal.rs"]
mod tests;

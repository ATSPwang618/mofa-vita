//! Ordered constant-color rectangles share a vertex stream. In particular,
//! script-generated icons/gradients must not issue one glClear per pixel.
use crate::{Gpu, Result, image::Plane, scene::raster::Raster, shader::Program};
use glow::HasContext;
use krkr_protocol::graphics::{DrawFace, Fill, Size};

const RECTANGLES: usize = 128;
const STRIDE: usize = 12; // Two float coordinates, then normalized RGBA bytes.

pub(crate) fn mask(fill: &Fill) -> [bool; 4] {
    match fill.face {
        DrawFace::Mask => [false, false, false, true],
        DrawFace::Opaque if fill.hold_alpha => [true, true, true, false],
        _ => [true; 4],
    }
}

pub(crate) fn unchanged(
    tile: &crate::image::Tile,
    logical: Size,
    raster: Raster,
    fill: &Fill,
) -> bool {
    let Some(part) = fill
        .rectangle
        .intersection(logical.rect())
        .and_then(|r| raster.rect(r))
        .and_then(|r| r.intersection(tile.rectangle))
    else {
        return true;
    };
    let (mask, color) = match fill.face {
        DrawFace::Province => (0x00ff0000, (fill.color & 255) << 16),
        DrawFace::Mask => (0xff000000, (fill.color & 255) << 24),
        DrawFace::Opaque if fill.hold_alpha => (0x00ffffff, fill.color),
        _ => (u32::MAX, fill.color),
    };
    tile.solid_region(krkr_protocol::graphics::Rect {
        left: part.left - tile.rectangle.left,
        top: part.top - tile.rectangle.top,
        ..part
    })
    .is_some_and(|old| old & mask == color & mask)
}

impl Gpu {
    /// On a constant canvas, a masked clear is another constant color. Keep
    /// it virtual instead of expanding it just to preserve known channels.
    pub(crate) fn constant_channel_fill(&self, image: &crate::Image, fill: &Fill) -> Option<Fill> {
        if !image.canvas {
            return None;
        }
        let (channels, value) = match fill.face {
            DrawFace::Mask => (0xff000000, (fill.color & 255) << 24),
            DrawFace::Opaque if fill.hold_alpha => (0x00ffffff, fill.color),
            _ => return None,
        };
        let plane = image.main.as_ref()?;
        let color = plane.tiles.first()?.texture.solid_color()?;
        if !plane
            .tiles
            .iter()
            .all(|t| t.texture.solid_color() == Some(color))
        {
            return None;
        }
        Some(Fill {
            color: (color & !channels) | (value & channels),
            face: DrawFace::Alpha,
            hold_alpha: false,
            ..*fill
        })
    }

    pub(crate) fn fill_rectangles(
        &self,
        plane: &Plane,
        logical: Size,
        raster: Raster,
        fills: &[Fill],
        mask: [bool; 4],
    ) -> Result<()> {
        let mut slot = self.fill_program.borrow_mut();
        if slot.is_none() {
            *slot = Some(Program::new(
                self.device.clone(),
                include_str!("fills.vert"),
                include_str!("fills.frag"),
            )?);
        }
        let program = slot.as_ref().unwrap();
        let gl = &self.device.gl;
        let mut vertices = [0u8; RECTANGLES * 6 * STRIDE];
        for chunk in fills.chunks(RECTANGLES) {
            let mut bytes = 0;
            let mut bounds = None;
            let mut first = None;
            for fill in chunk {
                let Some(area) = fill
                    .rectangle
                    .intersection(logical.rect())
                    .and_then(|r| raster.rect(r))
                else {
                    continue;
                };
                first.get_or_insert(area);
                bounds = Some(crate::scene_damage::union(bounds, area));
                let color = if fill.face == DrawFace::Mask {
                    [0, 0, 0, fill.color as u8]
                } else {
                    [
                        (fill.color >> 16) as u8,
                        (fill.color >> 8) as u8,
                        fill.color as u8,
                        (fill.color >> 24) as u8,
                    ]
                };
                let left = area.left as f32;
                let top = area.top as f32;
                let right = left + area.width as f32;
                let bottom = top + area.height as f32;
                // Primitive order is script order, including overlapping fills.
                // Both triangles carry identical byte colors at every vertex.
                for [x, y] in [
                    [left, top],
                    [right, top],
                    [left, bottom],
                    [left, bottom],
                    [right, top],
                    [right, bottom],
                ] {
                    vertices[bytes..bytes + 4].copy_from_slice(&x.to_ne_bytes());
                    vertices[bytes + 4..bytes + 8].copy_from_slice(&y.to_ne_bytes());
                    vertices[bytes + 8..bytes + STRIDE].copy_from_slice(&color);
                    bytes += STRIDE;
                }
            }
            let Some(bounds) = bounds else {
                continue;
            };
            // Immutable until GPU completion, using the same accounted lifetime
            // as mesh buffers. Never overwrite storage still used by a draw.
            let buffer =
                self.device
                    .buffer(glow::ARRAY_BUFFER, &vertices[..bytes], &self.staging)?;
            for tile in &plane.tiles {
                if chunk
                    .iter()
                    .all(|fill| unchanged(tile, logical, raster, fill))
                {
                    continue;
                }
                let Some(part) = bounds.intersection(tile.rectangle) else {
                    continue;
                };
                let local = krkr_protocol::graphics::Rect {
                    left: part.left - tile.rectangle.left,
                    top: part.top - tile.rectangle.top,
                    ..part
                };
                let framebuffer = if mask == [true; 4]
                    && first.is_some_and(|area| area.intersection(part) == Some(part))
                {
                    tile.texture.overwrite_framebuffer(local)?
                } else {
                    // Load gaps and unmodified channels once. The batch can then
                    // store one region without preserving per-rectangle state.
                    tile.texture.framebuffer_region(local)?
                };
                self.device.draw_state.invalidate_program();
                self.device.draw_state.invalidate_vertices();
                unsafe {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
                    gl.viewport(
                        0,
                        0,
                        tile.rectangle.width as i32,
                        tile.rectangle.height as i32,
                    );
                    gl.use_program(Some(program.name));
                    gl.bind_buffer(glow::ARRAY_BUFFER, Some(buffer.name));
                    gl.enable_vertex_attrib_array(0);
                    gl.enable_vertex_attrib_array(1);
                    gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, STRIDE as i32, 0);
                    gl.vertex_attrib_pointer_f32(1, 4, glow::UNSIGNED_BYTE, true, STRIDE as i32, 8);
                    gl.disable(glow::SCISSOR_TEST);
                    gl.disable(glow::BLEND);
                    gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
                    gl.active_texture(glow::TEXTURE0);
                }
                program.four("u_target", crate::drawing::rect(tile.rectangle));
                unsafe {
                    gl.draw_arrays(glow::TRIANGLES, 0, (bytes / STRIDE) as i32);
                }
                self.device.check()?;
            }
        }
        unsafe {
            gl.disable_vertex_attrib_array(1);
        }
        Ok(())
    }
}

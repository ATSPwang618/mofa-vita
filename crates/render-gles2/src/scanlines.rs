//! Legacy row mappings and half-pixel filters. Only row metadata is prepared on
//! CPU; the shader interpolates source bytes and preserves Duff-loop holes.
use crate::{Error, Gpu, Image, Result, drawing::rect, image::Tile, shader::Program};
use glow::HasContext;
use krkr_protocol::{
    graphics::{Rect, Size},
    pixels::Bytes,
    scanlines::Scanlines,
};

fn pixel(logical: i64, stored: u32, original: u32) -> i32 {
    ((2 * logical + 1) * i64::from(stored) / (2 * i64::from(original))) as i32
}
const BACKDROP_BYTES: usize = 512 * 1024;

impl Gpu {
    /// Include the row table and the bounded old-pixel snapshot in command
    /// admission, before any destination pixels have been modified.
    pub fn scanline_upload_bytes(&self, target: &Image, rows: &Scanlines) -> usize {
        let Some(area) = rows.rectangle.intersection(target.size.rect()) else {
            return 0;
        };
        let words = &rows.words;
        if words.len() < 8 || words[1] <= 0 {
            return 0;
        }
        let top = i64::from(area.top).max(i64::from(words[0]));
        let bottom = (i64::from(area.top) + i64::from(area.height))
            .min(i64::from(words[0]) + i64::from(words[1]));
        if bottom <= top {
            return 0;
        }
        let count = (bottom - top) as usize * 8;
        let maximum = self.device.max_texture as usize;
        let width = 256.max(count.div_ceil(maximum)).min(maximum).min(count);
        let table = width
            .saturating_mul(count.div_ceil(width))
            .saturating_mul(4);
        let backdrop = if words[2] >= 2 {
            let width = area.width.min(self.device.max_texture) as usize;
            let height = (bottom - top) as usize;
            width * height.min((BACKDROP_BYTES / (width * 4)).max(1)) * 4
        } else {
            0
        };
        table.saturating_add(backdrop)
    }

    pub fn copy_scanlines(
        &self,
        target: &mut Image,
        source: &Image,
        rows: &Scanlines,
    ) -> Result<()> {
        self.check_image(target)?;
        self.check_image(source)?;
        let Some(mut area) = rows.rectangle.intersection(target.size.rect()) else {
            return Ok(());
        };
        let words = &rows.words;
        if words.len() < 8
            || !words.len().is_multiple_of(8)
            || words[1] < 0
            || words[1] as usize != words.len() / 8 - 1
            || !(0..=3).contains(&words[2])
            || words[3] != source.size.width as i32
            || words[4] != source.size.height as i32
        {
            return Err(Error::Message("invalid scanline copy data"));
        }
        let plane = source.plane(false)?;
        let top = i64::from(area.top).max(i64::from(words[0]));
        let bottom = (i64::from(area.top) + i64::from(area.height))
            .min(i64::from(words[0]) + i64::from(words[1]));
        if bottom <= top {
            return Ok(());
        }
        area.top = top as i32;
        area.height = (bottom - top) as u32;
        let _records = self.staging.reserve(area.height as usize * 32)?;
        let mut records = vec![[0i32; 8]; area.height as usize];
        let mode = words[2];
        let source_width = i64::from(source.size.width);
        let source_len = source_width * i64::from(source.size.height);
        let right = i64::from(area.left) + i64::from(area.width);
        // Normalize large signed offsets before conversion to GLSL floats.
        // Each record now refers only to valid, visible source/destination data.
        for (row_index, record) in records.iter_mut().enumerate() {
            let index = 8 + ((top - i64::from(words[0])) as usize + row_index) * 8;
            let row = &words[index..index + 8];
            if row[1] <= 0 {
                continue;
            }
            if mode == 1 && !(0..=255).contains(&row[4]) {
                return Err(Error::Message("invalid scanline interpolation fraction"));
            }
            let origin = i64::from(row[0]);
            let end = origin + i64::from(row[1]);
            let source_origin = i64::from(row[3]) * source_width + i64::from(row[2]);
            let left = i64::from(area.left).max(origin).max(origin - source_origin);
            let end = right.min(end).min(origin + source_len - source_origin);
            if end <= left {
                continue;
            }
            let at = source_origin + left - origin;
            let first = (origin - left).max(-2);
            let odd = row[4] != 0;
            let last = (origin + i64::from(row[1]) - if odd { 4 } else { 1 } - left)
                .clamp(-2, end - left + 1);
            *record = [
                left as i32,
                (end - left) as i32,
                (at % source_width) as i32,
                (at / source_width) as i32,
                if mode >= 2 { i32::from(odd) } else { row[4] },
                first as i32,
                last as i32,
                i32::from(row[1] < 6),
            ];
        }
        if !records.iter().any(|row| row[1] != 0) {
            return Ok(());
        }
        // Coordinates and extents fit in two bytes for ordinary game images.
        // Keep the signed end threshold at full width, including its sentinels.
        let packed = records.iter().all(|row| {
            row[..4].iter().all(|&v| (0..=65535).contains(&v))
                && (mode == 0 || (0..=255).contains(&row[4]))
                && (-2..=0).contains(&row[5])
        });
        let stride = if packed { 4 } else { 8 };
        let count = records.len() * stride;
        let maximum = self.device.max_texture as usize;
        if count > maximum * maximum {
            return Err(Error::Message("scanline table exceeds texture limits"));
        }
        let width = 256.max(count.div_ceil(maximum)).min(maximum).min(count);
        let size = Size {
            width: width as u32,
            height: count.div_ceil(width) as u32,
        };
        let mut bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &self.staging)?;
        if packed {
            for (out, row) in bytes
                .as_mut_slice()
                .as_chunks_mut::<16>()
                .0
                .iter_mut()
                .zip(&records)
            {
                for (out, &value) in out[..8].as_chunks_mut::<2>().0.iter_mut().zip(&row[..4]) {
                    out.copy_from_slice(&(value as u16).to_le_bytes());
                }
                out[8] = if mode == 0 { 0 } else { row[4] as u8 };
                out[9] = (row[5] + 2) as u8;
                out[10] = row[7] as u8;
                out[12..16].copy_from_slice(&row[6].to_le_bytes());
            }
        } else {
            for (out, value) in bytes
                .as_mut_slice()
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(records.iter().flatten())
            {
                out.copy_from_slice(&value.to_le_bytes());
            }
        }
        let table = self.device.sample_texture(size, &self.scratch)?;
        self.device.upload(&table, bytes.as_slice())?;
        drop(bytes);
        let mut cell = self.scanline_program.borrow_mut();
        if cell.is_none() {
            *cell = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                include_str!("scanlines.frag"),
            )?);
        }
        let program = cell.as_ref().unwrap();
        let _candidates = self
            .staging
            .reserve(plane.tiles.len() * std::mem::size_of::<&Tile>())?;
        self.writable(target, area, false)?;
        let previous = if mode >= 2 {
            let width = area.width.min(self.device.max_texture);
            let height = area
                .height
                .min((BACKDROP_BYTES / (width as usize * 4)).max(1) as u32);
            Some(
                self.device
                    .sample_texture(Size { width, height }, &self.scratch)?,
            )
        } else {
            None
        };
        for tile in &target.plane(false)?.tiles {
            let Some(part) = tile.rectangle.intersection(area) else {
                continue;
            };
            let height = previous.as_ref().map_or(part.height, |t| t.size.height);
            for top in (0..part.height).step_by(height as usize) {
                let part = Rect {
                    top: part.top + top as i32,
                    height: (part.height - top).min(height),
                    ..part
                };
                let inputs = candidates(
                    &plane.tiles,
                    &records,
                    area.top,
                    part,
                    source.size,
                    plane.size,
                    mode,
                );
                if inputs.is_empty() {
                    continue;
                }
                if let Some(texture) = &previous {
                    // Reuse one strip in command order. The trailing strip may
                    // be shorter; only initialized pixels are sampled below.
                    self.device.copy_region_at(
                        &tile.texture,
                        Rect {
                            left: part.left - tile.rectangle.left,
                            top: part.top - tile.rectangle.top,
                            ..part
                        },
                        texture,
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
                    gl.disable(glow::SCISSOR_TEST);
                    gl.disable(glow::BLEND);
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
                program.two("u_table_size", size.width as f32, size.height as f32);
                program.two("u_backdrop_origin", part.left as f32, part.top as f32);
                let backdrop = previous.as_ref().map_or(part, |t| t.size.rect());
                program.two(
                    "u_backdrop_size",
                    backdrop.width as f32,
                    backdrop.height as f32,
                );
                self.bind_texture(2, &table)?;
                self.bind_texture(1, previous.as_deref().unwrap_or(&self.lookup))?;
                for &a in &inputs {
                    self.bind_texture(0, &a.texture)?;
                    program.two(
                        "u_source_origin",
                        a.rectangle.left as f32,
                        a.rectangle.top as f32,
                    );
                    program.two(
                        "u_source_size",
                        a.rectangle.width as f32,
                        a.rectangle.height as f32,
                    );
                    for (index, &b) in inputs.iter().enumerate() {
                        // Mode zero never samples a neighbour. Other modes select
                        // exactly one pair per pixel, including copy-only edges.
                        if mode == 0 && index != 0 {
                            break;
                        }
                        self.bind_texture(3, &b.texture)?;
                        program.four("u_region", rect(b.rectangle));
                        program.four(
                            "u_frame",
                            [
                                mode as f32,
                                area.top as f32,
                                if packed { 1. } else { 0. },
                                index as f32,
                            ],
                        );
                        unsafe {
                            self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                        }
                    }
                }
                self.device.check()?;
            }
        }
        Ok(())
    }
}

/// Bound source tile pairs by the actual sampled scanline ranges. A compact
/// logical image is sampled without a full-size materialization or CPU readback.
fn candidates<'a>(
    tiles: &'a [Tile],
    rows: &[[i32; 8]],
    top: i32,
    part: Rect,
    logical: Size,
    stored: Size,
    mode: i32,
) -> Vec<&'a Tile> {
    let mut bounds: Option<Rect> = None;
    let width = i64::from(logical.width);
    let length = width * i64::from(logical.height);
    for y in part.top..part.top + part.height as i32 {
        let row = rows[(y - top) as usize];
        let left = part.left.max(row[0]);
        let right = (i64::from(part.left) + i64::from(part.width))
            .min(i64::from(row[0]) + i64::from(row[1]));
        if right <= i64::from(left) {
            continue;
        }
        let start =
            i64::from(row[3]) * width + i64::from(row[2]) + i64::from(left) - i64::from(row[0]);
        let end = start + right - i64::from(left) - 1;
        let start = (start - i64::from(mode >= 2)).max(0);
        let end = (end + i64::from(mode == 1)).min(length - 1);
        let (sy, ey) = (start / width, end / width);
        let (left, right) = if sy == ey {
            (
                pixel(start % width, stored.width, logical.width),
                pixel(end % width, stored.width, logical.width) + 1,
            )
        } else {
            (0, stored.width as i32)
        };
        let (top, bottom) = (
            pixel(sy, stored.height, logical.height),
            pixel(ey, stored.height, logical.height) + 1,
        );
        bounds = Some(match bounds {
            None => Rect {
                left,
                top,
                width: (right - left) as u32,
                height: (bottom - top) as u32,
            },
            Some(old) => {
                let x = old.left.min(left);
                let y = old.top.min(top);
                Rect {
                    left: x,
                    top: y,
                    width: ((old.left + old.width as i32).max(right) - x) as u32,
                    height: ((old.top + old.height as i32).max(bottom) - y) as u32,
                }
            }
        });
    }
    let mut result = Vec::with_capacity(tiles.len());
    result.extend(
        tiles.iter().filter(|tile| {
            bounds.is_some_and(|bounds| bounds.intersection(tile.rectangle).is_some())
        }),
    );
    result
}

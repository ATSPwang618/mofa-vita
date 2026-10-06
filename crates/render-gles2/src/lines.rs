use crate::{Error, Gpu, Image, Result, drawing::rect, image::Tile, shader::Program};
use glow::HasContext;
use krkr_protocol::{
    graphics::{Rect, Size},
    lines::{Lines, TILE_SIZE},
    pixels::Bytes,
};

const BATCH: usize = 16;
const WORDS: usize = 10;
const BYTES: usize = BATCH * WORDS * 4;
const PASSES: usize = 64;

struct Pass<'a> {
    tile: &'a Tile,
    draw: Rect,
    lines: &'a [u32],
}

struct Index<'a> {
    ranges: &'a [u32],
    records: &'a [u32],
    words: &'a [u32],
    columns: u32,
}
impl<'a> Index<'a> {
    fn new(lines: &'a Lines) -> Result<Self> {
        let words = &lines.words;
        let invalid = || Error::Message("invalid ordered line index");
        let columns = lines.rectangle.width.div_ceil(TILE_SIZE);
        let rows = lines.rectangle.height.div_ceil(TILE_SIZE);
        let tiles = (columns as usize)
            .checked_mul(rows as usize)
            .ok_or_else(invalid)?;
        let records = tiles
            .checked_mul(2)
            .and_then(|v| v.checked_add(8))
            .ok_or_else(invalid)?;
        if words.len() < records
            || words[..4]
                != [
                    lines.rectangle.left as u32,
                    lines.rectangle.top as u32,
                    columns,
                    rows,
                ]
            || words[4] as usize != records
            || (words[5] as usize) < records
            || words[5] as usize > words.len()
            || !(words[5] as usize - records).is_multiple_of(8)
        {
            return Err(invalid());
        }
        let result = Self {
            ranges: &words[8..records],
            records: &words[records..words[5] as usize],
            words,
            columns,
        };
        let count = result.records.len() / 8;
        for range in result.ranges.as_chunks::<2>().0 {
            let start = range[0] as usize;
            let end = start.checked_add(range[1] as usize).ok_or_else(invalid)?;
            if start < words[5] as usize
                || end > words.len()
                || words[start..end].iter().any(|&i| i as usize >= count)
            {
                return Err(invalid());
            }
        }
        // Providers already clip endpoints. Validate before submitting any
        // writes; index arithmetic and fixed-point parameter preparation then
        // cannot overflow signed subtraction or divide by zero.
        for line in result.records.as_chunks::<8>().0 {
            for p in [&line[..2], &line[2..4]] {
                if (p[0] as i32) < lines.rectangle.left
                    || (p[1] as i32) < lines.rectangle.top
                    || i64::from(p[0] as i32)
                        >= i64::from(lines.rectangle.left) + i64::from(lines.rectangle.width)
                    || i64::from(p[1] as i32)
                        >= i64::from(lines.rectangle.top) + i64::from(lines.rectangle.height)
                {
                    return Err(invalid());
                }
            }
            if (i64::from(line[2] as i32) - i64::from(line[0] as i32)).unsigned_abs()
                > i32::MAX as u64
                || (i64::from(line[3] as i32) - i64::from(line[1] as i32)).unsigned_abs()
                    > i32::MAX as u64
            {
                return Err(invalid());
            }
        }
        Ok(result)
    }
    fn references(&self, x: u32, y: u32) -> &[u32] {
        let offset = (y as usize * self.columns as usize + x as usize) * 2;
        let start = self.ranges[offset] as usize;
        &self.words[start..start + self.ranges[offset + 1] as usize]
    }
    fn record(&self, index: u32) -> [u32; WORDS] {
        let line = &self.records[index as usize * 8..][..8];
        let delta = [
            (line[2] as i32) - (line[0] as i32),
            (line[3] as i32) - (line[1] as i32),
        ];
        let horizontal = delta[0].unsigned_abs() >= delta[1].unsigned_abs();
        let major = usize::from(!horizontal);
        let minor = 1 - major;
        let distance = delta[major].unsigned_abs().max(u32::from(line[5] != 0));
        let step = delta[minor].wrapping_mul(65536) / distance.max(1) as i32;
        [
            line[major],
            line[minor],
            distance,
            delta[minor] as u32,
            line[4],
            u32::from(line[5] != 0) + 2 * u32::from(horizontal) + 4 * u32::from(delta[major] >= 0),
            line[6].min(distance),
            (line[4] & 0xff000000).checked_div(line[6]).unwrap_or(0),
            step as u32,
            (distance as i32).wrapping_mul(2).max(1) as u32,
        ]
    }
}

impl Gpu {
    pub(crate) fn draw_lines(
        &self,
        image: &mut Image,
        rectangle: Rect,
        lines: &Lines,
    ) -> Result<()> {
        let Some(area) = rectangle
            .intersection(lines.rectangle)
            .and_then(|r| r.intersection(image.size.rect()))
        else {
            return Ok(());
        };
        let index = Index::new(lines)?;
        if index.records.is_empty() {
            return Ok(());
        }
        let mut renderer = self.line_program.borrow_mut();
        if renderer.is_none() {
            *renderer = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &format!(
                    "{}\n{}",
                    include_str!("integer.glsl"),
                    include_str!("lines.frag")
                ),
            )?);
        }
        let program = renderer.as_ref().unwrap();
        let previous = self.device.texture(
            Size {
                width: self.tile_edge.min(TILE_SIZE).min(area.width),
                height: self.tile_edge.min(TILE_SIZE).min(area.height),
            },
            &self.scratch,
        )?;
        let capacity = PASSES
            .min(self.scratch.available() / BYTES)
            .min(self.staging.available() / BYTES)
            .min(self.device.max_texture as usize / BATCH)
            .max(1);
        let mut bytes = Bytes::zeroed(BYTES * capacity, &self.staging)?;
        let table = self.device.sample_texture(
            Size {
                width: WORDS as u32,
                height: (BATCH * capacity) as u32,
            },
            &self.scratch,
        )?;
        self.writable(image, area, false)?;
        let mut passes = Vec::with_capacity(capacity);
        let mut flush = |passes: &mut Vec<Pass<'_>>| -> Result<()> {
            if passes.is_empty() {
                return Ok(());
            }
            for (slot, pass) in passes.iter().enumerate() {
                for (row, &line) in pass.lines.iter().enumerate() {
                    for (column, word) in index.record(line).into_iter().enumerate() {
                        let at = slot * BYTES + (row * WORDS + column) * 4;
                        bytes.as_mut_slice()[at..at + 4].copy_from_slice(&word.to_le_bytes());
                    }
                }
            }
            self.device.upload(&table, bytes.as_slice())?;
            for (slot, pass) in passes.iter().enumerate() {
                let (tile, draw) = (pass.tile, pass.draw);
                self.device.copy_region_at(
                    &tile.texture,
                    Rect {
                        left: draw.left - tile.rectangle.left,
                        top: draw.top - tile.rectangle.top,
                        ..draw
                    },
                    &previous,
                    0,
                    0,
                )?;
                program.bind();
                unsafe {
                    let gl = &self.device.gl;
                    // The next batch snapshots old pixels. Mark only
                    // this block so that snapshot won't store the whole tile.
                    gl.bind_framebuffer(
                        glow::FRAMEBUFFER,
                        Some(tile.texture.overwrite_framebuffer(Rect {
                            left: draw.left - tile.rectangle.left,
                            top: draw.top - tile.rectangle.top,
                            ..draw
                        })?),
                    );
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
                program.four("u_rectangle", rect(draw));
                program.one("u_flip", 1.);
                program.one("u_kind", pass.lines.len() as f32);
                program.two(
                    "u_lookup_window",
                    (slot * BATCH) as f32,
                    table.size.height as f32,
                );
                program.two("u_source_origin", draw.left as f32, draw.top as f32);
                program.two(
                    "u_source_size",
                    previous.size.width as f32,
                    previous.size.height as f32,
                );
                self.bind_texture(0, &previous)?;
                self.bind_texture(2, &table)?;
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
                self.device.check()?;
            }
            passes.clear();
            Ok(())
        };
        // The producer's 32-pixel index keeps unrelated lines out of a pass.
        // ES2 has no storage buffers or unbounded fragment loops: stream fixed
        // batches in their original order. Upload multiple passes together so
        // every tiny draw does not rewrite an in-flight lookup texture.
        for tile in &image.plane(false)?.tiles {
            let Some(part) = area.intersection(tile.rectangle) else {
                continue;
            };
            let first_x =
                ((i64::from(part.left) - i64::from(lines.rectangle.left)) as u32) / TILE_SIZE;
            let first_y =
                ((i64::from(part.top) - i64::from(lines.rectangle.top)) as u32) / TILE_SIZE;
            let last_x = ((i64::from(part.left) + i64::from(part.width)
                - 1
                - i64::from(lines.rectangle.left)) as u32)
                / TILE_SIZE;
            let last_y = ((i64::from(part.top) + i64::from(part.height)
                - 1
                - i64::from(lines.rectangle.top)) as u32)
                / TILE_SIZE;
            for y in first_y..=last_y {
                for x in first_x..=last_x {
                    let indexed = Rect {
                        left: (i64::from(lines.rectangle.left)
                            + i64::from(x) * i64::from(TILE_SIZE))
                            as i32,
                        top: (i64::from(lines.rectangle.top) + i64::from(y) * i64::from(TILE_SIZE))
                            as i32,
                        width: TILE_SIZE,
                        height: TILE_SIZE,
                    };
                    let draw = indexed.intersection(part).unwrap();
                    for batch in index.references(x, y).chunks(BATCH) {
                        passes.push(Pass {
                            tile,
                            draw,
                            lines: batch,
                        });
                        if passes.len() == capacity {
                            flush(&mut passes)?;
                        }
                    }
                }
            }
        }
        flush(&mut passes)?;
        Ok(())
    }
}

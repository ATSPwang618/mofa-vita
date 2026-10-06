mod neighborhood;
mod random;
use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{rect, rgba},
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::{
    filter::{Filter, Kind},
    graphics::{Rect, Size},
    pixels::Bytes,
};
use std::{
    rc::Rc,
    sync::{Arc, Weak},
};

#[derive(Default)]
pub(crate) struct Renderer {
    programs: [Option<Program>; 7],
    table: Option<(Weak<Bytes>, Rc<Texture>)>,
}
fn word(value: u32) -> [f32; 4] {
    value.to_le_bytes().map(f32::from)
}
fn table_size(filter: &Filter) -> Size {
    let words = (filter.table.as_slice().len() / 4) as u32;
    let width = words.clamp(1, 256);
    Size {
        width,
        height: words.div_ceil(width).max(1),
    }
}
fn same_table(owner: &Weak<Bytes>, table: &Arc<Bytes>) -> bool {
    owner
        .upgrade()
        .is_some_and(|old| Arc::ptr_eq(&old, table) || old.as_slice() == table.as_slice())
}
impl Renderer {
    fn ensure_table(&mut self, gpu: &Gpu, filter: &Filter) -> Result<()> {
        if self
            .table
            .as_ref()
            .is_some_and(|(owner, _)| same_table(owner, &filter.table))
        {
            self.table.as_mut().unwrap().0 = Arc::downgrade(&filter.table);
            return Ok(());
        }
        self.table.take();
        let size = table_size(filter);
        let texture = gpu.device.sample_texture(size, &gpu.resident)?;
        let source = filter.table.as_slice();
        if size.rgba_bytes().unwrap() == source.len() {
            gpu.device.upload(&texture, source)?;
        } else {
            let mut padded = Bytes::zeroed(size.rgba_bytes().unwrap(), &gpu.staging)?;
            padded.as_mut_slice()[..source.len()].copy_from_slice(source);
            gpu.device.upload(&texture, padded.as_slice())?;
        }
        self.table = Some((Arc::downgrade(&filter.table), texture));
        Ok(())
    }
}
impl Gpu {
    pub(crate) fn filter_upload_bytes(&self, filter: &Filter) -> usize {
        if !matches!(
            filter.kind,
            Kind::Lookup | Kind::Colorize { .. } | Kind::Gaussian
        ) {
            return 0;
        }
        if self
            .image_filters
            .borrow()
            .table
            .as_ref()
            .is_some_and(|(owner, _)| same_table(owner, &filter.table))
        {
            0
        } else {
            table_size(filter).rgba_bytes().unwrap_or(usize::MAX)
        }
    }
    pub(crate) fn collect_filter_tables(&self) {
        let mut renderer = self.image_filters.borrow_mut();
        if renderer
            .table
            .as_ref()
            .is_some_and(|(owner, _)| owner.strong_count() == 0)
        {
            renderer.table.take();
        }
    }
    pub(crate) fn image_filter(
        &self,
        image: &mut Image,
        area: Rect,
        filter: &Filter,
    ) -> Result<()> {
        let bytes = filter.table.as_slice();
        let length = bytes.len() / 4;
        let valid = bytes.len().is_multiple_of(4)
            && length <= 1024
            && match filter.kind {
                Kind::Lookup | Kind::Colorize { .. } => length == 256,
                Kind::Gaussian => length != 0 && !length.is_multiple_of(2),
                _ => length == 0,
            };
        if !valid {
            return Err(Error::Message("invalid image filter table"));
        }
        if matches!(filter.kind, Kind::Gaussian | Kind::Smudge { .. }) {
            return self.neighborhood_filter(image, area, filter);
        }
        let kind = match filter.kind {
            Kind::Lookup | Kind::Colorize { .. } => 0,
            Kind::Modulate {
                hue,
                saturation,
                luminance,
            } => {
                if [hue, saturation, luminance].iter().any(|v| !v.is_finite()) {
                    return Err(Error::Message("non-finite HSL filter parameter"));
                }
                1
            }
            Kind::Noise { .. } => 2,
            Kind::Xor { .. } | Kind::Dither { .. } => 3,
            Kind::RandomFill { .. } => 4,
            Kind::Gaussian | Kind::Smudge { .. } => unreachable!(),
        };
        let mut renderer = self.image_filters.borrow_mut();
        if renderer.programs[kind].is_none() {
            renderer.programs[kind] = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &format!(
                    "#define FILTER_KIND {kind}\n{}\n{}",
                    include_str!("integer.glsl"),
                    include_str!("image_filter.frag")
                ),
            )?);
        }
        if kind == 0 {
            renderer.ensure_table(self, filter)?;
        }
        let stored = image.plane(false)?;
        let extent = if stored.size != image.size {
            Size {
                width: self.tile_edge.min(image.size.width),
                height: self.tile_edge.min(image.size.height),
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
        let extent = Size {
            width: extent.width.min(area.width),
            height: extent.height.min(area.height),
        };
        let reads_old = !matches!(
            filter.kind,
            Kind::RandomFill {
                legacy: false,
                hold_alpha: false,
                ..
            }
        );
        // The work framebuffer is distinct from the backing texture. Point
        // kernels can sample that backing directly after publishing old writes,
        // avoiding a tile-sized scratch image and its copy for every pass.
        let sequence = random::Sequence::new(filter.kind, area);
        let stream = sequence
            .as_ref()
            .map(|_| {
                let size = Size {
                    width: extent.width.max(extent.height),
                    height: 4,
                };
                let texture = self.device.sample_texture(size, &self.scratch)?;
                let bytes = Bytes::zeroed(size.rgba_bytes().unwrap(), &self.staging)?;
                Ok::<_, Error>((texture, bytes))
            })
            .transpose()?;
        let mut stream = stream;
        self.writable(image, area, false)?;
        let mut copy_extent = Size {
            width: 0,
            height: 0,
        };
        if reads_old {
            for tile in &image.plane(false)?.tiles {
                if self.device.streamed_uploads() && self.device.supports_work_draw(&tile.texture) {
                    continue;
                }
                if let Some(part) = area.intersection(tile.rectangle) {
                    copy_extent.width = copy_extent.width.max(part.width);
                    copy_extent.height = copy_extent.height.max(part.height);
                }
            }
        }
        let previous = (copy_extent.width != 0)
            .then(|| self.device.sample_texture(copy_extent, &self.scratch))
            .transpose()?;
        let program = renderer.programs[kind].as_ref().unwrap();
        for tile in &image.plane(false)?.tiles {
            let Some(part) = area.intersection(tile.rectangle) else {
                continue;
            };
            let work_source =
                self.device.streamed_uploads() && self.device.supports_work_draw(&tile.texture);
            if reads_old && !work_source {
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
            if let Some((texture, bytes)) = &mut stream {
                sequence.as_ref().unwrap().fill(
                    bytes.as_mut_slice(),
                    texture.size.width as usize,
                    part,
                );
                self.device.upload(texture, bytes.as_slice())?;
            }
            let local = Rect {
                left: part.left - tile.rectangle.left,
                top: part.top - tile.rectangle.top,
                ..part
            };
            let backing = if work_source && reads_old {
                self.device.backdrop(&tile.texture, local)?
            } else {
                None
            };
            // The old value is sampled separately; every fragment writes all
            // channels. Preserve only the pixels outside this operation.
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
            let (old, origin) = if let Some(backing) = backing {
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
            let table = stream
                .as_ref()
                .map(|(t, _)| t.as_ref())
                .or_else(|| renderer.table.as_ref().map(|(_, t)| t.as_ref()))
                .unwrap_or(&self.lookup);
            self.bind_texture(2, table)?;
            program.two(
                "u_table_size",
                table.size.width as f32,
                table.size.height as f32,
            );
            if let Some(sequence) = &sequence {
                program.two("u_stream_origin", part.left as f32, part.top as f32);
                program.four("u_data0", word(sequence.a));
                program.four("u_data1", word(sequence.c));
            }
            match filter.kind {
                Kind::Lookup => program.one("u_kind", 0.),
                Kind::Colorize { amount } => {
                    program.one("u_kind", 1.);
                    program.one("u_offset", f32::from(amount));
                }
                Kind::Modulate {
                    hue,
                    saturation,
                    luminance,
                } => program.three("u_color", [hue, saturation, luminance]),
                Kind::Noise { level, .. } => {
                    program.one("u_kind", f32::from(level.is_some()));
                    program.one("u_offset", level.unwrap_or(0) as f32);
                }
                Kind::Xor { color } => {
                    program.one("u_kind", 0.);
                    program.four("u_color", rgba(color));
                }
                Kind::Dither { width, height } => {
                    program.one("u_kind", 1.);
                    program.two("u_region", area.left as f32, area.top as f32);
                    program.two("u_operation", (width & 1) as f32, (height & 1) as f32);
                }
                Kind::RandomFill {
                    range,
                    under,
                    legacy,
                    monochrome,
                    hold_alpha,
                    ..
                } => {
                    program.four(
                        "u_operation",
                        [
                            f32::from(monochrome),
                            f32::from(range == 255),
                            f32::from(hold_alpha),
                            f32::from(legacy),
                        ],
                    );
                    program.four("u_data2", word(range as u32));
                    program.four("u_data3", word(under as u32));
                }
                _ => unreachable!(),
            }
            unsafe {
                self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            }
            self.device.check()?;
        }
        Ok(())
    }
}

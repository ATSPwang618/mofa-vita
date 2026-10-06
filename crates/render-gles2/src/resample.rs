use crate::{
    Error, Gpu, Image, Result, device::Texture, drawing::rect, image::Plane, shader::Program,
};
use glow::HasContext;
use krkr_protocol::{
    graphics::{Rect, Size},
    pixels::Bytes,
    transform::{Filter, ImageOperation, Sampling, StretchRect, Transform},
};
use krkr_render::{resample::Axis, transform::Mapping};
use std::rc::Rc;

const TAPS: usize = 64;
const COEFFICIENT_WIDTH: u32 = TAPS as u32 + 2;
#[path = "resample_direct.rs"]
mod direct;

impl Gpu {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn resample(
        &self,
        image: &mut Image,
        source: &Image,
        rectangle: Rect,
        destination: StretchRect,
        sampling: Sampling,
        operation: ImageOperation,
        clip: Rect,
    ) -> Result<()> {
        if !sampling.sharpness.is_finite() {
            return Err(Error::Message("filter sharpness must be finite"));
        }
        if sampling.filter == Filter::Area
            && (destination.width.unsigned_abs() > rectangle.width
                || destination.height.unsigned_abs() > rectangle.height)
        {
            return Ok(());
        }
        let Some(mapping) = Mapping::new(rectangle, Transform::Stretch(destination), clip)? else {
            return Ok(());
        };
        let region = mapping.bounds;
        let x = self.filter_axes.borrow_mut().get(
            rectangle.left,
            rectangle.width,
            destination.left,
            destination.width,
            region.left,
            region.width,
            sampling,
            &self.staging,
        )?;
        let y = self.filter_axes.borrow_mut().get(
            rectangle.top,
            rectangle.height,
            destination.top,
            destination.height,
            region.top,
            region.height,
            sampling,
            &self.staging,
        )?;
        if x.data.iter().chain(&y.data).any(|value| !value.is_finite()) {
            return Err(Error::Message(
                "filter coefficients exceed renderer precision",
            ));
        }
        if self.device.streamed_uploads() {
            if let Some(result) = self.resample_direct(source, region, &x, &y)? {
                return self.apply_operation(
                    image,
                    &result,
                    result.size.rect(),
                    region.left,
                    region.top,
                    clip,
                    operation,
                );
            }
            // A single work FBO makes the byte-packed-float GPU algorithm
            // require four channels * both axes * tap batches * source tiles
            // of render/transfer barriers. Run these uncommon script filters
            // with native CPU floats, one stored-pixel read and one upload.
            // Ordinary copies, composition and nearest/fast-linear stay on GPU.
            return self.resample_rows(image, source, region, &x, &y, operation, clip);
        }
        if self.filter_program.borrow().is_none() {
            let program = Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &format!(
                    "{}\n{}",
                    include_str!("float.glsl"),
                    include_str!("resample.frag")
                ),
            )?;
            *self.filter_program.borrow_mut() = Some(program);
        }
        let program = self.filter_program.borrow();
        let program = program.as_ref().unwrap();
        let intermediate_size = Size {
            width: (x.range.end - x.range.start) as u32,
            height: region.height,
        };
        let intermediate =
            self.plane_with_edge(intermediate_size, &self.scratch, self.tile_edge.min(256))?;
        let main = source.plane(false)?;
        let ratio = [
            main.size.width as f32 / source.size.width as f32,
            main.size.height as f32 / source.size.height as f32,
        ];
        self.filter_axis(
            program,
            &intermediate,
            main,
            &y,
            true,
            [x.range.start, 0],
            ratio,
        )?;
        let output_size = Size {
            width: region.width,
            height: region.height,
        };
        let output = self.plane_with_edge(output_size, &self.scratch, self.tile_edge.min(256))?;
        self.filter_axis(
            program,
            &output,
            &intermediate,
            &x,
            false,
            [-x.range.start, 0],
            [1., 1.],
        )?;
        let result = Image {
            size: output_size,
            device: self.device.clone(),
            canvas: false,
            text: false,
            main: Some(output),
            province: None,
        };
        self.apply_operation(
            image,
            &result,
            output_size.rect(),
            region.left,
            region.top,
            clip,
            operation,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn resample_rows(
        &self,
        image: &mut Image,
        source: &Image,
        region: Rect,
        x: &Axis,
        y: &Axis,
        operation: ImageOperation,
        clip: Rect,
    ) -> Result<()> {
        let stored = source.plane(false)?.size;
        let mut view = source.shared();
        view.size = stored;
        let ratio = [
            stored.width as f32 / source.size.width as f32,
            stored.height as f32 / source.size.height as f32,
        ];
        let area = physical_region(
            Rect {
                left: x.range.start,
                top: y.range.start,
                width: (x.range.end - x.range.start) as u32,
                height: (y.range.end - y.range.start) as u32,
            },
            ratio,
            stored,
        );
        let size = Size {
            width: region.width,
            height: region.height,
        };
        // Admission precedes readback and all target mutation. Only one
        // intermediate row is kept, never a full floating-point image.
        let mut pixels = Bytes::zeroed(
            size.rgba_bytes()
                .ok_or(Error::Message("filter size overflow"))?,
            &self.staging,
        )?;
        let row_width = (x.range.end - x.range.start) as usize;
        let max_taps = y.data[..size.height as usize * 4]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|header| header[2] as usize)
            .max()
            .unwrap_or(0);
        let _row_permit = self.staging.reserve(
            row_width * (4 + std::mem::size_of::<usize>())
                + max_taps * std::mem::size_of::<usize>(),
        )?;
        let mut row = vec![[0u8; 4]; row_width];
        let columns: Vec<usize> = (0..row_width)
            .map(|column| {
                let logical_x = x.range.start + column as i32;
                (((logical_x as f32 + 0.5) * ratio[0]).floor() as i32 - area.left) as usize * 4
            })
            .collect();
        let mut rows = Vec::with_capacity(max_taps);
        let input = self.readback(&view, area, false)?;
        for output_y in 0..size.height as usize {
            let header = &y.data[output_y * 4..output_y * 4 + 4];
            let weights = &y.data[header[1] as usize..][..header[2] as usize];
            rows.clear();
            rows.extend((0..weights.len()).map(|tap| {
                let sy = (((header[0] + tap as f32 + 0.5) * ratio[1]).floor() as i32 - area.top)
                    as usize;
                sy * input.size.width as usize * 4
            }));
            let mut previous = None;
            for (&sx, value) in columns.iter().zip(&mut row) {
                // Compact assets can map several logical columns to the same
                // stored pixel. Reuse its exact rounded vertical result;
                // keep the original tap order and intermediate u8 rounding.
                if let Some((column, pixel)) = previous
                    && column == sx
                {
                    *value = pixel;
                    continue;
                }
                let mut sum = [0f32; 4];
                for (&offset, &weight) in rows.iter().zip(weights) {
                    let pixel = &input.data.as_slice()[offset + sx..][..4];
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += f32::from(pixel[channel]) * weight;
                    }
                }
                *value = sum.map(|value| value.clamp(0., 255.).floor() as u8);
                previous = Some((sx, *value));
            }
            let output = &mut pixels.as_mut_slice()[output_y * size.width as usize * 4..]
                [..size.width as usize * 4];
            for (column, pixel) in output.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let header = &x.data[column * 4..column * 4 + 4];
                let weights = &x.data[header[1] as usize..][..header[2] as usize];
                let first = (header[0] as i32 - x.range.start) as usize;
                let mut sum = [0f32; 4];
                for (value, &weight) in row[first..].iter().zip(weights) {
                    for channel in 0..4 {
                        sum[channel] += f32::from(value[channel]) * weight;
                    }
                }
                pixel.copy_from_slice(&sum.map(|value| value.clamp(0., 255.).floor() as u8));
            }
        }
        drop(input);
        let mut result = self.create_surface_image(size)?;
        self.upload(
            &mut result,
            &krkr_protocol::pixels::Pixels {
                size,
                main: Some(pixels),
                province: None,
            },
        )?;
        self.apply_operation(
            image,
            &result,
            size.rect(),
            region.left,
            region.top,
            clip,
            operation,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn filter_axis(
        &self,
        program: &Program,
        target: &Plane,
        source: &Plane,
        axis: &Axis,
        vertical: bool,
        offset: [i32; 2],
        ratio: [f32; 2],
    ) -> Result<()> {
        for tile in &target.tiles {
            let size = tile.texture.size;
            let first = if vertical {
                tile.rectangle.top
            } else {
                tile.rectangle.left
            } as usize;
            let rows = if vertical { size.height } else { size.width } as usize;
            let maximum = (first..first + rows)
                .map(|row| axis.data[row * 4 + 2] as usize)
                .max()
                .unwrap();
            let mut previous = self.device.texture(size, &self.scratch)?;
            let mut next = self.device.texture(size, &self.scratch)?;
            let weights = self.device.sample_texture(
                Size {
                    width: COEFFICIENT_WIDTH,
                    height: rows as u32,
                },
                &self.scratch,
            )?;
            let mut packed = Bytes::zeroed(COEFFICIENT_WIDTH as usize * rows * 4, &self.staging)?;
            for channel in 0..4 {
                unsafe {
                    let gl = &self.device.gl;
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(previous.framebuffer()?));
                    gl.enable(glow::SCISSOR_TEST);
                    gl.scissor(
                        0,
                        0,
                        previous.size.width as i32,
                        previous.size.height as i32,
                    );
                    gl.color_mask(true, true, true, true);
                    gl.clear_color(0., 0., 0., 0.);
                    gl.clear(glow::COLOR_BUFFER_BIT);
                }
                let mut select = [0.; 4];
                select[channel] = 1.;
                for base in (0..maximum).step_by(TAPS) {
                    let mut lo = i32::MAX;
                    let mut hi = i32::MIN;
                    for row in 0..rows {
                        let header = &axis.data[(first + row) * 4..(first + row) * 4 + 4];
                        let count = (header[2] as usize).saturating_sub(base).min(TAPS);
                        let start = header[0] as i32 + base as i32;
                        if count != 0 {
                            lo = lo.min(start);
                            hi = hi.max(start + count as i32);
                        }
                        let output = &mut packed.as_mut_slice()[row * COEFFICIENT_WIDTH as usize * 4
                            ..(row + 1) * COEFFICIENT_WIDTH as usize * 4];
                        output[..4].copy_from_slice(&(start as f32).to_le_bytes());
                        output[4..8].copy_from_slice(&(count as f32).to_le_bytes());
                        for k in 0..count {
                            output[(k + 2) * 4..(k + 3) * 4].copy_from_slice(
                                &axis.data[header[1] as usize + base + k].to_le_bytes(),
                            );
                        }
                    }
                    self.device.upload(&weights, packed.as_slice())?;
                    let logical = if vertical {
                        Rect {
                            left: tile.rectangle.left + offset[0],
                            top: lo + offset[1],
                            width: size.width,
                            height: (hi - lo) as u32,
                        }
                    } else {
                        Rect {
                            left: lo + offset[0],
                            top: tile.rectangle.top + offset[1],
                            width: (hi - lo) as u32,
                            height: size.height,
                        }
                    };
                    let physical = physical_region(logical, ratio, source.size);
                    for input in &source.tiles {
                        if physical.intersection(input.rectangle).is_none() {
                            continue;
                        }
                        self.filter_target(program, &next, tile.rectangle, [true; 4])?;
                        self.bind_texture(0, &input.texture)?;
                        program.four("u_source_visible", crate::drawing::rect(input.rectangle));
                        self.bind_texture(1, &previous)?;
                        self.bind_texture(2, &weights)?;
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
                        program.two("u_source_scale", ratio[0], ratio[1]);
                        program.two("u_weights_size", COEFFICIENT_WIDTH as f32, rows as f32);
                        program.two("u_offset", offset[0] as f32, offset[1] as f32);
                        program.four("u_channel", select);
                        program.one("u_axis", f32::from(vertical));
                        program.one("u_resample_kind", 0.);
                        unsafe {
                            self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                        }
                        std::mem::swap(&mut previous, &mut next);
                    }
                }
                let mut mask = [false; 4];
                mask[channel] = true;
                self.filter_target(program, &tile.texture, tile.rectangle, mask)?;
                self.bind_texture(0, &self.lookup)?;
                self.bind_texture(1, &previous)?;
                self.bind_texture(2, &weights)?;
                program.one("u_resample_kind", 1.);
                unsafe {
                    self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                }
            }
            self.device.check()?;
        }
        Ok(())
    }
    fn filter_target(
        &self,
        program: &Program,
        target: &Rc<Texture>,
        area: Rect,
        mask: [bool; 4],
    ) -> Result<()> {
        program.bind();
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target.framebuffer()?));
            gl.viewport(0, 0, target.size.width as i32, target.size.height as i32);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.color_mask(mask[0], mask[1], mask[2], mask[3]);
        }
        program.four("u_target", rect(area));
        program.four("u_rectangle", rect(area));
        program.one("u_flip", 1.);
        Ok(())
    }
}
fn physical_region(logical: Rect, ratio: [f32; 2], size: Size) -> Rect {
    // One guard texel covers precision at a compact image's sample boundary.
    let left = ((logical.left as f64 * f64::from(ratio[0])).floor() - 1.).max(0.) as i32;
    let top = ((logical.top as f64 * f64::from(ratio[1])).floor() - 1.).max(0.) as i32;
    let right =
        (((i64::from(logical.left) + i64::from(logical.width)) as f64 * f64::from(ratio[0])).ceil()
            + 1.)
            .min(f64::from(size.width)) as i32;
    let bottom =
        (((i64::from(logical.top) + i64::from(logical.height)) as f64 * f64::from(ratio[1])).ceil()
            + 1.)
            .min(f64::from(size.height)) as i32;
    Rect {
        left,
        top,
        width: (right - left).max(0) as u32,
        height: (bottom - top).max(0) as u32,
    }
}

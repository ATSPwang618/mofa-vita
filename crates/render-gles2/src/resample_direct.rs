//! Bounded separable kernels keep all four channel sums in fragment registers.
//! No framebuffer readback, float render target or per-channel transfer passes.
use super::*;

impl Gpu {
    pub(super) fn resample_direct(
        &self,
        source: &Image,
        region: Rect,
        x: &Axis,
        y: &Axis,
    ) -> Result<Option<Image>> {
        let main = source.plane(false)?;
        let size = Size {
            width: region.width,
            height: region.height,
        };
        if main.tiles.len() != 1
            || main.tiles[0].backing.is_some()
            || main.tiles[0].rectangle != main.size.rect()
            || size.width > self.tile_edge
            || size.height > self.tile_edge
        {
            return Ok(None);
        }
        let maximum = [(x, size.width), (y, size.height)]
            .into_iter()
            .flat_map(|(axis, count)| axis.data[..count as usize * 4].as_chunks::<4>().0.iter())
            .map(|header| header[2] as usize)
            .max()
            .unwrap_or(0);
        let Some(index) = crate::resample_source::TAPS
            .iter()
            .position(|&taps| maximum <= taps)
        else {
            return Ok(None);
        };
        let taps = crate::resample_source::TAPS[index];
        let width = taps + 2;
        let ratio = [
            main.size.width as f32 / source.size.width as f32,
            main.size.height as f32 / source.size.height as f32,
        ];
        let area = super::physical_region(
            Rect {
                left: x.range.start,
                top: y.range.start,
                width: (x.range.end - x.range.start) as u32,
                height: (y.range.end - y.range.start) as u32,
            },
            ratio,
            main.size,
        );
        // Compact assets need physical intermediate columns, not an expanded
        // PC-resolution texture. The horizontal pass maps logical taps to it.
        let intermediate_size = Size {
            width: area.width,
            height: size.height,
        };
        if intermediate_size.width > self.tile_edge || intermediate_size.width == 0 {
            return Ok(None);
        }
        let bytes = intermediate_size.rgba_bytes().unwrap()
            + size.rgba_bytes().unwrap()
            + width * (size.width + size.height) as usize * 4;
        if bytes > self.scratch.available() {
            return Ok(None);
        }
        if self.direct_filter_program.borrow()[index].is_none() {
            self.direct_filter_program.borrow_mut()[index] = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                &crate::resample_source::fragment(taps),
            )?);
        }
        let program = self.direct_filter_program.borrow();
        let program = program[index].as_ref().unwrap();
        let intermediate = self.overwrite_plane(intermediate_size, &self.scratch)?;
        let output = self.overwrite_plane(size, &self.scratch)?;
        self.direct_filter_axis(
            program,
            &intermediate.tiles[0].texture,
            &main.tiles[0].texture,
            y,
            width,
            true,
            [0., 0.],
            [area.left as f32, 0.],
            [1., ratio[1]],
        )?;
        self.direct_filter_axis(
            program,
            &output.tiles[0].texture,
            &intermediate.tiles[0].texture,
            x,
            width,
            false,
            [area.left as f32, 0.],
            [0., 0.],
            [ratio[0], 1.],
        )?;
        Ok(Some(Image {
            size,
            device: self.device.clone(),
            canvas: false,
            text: false,
            main: Some(output),
            province: None,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn direct_filter_axis(
        &self,
        program: &Program,
        target: &Rc<Texture>,
        source: &Rc<Texture>,
        axis: &Axis,
        width: usize,
        vertical: bool,
        origin: [f32; 2],
        offset: [f32; 2],
        ratio: [f32; 2],
    ) -> Result<()> {
        let rows = if vertical {
            target.size.height
        } else {
            target.size.width
        } as usize;
        let mut packed = Bytes::zeroed(width * rows * 4, &self.staging)?;
        for (row, bytes) in packed
            .as_mut_slice()
            .chunks_exact_mut(width * 4)
            .enumerate()
        {
            let header = &axis.data[row * 4..row * 4 + 4];
            bytes[..4].copy_from_slice(&header[0].to_le_bytes());
            bytes[4..8].copy_from_slice(&header[2].to_le_bytes());
            for k in 0..header[2] as usize {
                bytes[(k + 2) * 4..(k + 3) * 4]
                    .copy_from_slice(&axis.data[header[1] as usize + k].to_le_bytes());
            }
        }
        let weights = self.device.sample_texture(
            Size {
                width: width as u32,
                height: rows as u32,
            },
            &self.scratch,
        )?;
        self.device.upload(&weights, packed.as_slice())?;
        program.bind();
        unsafe {
            let gl = &self.device.gl;
            gl.bind_framebuffer(
                glow::FRAMEBUFFER,
                Some(target.overwrite_framebuffer(target.size.rect())?),
            );
            gl.viewport(0, 0, target.size.width as i32, target.size.height as i32);
            gl.disable(glow::SCISSOR_TEST);
            gl.disable(glow::BLEND);
            gl.color_mask(true, true, true, true);
        }
        self.bind_texture(0, source)?;
        self.bind_texture(2, &weights)?;
        program.four("u_target", rect(target.size.rect()));
        program.four("u_rectangle", rect(target.size.rect()));
        program.one("u_flip", 1.);
        program.one("u_axis", f32::from(vertical));
        program.two("u_source_origin", origin[0], origin[1]);
        program.two(
            "u_source_size",
            source.size.width as f32,
            source.size.height as f32,
        );
        program.two("u_source_scale", ratio[0], ratio[1]);
        program.two("u_offset", offset[0], offset[1]);
        program.two("u_weights_size", width as f32, rows as f32);
        unsafe { self.device.gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4) };
        self.device.check()
    }
}

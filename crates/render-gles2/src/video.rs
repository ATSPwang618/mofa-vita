use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::{Draw, rect},
    image::{Plane, Tile},
    shader::Program,
};
use glow::HasContext;
use krkr_protocol::{
    graphics::{DrawFace, Rect, Size},
    pixels::{Yuv420, Yuv420Layout},
};
use std::rc::Rc;

#[derive(Default)]
pub(crate) struct Renderer {
    program: Option<Program>,
    planes: Option<[Rc<Texture>; 2]>,
}
impl Gpu {
    pub fn upload_yuv(&self, pixels: &Yuv420, logical: Size) -> Result<Image> {
        let size = pixels.size;
        if Yuv420::byte_len(size) != Some(pixels.data.as_slice().len()) {
            return Err(Error::Message(
                "invalid YUV420 frame dimensions or byte count",
            ));
        }
        let mut renderer = self.video.borrow_mut();
        if renderer.program.is_none() {
            renderer.program = Some(Program::new(
                self.device.clone(),
                include_str!("quad.vert"),
                include_str!("video.frag"),
            )?);
        }
        let chroma_size = match pixels.layout {
            Yuv420Layout::Yv12 => Size {
                width: size.width / 2,
                height: size.height,
            },
            Yuv420Layout::Nv12 => Size {
                width: size.width / 2,
                height: size.height / 2,
            },
        };
        if renderer
            .planes
            .as_ref()
            .is_none_or(|p| p[0].size != size || p[1].size != chroma_size)
        {
            renderer.planes.take();
            renderer.planes = Some([
                self.device.mask_texture(size, &self.resident, false)?,
                match pixels.layout {
                    Yuv420Layout::Nv12 => self
                        .device
                        .nv12_chroma_texture(chroma_size, &self.resident)?,
                    Yuv420Layout::Yv12 => {
                        self.device
                            .mask_texture(chroma_size, &self.resident, false)?
                    }
                },
            ]);
        }
        let planes = renderer.planes.as_ref().unwrap();
        let y = size.width as usize * size.height as usize;
        self.device
            .upload(&planes[0], &pixels.data.as_slice()[..y])?;
        self.device
            .upload(&planes[1], &pixels.data.as_slice()[y..])?;
        // Conversion overwrites every output pixel, including alpha. Avoid
        // clearing and loading an RGBA frame immediately before replacing it.
        let image = Image {
            size,
            device: self.device.clone(),
            canvas: false,
            text: false,
            main: Some(self.video_plane(size)?),
            province: None,
        };
        let program = renderer.program.as_ref().unwrap();
        program.bind();
        self.bind_texture(0, &planes[0])?;
        self.bind_texture(1, &planes[1])?;
        program.two("u_source_size", size.width as f32, size.height as f32);
        program.one("u_nv12", f32::from(pixels.layout == Yuv420Layout::Nv12));
        program.one("u_flip", 1.);
        unsafe {
            let gl = &self.device.gl;
            gl.disable(glow::BLEND);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            for tile in &image.plane(false)?.tiles {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(tile.texture.video_framebuffer()?));
                gl.viewport(
                    0,
                    0,
                    tile.rectangle.width as i32,
                    tile.rectangle.height as i32,
                );
                program.four("u_target", rect(tile.rectangle));
                program.four("u_rectangle", rect(tile.rectangle));
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                // The conversion shader writes alpha=1 for every texel.
                // Keep that fact with this texture version for mask queries.
                tile.texture.set_uniform_alpha(255);
            }
        }
        self.device.check()?;
        self.logical_image(image, logical)
    }
    fn video_plane(&self, size: Size) -> Result<Rc<Plane>> {
        let edge = self.tile_edge;
        let mut tiles = Vec::new();
        for top in (0..size.height).step_by(edge as usize) {
            for left in (0..size.width).step_by(edge as usize) {
                let extent = Size {
                    width: edge.min(size.width - left),
                    height: edge.min(size.height - top),
                };
                // Video overwrites every channel. Reuse retired native output
                // attachments in GL command order, without a work-surface
                // store or a global finish. Live snapshots retain their own
                // allocations and can never enter this reuse path.
                tiles.push(Tile {
                    rectangle: Rect {
                        left: left as i32,
                        top: top as i32,
                        ..extent.rect()
                    },
                    backing: None,
                    texture: self.device.render_texture(extent, &self.resident)?,
                });
            }
        }
        Ok(Rc::new(Plane {
            size,
            tiles,
            budget: self.resident.clone(),
        }))
    }
    pub fn copy_yuv(
        &self,
        target: &mut Image,
        pixels: &Yuv420,
        logical: Size,
        split_alpha: bool,
        limit: Size,
    ) -> Result<()> {
        self.check_image(target)?;
        let source = self.upload_yuv(pixels, logical)?;
        let width = logical.width / if split_alpha { 2 } else { 1 };
        let size = Size {
            width: width.min(target.size.width).min(limit.width),
            height: logical.height.min(target.size.height).min(limit.height),
        };
        if size.width == 0 || size.height == 0 {
            return Ok(());
        }
        self.copy_rect(
            target,
            &source,
            size.rect(),
            0,
            0,
            size.rect(),
            DrawFace::Alpha,
            false,
        )?;
        if split_alpha {
            let plane = source.plane(false)?;
            let sx = plane.size.width as f32 / logical.width as f32;
            let sy = plane.size.height as f32 / logical.height as f32;
            self.draw(
                target.plane(false)?,
                Some(plane),
                size.rect(),
                &Draw {
                    kind: 8.,
                    ..Draw::copy(
                        [
                            sx,
                            0.,
                            (width as f32 + 0.5) * sx - 0.5,
                            0.,
                            sy,
                            (sy - 1.) * 0.5,
                        ],
                        [false, false, false, true],
                    )
                },
            )?;
        }
        Ok(())
    }
}

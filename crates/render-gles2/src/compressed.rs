use crate::{
    Error, Gpu, Image, Result,
    image::{Plane, Tile},
};
use krkr_protocol::{
    graphics::Size,
    texture::{Compressed, Format},
};
use std::rc::Rc;

impl Gpu {
    pub fn supports_compressed(&self, texture: &Compressed) -> bool {
        texture.tile_size.width <= self.tile_edge && texture.tile_size.height <= self.tile_edge
            // PVR's NPOT layout hint selects STRIDE even for ETC1, but its
            // TextureUpload STRIDE path explicitly excludes compressed data.
            // POT textures force the supported twiddled hardware path. Tiny
            // levels also use PVRTC's minimum allocation; decode those normally.
            && texture.tile_size.width >= 8 && texture.tile_size.height >= 8
            && texture.tile_size.width.is_power_of_two() && texture.tile_size.height.is_power_of_two()
            && Compressed::payload_len(texture.size, texture.tile_size, texture.format) == Some(texture.data().len())
            && self.device.supports_compressed_format(texture.format)
    }
    /// No RGBA reservation, zero-fill upload or CPU expansion. Drawing samples the
    /// compressed blocks; the first write detaches only the affected tiles.
    pub fn load_compressed(&self, texture: &Compressed) -> Result<Image> {
        if !self.supports_compressed(texture) {
            return Err(Error::Message(
                "native compressed texture unavailable for this image",
            ));
        }
        // ETC1/PVRTC still retain host levels until first sampled. BC uploads
        // go directly to device storage and need no draw or completion wait.
        const UPLOAD_BATCH_BYTES: usize = 4 * 1024 * 1024;
        let deferred = self.device.streamed_uploads()
            && matches!(texture.format, Format::Etc1 | Format::Pvrtc1Rgba4);
        let mut pending = Vec::new();
        let mut pending_bytes = 0usize;
        let mut tiles = Vec::new();
        for (rectangle, data) in texture.tiles() {
            // BC3's RGB and alpha endpoints both zero prove exact transparent
            // black, including hidden RGB. Empty atlas tiles need no upload.
            let transparent = texture.format.linear() == Format::Bc3Rgba
                && data
                    .as_chunks::<16>()
                    .0
                    .iter()
                    .all(|block| block[..12].iter().all(|&byte| byte == 0));
            if deferred
                && !transparent
                && !pending.is_empty()
                && pending_bytes.saturating_add(data.len()) > UPLOAD_BATCH_BYTES
            {
                self.device.make_textures_resident(&pending)?;
                pending.clear();
                pending_bytes = 0;
            }
            let tile = if transparent {
                self.canvas_solid_tile(
                    Size {
                        width: 1,
                        height: 1,
                    },
                    0,
                    true,
                )?
            } else {
                let tile = self.device.compressed_texture(
                    texture.tile_size,
                    texture.format,
                    data,
                    &self.resident,
                )?;
                if deferred {
                    pending.push((tile.name(), tile.size));
                    pending_bytes = pending_bytes.saturating_add(data.len());
                }
                tile
            };
            tiles.push(Tile {
                backing: None,
                rectangle,
                texture: tile,
            });
        }
        if !pending.is_empty() {
            self.device.make_textures_resident(&pending)?;
        }
        Ok(Image {
            size: texture.size,
            device: self.device.clone(),
            canvas: true,
            text: false,
            province: None,
            main: Some(Rc::new(Plane {
                size: texture.size,
                budget: self.resident.clone(),
                tiles,
            })),
        })
    }
}

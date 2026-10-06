//! Bounded, lossless fallback for otherwise unrenderable memory pressure.
//! Only fully owned RGBA canvas planes qualify; snapshots stay on GPU.
use crate::{
    Error, Gpu, Image, Result,
    image::{Plane, Tile},
};
use glow::HasContext;
use krkr_protocol::{
    budget::Budget,
    graphics::{DrawFace, Fill, Rect, Size},
    pixels::Bytes,
};
use std::{
    io::{self, Read, Write},
    rc::Rc,
};

pub struct SpilledImage {
    logical: Size,
    text: bool,
    stored: Size,
    tiles: Vec<(Rect, Payload)>,
    bytes: usize,
}
enum Payload {
    Solid(u32),
    Raw(Bytes),
    // Independent PNG Sub prediction per RGBA row, then zlib. No prior strip
    // is needed to restore a row, so decoding keeps its bounded workspace.
    Zlib(Vec<Bytes>),
}

struct PackedWriter<'a> {
    budget: &'a Budget,
    limit: usize,
    bytes: usize,
    chunks: Vec<Bytes>,
}
impl<'a> PackedWriter<'a> {
    fn new(budget: &'a Budget, raw: usize) -> Self {
        Self {
            budget,
            limit: raw.saturating_sub((raw / 32).max(64)),
            bytes: 0,
            chunks: Vec::new(),
        }
    }
}
impl Write for PackedWriter<'_> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.len() > self.limit.saturating_sub(self.bytes) {
            return Err(io::Error::other("canvas compression did not save space"));
        }
        if !input.is_empty() {
            let mut chunk = Bytes::zeroed(input.len(), self.budget).map_err(io::Error::other)?;
            chunk.as_mut_slice().copy_from_slice(input);
            self.chunks.push(chunk);
            self.bytes += input.len();
        }
        Ok(input.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct PackedReader<'a> {
    chunks: &'a [Bytes],
    offset: usize,
}
impl Read for PackedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let Some(chunk) = self.chunks.first() else {
            return Ok(0);
        };
        let data = &chunk.as_slice()[self.offset..];
        let count = data.len().min(output.len());
        output[..count].copy_from_slice(&data[..count]);
        self.offset += count;
        if self.offset == chunk.as_slice().len() {
            self.chunks = &self.chunks[1..];
            self.offset = 0;
        }
        Ok(count)
    }
}

fn predict_rows(data: &mut [u8], stride: usize) {
    for row in data.chunks_exact_mut(stride) {
        for i in (4..row.len()).rev() {
            row[i] = row[i].wrapping_sub(row[i - 4]);
        }
    }
}
fn restore_rows(data: &mut [u8], stride: usize) {
    for row in data.chunks_exact_mut(stride) {
        for i in 4..row.len() {
            row[i] = row[i].wrapping_add(row[i - 4]);
        }
    }
}
impl SpilledImage {
    pub fn gpu_bytes(&self) -> usize {
        self.bytes
    }
    pub fn staging_bytes(&self) -> usize {
        self.tiles
            .iter()
            .map(|(_, payload)| match payload {
                Payload::Solid(_) => 0,
                Payload::Raw(bytes) => bytes.as_slice().len(),
                Payload::Zlib(chunks) => chunks.iter().map(|b| b.as_slice().len()).sum(),
            })
            .sum()
    }
}
impl Image {
    pub fn same_canvas_storage(&self, other: &Image) -> bool {
        self.canvas
            && other.canvas
            && self.text == other.text
            && self.size == other.size
            && self.province.is_none()
            && other.province.is_none()
            && self
                .main
                .as_ref()
                .zip(other.main.as_ref())
                .is_some_and(|(a, b)| Rc::ptr_eq(a, b))
    }
}
impl Gpu {
    pub fn spillable_bytes(&self, image: &Image) -> usize {
        self.spillable_owned_bytes(image, 1)
    }
    fn spillable_owned_bytes(&self, image: &Image, owners: usize) -> usize {
        let Some(plane) = &image.main else { return 0 };
        if !self.device.streamed_uploads()
            || !image.canvas
            || image.province.is_some()
            || Rc::strong_count(plane) != owners
            || plane.tiles.len() > 256
        {
            return 0;
        }
        let mut bytes = 0;
        for tile in &plane.tiles {
            if tile.texture.solid_color().is_some() {
                continue;
            }
            let backing = tile.sample_rectangle();
            if !tile.texture.renderable()
                || backing.width != tile.texture.size.width
                || backing.height != tile.texture.size.height
                || tile.rectangle.intersection(backing) != Some(tile.rectangle)
            {
                return 0;
            }
            // Cropped views can keep a much larger source allocation alive.
            // Save just their exact texel rectangle. Remaining views continue
            // owning the source until they too are evicted or released.
            bytes += tile.size().rgba_bytes().unwrap();
        }
        bytes
    }
    pub fn spill_canvas(&self, image: &Image) -> Result<Option<SpilledImage>> {
        self.spill_canvas_group(&[image])
    }
    pub fn spill_group_bytes(&self, images: &[&Image]) -> usize {
        let Some(first) = images.first() else {
            return 0;
        };
        if images
            .iter()
            .enumerate()
            .any(|(i, image)| images[..i].iter().any(|old| std::ptr::eq(*old, *image)))
        {
            return 0;
        }
        if images.iter().any(|image| !first.same_canvas_storage(image)) {
            return 0;
        }
        self.spillable_owned_bytes(first, images.len())
    }
    pub fn spill_canvas_group(&self, images: &[&Image]) -> Result<Option<SpilledImage>> {
        let _profile = krkr_protocol::profile::span("gpu.spill");
        let bytes = self.spill_group_bytes(images);
        if bytes < 256 * 1024 {
            return Ok(None);
        }
        let image = images[0];
        self.check_image(image)?;
        let plane = image.main.as_ref().unwrap();
        let mut tiles = Vec::with_capacity(plane.tiles.len());
        for tile in &plane.tiles {
            if let Some(color) = tile.texture.solid_color() {
                tiles.push((tile.rectangle, Payload::Solid(color)));
                continue;
            }
            let size = tile.size();
            let len = size.rgba_bytes().unwrap();
            // Under pressure, reading the full tile would either fail or
            // leave no space to compress it. Stream through a small strip
            // before falling back to an incompressible raw allocation.
            if len.saturating_mul(2).saturating_add(512 * 1024) > self.staging.available()
                && let Some(packed) = self.spill_compressed_strips(tile)?
            {
                tiles.push((tile.rectangle, Payload::Zlib(packed)));
                continue;
            }
            if len > self.staging.available() {
                return Ok(None);
            }
            let mut raw = Bytes::zeroed(len, &self.staging)?;
            {
                let _profile = krkr_protocol::profile::span("gpu.spill.readback");
                let backing = tile.sample_rectangle();
                unsafe {
                    self.device.gl.bind_framebuffer(
                        glow::FRAMEBUFFER,
                        Some(tile.texture.read_framebuffer()?),
                    );
                    self.device.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    self.device.gl.read_pixels(
                        tile.rectangle.left - backing.left,
                        tile.rectangle.top - backing.top,
                        size.width as i32,
                        size.height as i32,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(raw.as_mut_slice())),
                    );
                }
                self.device.check()?;
            }
            // CPU staging is a separate pool. Incompressible pictures can
            // still release GPU storage by keeping this already-read buffer.
            // Compression is optional, including its workspace and output.
            if 512 * 1024 >= self.staging.available() {
                tiles.push((tile.rectangle, Payload::Raw(raw)));
                continue;
            }
            let _codec = self.staging.reserve(512 * 1024)?;
            let _profile = krkr_protocol::profile::span("gpu.spill.compress");
            let mut encoder = flate2::write::ZlibEncoder::new(
                PackedWriter::new(&self.staging, len),
                flate2::Compression::fast(),
            );
            predict_rows(raw.as_mut_slice(), size.width as usize * 4);
            if encoder.write_all(raw.as_slice()).is_err() {
                restore_rows(raw.as_mut_slice(), size.width as usize * 4);
                tiles.push((tile.rectangle, Payload::Raw(raw)));
                continue;
            }
            let Ok(packed) = encoder.finish() else {
                restore_rows(raw.as_mut_slice(), size.width as usize * 4);
                tiles.push((tile.rectangle, Payload::Raw(raw)));
                continue;
            };
            tiles.push((tile.rectangle, Payload::Zlib(packed.chunks)));
        }
        Ok(Some(SpilledImage {
            logical: image.size,
            text: image.text,
            stored: plane.size,
            tiles,
            bytes: bytes + 4 * plane.tiles.len(),
        }))
    }

    /// GPU storage actually released by parking this group. Other planes can
    /// retain the same texture through crop views even when the plane is owned.
    pub fn spill_group_reclaim_bytes(&self, images: &[&Image]) -> usize {
        self.spill_batch_reclaim_bytes(&[images])
    }

    /// Count allocations released when all these plane groups are parked.
    /// A crop view in another group otherwise makes each group appear unable
    /// to free memory, even though retiring both releases their shared backing.
    pub fn spill_batch_reclaim_bytes(&self, groups: &[&[&Image]]) -> usize {
        let mut planes = std::collections::HashSet::new();
        let mut textures = std::collections::HashMap::new();
        for images in groups {
            if self.spill_group_bytes(images) == 0 {
                return 0;
            }
            let plane = images[0].main.as_ref().unwrap();
            if !planes.insert(Rc::as_ptr(plane)) {
                return 0;
            }
            for tile in &plane.tiles {
                let entry = textures
                    .entry(Rc::as_ptr(&tile.texture))
                    .or_insert((&tile.texture, 0));
                entry.1 += 1;
            }
        }
        textures
            .values()
            .filter(|(texture, owners)| Rc::strong_count(texture) == *owners)
            .map(|(texture, _)| texture.allocation_bytes())
            .sum()
    }

    fn spill_compressed_strips(&self, tile: &Tile) -> Result<Option<Vec<Bytes>>> {
        let size = tile.size();
        let stride = size.width as usize * 4;
        // Leave room for the codec and a worst-case packed tile, then batch
        // readbacks rather than synchronizing the GPU every 64 KiB.
        let strip_limit = self
            .staging
            .available()
            .saturating_sub(512 * 1024)
            .saturating_sub(size.rgba_bytes().unwrap())
            .clamp(64 * 1024, 512 * 1024);
        let rows = (strip_limit / stride).max(1).min(size.height as usize);
        let strip_bytes = rows * stride;
        if self.staging.available() <= 512 * 1024 + strip_bytes {
            return Ok(None);
        }
        let _codec = self.staging.reserve(512 * 1024)?;
        let mut raw = Bytes::zeroed(strip_bytes, &self.staging)?;
        let mut encoder = flate2::write::ZlibEncoder::new(
            PackedWriter::new(&self.staging, size.rgba_bytes().unwrap()),
            flate2::Compression::fast(),
        );
        let backing = tile.sample_rectangle();
        for top in (0..size.height).step_by(rows) {
            let height = (size.height - top).min(rows as u32);
            let strip = &mut raw.as_mut_slice()[..height as usize * stride];
            {
                let _profile = krkr_protocol::profile::span("gpu.spill.readback");
                unsafe {
                    self.device.gl.bind_framebuffer(
                        glow::FRAMEBUFFER,
                        Some(tile.texture.read_framebuffer()?),
                    );
                    self.device.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    self.device.gl.read_pixels(
                        tile.rectangle.left - backing.left,
                        tile.rectangle.top - backing.top + top as i32,
                        size.width as i32,
                        height as i32,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(strip)),
                    );
                }
                self.device.check()?;
            }
            let _profile = krkr_protocol::profile::span("gpu.spill.compress");
            predict_rows(strip, stride);
            if encoder.write_all(strip).is_err() {
                return Ok(None);
            }
        }
        let Ok(packed) = encoder.finish() else {
            return Ok(None);
        };
        Ok(Some(packed.chunks))
    }
    /// Whole-plane replacement does not consume the parked pixels.
    pub fn fill_spilled_canvas(
        &self,
        saved: &SpilledImage,
        fills: &[Fill],
    ) -> Result<Option<Image>> {
        let [fill] = fills else {
            return Ok(None);
        };
        if matches!(fill.face, DrawFace::Province | DrawFace::Mask)
            || (fill.face == DrawFace::Opaque && fill.hold_alpha)
            || fill.rectangle.intersection(saved.logical.rect()) != Some(saved.logical.rect())
        {
            return Ok(None);
        }
        let mut image = self.create_image(saved.logical, fill.color)?;
        image.text = saved.text;
        Ok(Some(image))
    }
    /// Prepare a full affine-copy replacement without restoring dead pixels.
    /// The caller must exclude self-copies and operations preserving alpha.
    pub fn clear_spilled_affine(
        &self,
        saved: &SpilledImage,
        source: &Image,
        rectangle: Rect,
        points: [[f64; 2]; 3],
        clip: Rect,
        color: u32,
    ) -> Result<Option<Image>> {
        if rectangle.width == 0
            || rectangle.height == 0
            || clip.intersection(saved.logical.rect()) != Some(saved.logical.rect())
        {
            return Ok(None);
        }
        self.check_image(source)?;
        source.plane(false)?;
        krkr_render::transform::validate_source(rectangle, source.size)?;
        krkr_render::transform::Mapping::new(
            rectangle,
            krkr_protocol::transform::Transform::Affine(points),
            saved.logical.rect(),
        )?;
        let mut image = self.create_image(saved.logical, color)?;
        image.text = saved.text;
        Ok(Some(image))
    }

    pub fn restore_canvas(&self, image: &SpilledImage) -> Result<Image> {
        let _profile = krkr_protocol::profile::span("gpu.restore");
        let _codec = image
            .tiles
            .iter()
            .any(|(_, payload)| matches!(payload, Payload::Zlib(_)))
            .then(|| self.staging.reserve(128 * 1024))
            .transpose()?;
        let mut tiles = Vec::with_capacity(image.tiles.len());
        for (rectangle, payload) in &image.tiles {
            let size = Size {
                width: rectangle.width,
                height: rectangle.height,
            };
            let texture = match payload {
                Payload::Solid(color) => self.canvas_solid_tile(
                    Size {
                        width: 1,
                        height: 1,
                    },
                    *color,
                    true,
                )?,
                Payload::Raw(data) => {
                    self.device
                        .uploaded_texture(size, data.as_slice(), &self.resident)?
                }
                Payload::Zlib(data) => {
                    // Restore strips directly into idle texture storage. A
                    // full decompressed tile can exceed the remaining staging
                    // budget precisely when eviction is needed most.
                    let stride = size.width as usize * 4;
                    let rows = (64 * 1024 / stride).max(1).min(size.height as usize);
                    let mut raw = Bytes::zeroed(stride * rows, &self.staging)?;
                    let mut decoder = flate2::read::ZlibDecoder::new(PackedReader {
                        chunks: data,
                        offset: 0,
                    });
                    let texture = self.device.sample_texture(size, &self.resident)?;
                    for top in (0..size.height).step_by(rows) {
                        let height = (size.height - top).min(rows as u32);
                        let strip = &mut raw.as_mut_slice()[..stride * height as usize];
                        decoder
                            .read_exact(strip)
                            .map_err(|_| Error::Message("invalid spilled canvas"))?;
                        restore_rows(strip, stride);
                        self.device.upload_region(
                            &texture,
                            Rect {
                                left: 0,
                                top: top as i32,
                                width: size.width,
                                height,
                            },
                            strip,
                        )?;
                    }
                    let mut extra = [0];
                    if decoder
                        .read(&mut extra)
                        .map_err(|_| Error::Message("invalid spilled canvas"))?
                        != 0
                    {
                        return Err(Error::Message("spilled canvas size mismatch"));
                    }
                    texture
                }
            };
            tiles.push(Tile {
                rectangle: *rectangle,
                backing: None,
                texture,
            });
        }
        Ok(Image {
            size: image.logical,
            canvas: true,
            text: image.text,
            device: self.device.clone(),
            province: None,
            main: Some(Rc::new(Plane {
                size: image.stored,
                budget: self.resident.clone(),
                tiles,
            })),
        })
    }
}

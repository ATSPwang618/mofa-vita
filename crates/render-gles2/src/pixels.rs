use crate::{
    Error, Gpu, Image, Result,
    device::Texture,
    drawing::Draw,
    image::{Plane, Tile},
};
use glow::HasContext;
use krkr_protocol::{
    graphics::{DrawFace, Rect, Size},
    pixels::{Bytes, Pixels},
};
use std::rc::Rc;

pub struct ReadPixels {
    pub data: Bytes,
    pub size: Size,
    pub channels: usize,
}

fn readback_view(source: &Plane, rectangle: Rect) -> Option<(&Texture, Rect)> {
    let mut tiles = source.tiles.iter().filter_map(|tile| {
        tile.rectangle
            .intersection(rectangle)
            .map(|part| (tile, part))
    });
    let (first, part) = tiles.next()?;
    let backing = first.sample_rectangle();
    if !first.texture.renderable()
        || backing.width != first.texture.size.width
        || backing.height != first.texture.size.height
        || backing.intersection(rectangle) != Some(rectangle)
    {
        return None;
    }
    let mut covered = u64::from(part.width) * u64::from(part.height);
    for (tile, part) in tiles {
        if !Rc::ptr_eq(&first.texture, &tile.texture) || tile.sample_rectangle() != backing {
            return None;
        }
        covered += u64::from(part.width) * u64::from(part.height);
    }
    (covered == u64::from(rectangle.width) * u64::from(rectangle.height)).then_some((
        &first.texture,
        Rect {
            left: rectangle.left - backing.left,
            top: rectangle.top - backing.top,
            ..rectangle
        },
    ))
}

fn readback_extent(size: Size, tile_edge: u32, available: usize) -> Size {
    let width = size.width.min(tile_edge).min((available / 4).max(1) as u32);
    let height = size
        .height
        .min(tile_edge)
        .min((available / (width.max(1) as usize * 4)).max(1) as u32);
    Size { width, height }
}

fn native_read_count(
    source: &Plane,
    rectangle: Rect,
    province: bool,
    tile_edge: u32,
    available: usize,
) -> Option<usize> {
    let mut reads = 0;
    for tile in &source.tiles {
        let Some(part) = tile.rectangle.intersection(rectangle) else {
            continue;
        };
        if tile.texture.solid_color().is_some() {
            continue;
        }
        if !tile.texture.renderable() {
            return None;
        }
        if !province && part.width == rectangle.width {
            reads += 1;
        } else {
            let stride = part.width as usize * 4;
            let rows = (available.min(tile_edge as usize * tile_edge as usize * 4) / stride)
                .min(part.height as usize);
            if rows == 0 {
                return None;
            }
            reads += (part.height as usize).div_ceil(rows);
        }
    }
    Some(reads)
}

impl Gpu {
    pub fn patch_region(&self, image: &mut Image, rectangle: Rect, pixels: &Pixels) -> Result<()> {
        if image.size.rect().intersection(rectangle) != Some(rectangle)
            || pixels.size.width != rectangle.width
            || pixels.size.height != rectangle.height
            || pixels.main.is_none()
            || pixels.province.is_some()
        {
            return Err(Error::Message("invalid main-plane patch region"));
        }
        if rectangle == image.size.rect() {
            return self.patch_pixels(image, pixels);
        }
        self.check_image(image)?;
        let plane = image.plane(false)?;
        if plane.size == image.size
            && plane.tiles.len() == 1
            && plane.tiles[0].rectangle == image.size.rect()
            && plane.tiles[0].renderable()
            && Rc::strong_count(image.main.as_ref().unwrap()) == 1
            && Rc::strong_count(&plane.tiles[0].texture) == 1
        {
            let data = pixels.main.as_ref().unwrap().as_slice();
            if pixels.size.rgba_bytes() != Some(data.len()) {
                return Err(Error::Message("patch byte count differs from region"));
            }
            // An independent full-density tile needs no source texture or GPU
            // blit. Shared snapshots retain the copy path to avoid extra stores.
            self.writable(image, rectangle, false)?;
            let texture = &image.plane(false)?.tiles[0].texture;
            return self.device.upload_region(texture, rectangle, data);
        }
        let source = self.upload_main(pixels)?;
        self.copy_rect(
            image,
            &source,
            pixels.size.rect(),
            rectangle.left,
            rectangle.top,
            image.size.rect(),
            DrawFace::Alpha,
            false,
        )
    }
    /// Read stored alpha/province samples for hit testing. Expanding compact
    /// RGBA to the script canvas here wastes both staging and GPU work; the
    /// protocol plane maps logical coordinates to these samples on demand.
    pub fn read_hit_plane(
        &self,
        image: &Image,
        province: bool,
    ) -> Result<krkr_protocol::hit::Plane> {
        self.check_image(image)?;
        if province && !image.has_province() {
            return Ok(krkr_protocol::hit::Plane {
                size: image.size,
                data: krkr_protocol::hit::Data::Empty,
            });
        }
        let plane = image.plane(province)?;
        let channel = |color: u32| {
            if province {
                (color >> 16) as u8
            } else {
                (color >> 24) as u8
            }
        };
        let mut uniform = None;
        let constant = plane.tiles.iter().all(|tile| {
            let value = (!province)
                .then(|| tile.texture.uniform_alpha())
                .flatten()
                .or_else(|| tile.solid_region(tile.size().rect()).map(channel));
            let Some(value) = value else {
                return false;
            };
            let same = uniform.is_none_or(|old| old == value);
            uniform = Some(value);
            same
        });
        if constant && let Some(value) = uniform {
            return Ok(krkr_protocol::hit::Plane {
                size: image.size,
                data: krkr_protocol::hit::Data::Uniform(value),
            });
        }
        // Supersampled assets need only the samples addressable by script
        // coordinates. This also bounds readback staging by the logical size
        // used when the VM admits a cached mask read.
        let stored = Size {
            width: plane.size.width.min(image.size.width),
            height: plane.size.height.min(image.size.height),
        };
        let mut view = image.shared();
        view.size = stored;
        let read = self.readback(&view, stored.rect(), province)?;
        Ok(krkr_protocol::hit::Plane::from_scaled_pixels(
            image.size,
            stored,
            read.data,
            read.channels,
            &self.staging,
        )?)
    }
    /// Prepare replacement planes before publishing either one. Unspecified
    /// planes retain their exact image version, including compact main storage.
    pub fn patch_pixels(&self, image: &mut Image, pixels: &Pixels) -> Result<()> {
        if image.size != pixels.size {
            return Err(Error::Message("patch dimensions differ from image"));
        }
        let mut next = self
            .begin_upload(
                Some(image),
                pixels.size,
                pixels.main.is_some(),
                pixels.province.is_some(),
            )?
            .complete(self, pixels, None)?;
        if pixels.province.is_none() {
            next.province = image.province.clone();
        }
        next.canvas = image.canvas;
        *image = next;
        Ok(())
    }
    pub fn assign_bitmap(&self, source: Option<&Image>, pixels: &Pixels) -> Result<Image> {
        if pixels.main.is_none() || pixels.province.is_some() {
            return Err(Error::Message(
                "bitmap assignment requires only main pixels",
            ));
        }
        let mut next = self.upload_main(pixels)?;
        next.canvas = true;
        if let Some(source) = source {
            self.check_image(source)?;
            if source.size == next.size {
                next.province = source.province.clone();
            } else if source.has_province() {
                next.province = Some(self.plane(next.size, &self.resident)?);
                self.copy_rect(
                    &mut next,
                    source,
                    source.size.rect(),
                    0,
                    0,
                    pixels.size.rect(),
                    krkr_protocol::graphics::DrawFace::Province,
                    false,
                )?;
            }
        }
        Ok(next)
    }
    /// Transfer decoded bytes to texture tiles. ES2 has no UNPACK_ROW_LENGTH;
    /// staging is bounded by one tile, including expanded province bytes.
    pub fn upload(&self, image: &mut Image, pixels: &Pixels) -> Result<()> {
        self.check_image(image)?;
        if image.size != pixels.size {
            return Err(Error::Message("upload dimensions differ from image"));
        }
        let rgba = pixels
            .size
            .rgba_bytes()
            .ok_or(Error::Message("upload byte size overflow"))?;
        for (province, bytes) in [(false, &pixels.main), (true, &pixels.province)] {
            if let Some(bytes) = bytes {
                if bytes.as_slice().len() != if province { rgba / 4 } else { rgba } {
                    return Err(Error::Message("upload byte count differs from image"));
                }
                image.plane(province)?;
            }
        }
        for (province, bytes) in [(false, &pixels.main), (true, &pixels.province)] {
            let Some(bytes) = bytes else {
                continue;
            };
            let old = image.plane(province)?;
            let contiguous = !province
                && if old.size == image.size {
                    old.tiles
                        .iter()
                        .all(|tile| tile.rectangle.width == pixels.size.width)
                } else {
                    image.size.width <= self.tile_edge
                };
            // Admit CPU staging before detaching any shared tile. If the
            // scratch allocation fails, existing pixels must remain readable.
            let mut staging = if contiguous {
                None
            } else {
                let capacity = if old.size == image.size {
                    old.tiles
                        .iter()
                        .map(|tile| tile.size().rgba_bytes().unwrap())
                        .max()
                        .unwrap()
                } else {
                    Size {
                        width: image.size.width.min(self.tile_edge),
                        height: image.size.height.min(self.tile_edge),
                    }
                    .rgba_bytes()
                    .unwrap()
                };
                let stride = if old.size == image.size {
                    old.tiles
                        .iter()
                        .map(|tile| tile.size().width as usize * 4)
                        .max()
                        .unwrap()
                } else {
                    image.size.width.min(self.tile_edge) as usize * 4
                };
                let capacity = if capacity <= self.staging.available() {
                    capacity
                } else {
                    (self.staging.available().min(64 * 1024) / stride).max(1) * stride
                };
                Some(Bytes::zeroed(capacity, &self.staging)?)
            };
            self.writable_upload(image, province)?;
            let plane = image.plane(province)?;
            // Full-width RGBA tiles are already contiguous in the decoder's
            // buffer. Converted Vita images normally take this path, including
            // tall strips: no zeroed staging tile or row-by-row copy is needed.
            if contiguous {
                for tile in &plane.tiles {
                    let start = tile.rectangle.top as usize * pixels.size.width as usize * 4;
                    let end = start + tile.size().rgba_bytes().unwrap();
                    self.device
                        .upload(&tile.texture, &bytes.as_slice()[start..end])?;
                }
                continue;
            }
            let staging = staging.as_mut().expect("admitted upload staging");
            for tile in &plane.tiles {
                let width = tile.rectangle.width as usize;
                let rows =
                    (staging.as_slice().len() / (width * 4)).min(tile.rectangle.height as usize);
                for top in (0..tile.rectangle.height as usize).step_by(rows) {
                    let height = rows.min(tile.rectangle.height as usize - top);
                    let output = &mut staging.as_mut_slice()[..height * width * 4];
                    for (row, target) in output.chunks_exact_mut(width * 4).enumerate() {
                        let offset = (tile.rectangle.top as usize + top + row)
                            * pixels.size.width as usize
                            + tile.rectangle.left as usize;
                        if province {
                            for (pixel, &value) in target
                                .as_chunks_mut::<4>()
                                .0
                                .iter_mut()
                                .zip(&bytes.as_slice()[offset..offset + width])
                            {
                                pixel.copy_from_slice(&[value, 0, 0, 0]);
                            }
                        } else {
                            target.copy_from_slice(
                                &bytes.as_slice()[offset * 4..(offset + width) * 4],
                            );
                        }
                    }
                    if height == tile.rectangle.height as usize {
                        self.device.upload(&tile.texture, output)?;
                    } else {
                        self.device.upload_region(
                            &tile.texture,
                            Rect {
                                left: 0,
                                top: top as i32,
                                width: width as u32,
                                height: height as u32,
                            },
                            output,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
    pub fn upload_scaled(&self, pixels: &Pixels, logical: Size) -> Result<Image> {
        if pixels.province.is_some() || pixels.main.is_none() {
            return Err(Error::Message(
                "scaled upload must contain only RGBA pixels",
            ));
        }
        let next = self.upload_main(pixels)?;
        self.logical_image(next, logical)
    }
    pub fn upload_scaled_into(
        &self,
        image: &mut Image,
        pixels: &Pixels,
        logical: Size,
    ) -> Result<()> {
        if logical.width == 0
            || logical.height == 0
            || logical.width > i32::MAX as u32
            || logical.height > i32::MAX as u32
        {
            return Err(Error::Message("invalid logical image dimensions"));
        }
        if image.size == pixels.size
            && image.has_main()
            && !image.has_province()
            && pixels.main.is_some()
            && pixels.province.is_none()
        {
            self.upload(image, pixels)?;
            image.size = logical;
            image.canvas = true;
        } else {
            *image = self.upload_scaled(pixels, logical)?;
        }
        Ok(())
    }
    /// Explicit synchronous readback. Normal composition and presentation never
    /// call this. Compact images resolve only the requested rectangle on GPU;
    /// they do not allocate a full logical-size intermediate texture.
    pub fn readback(&self, image: &Image, rectangle: Rect, province: bool) -> Result<ReadPixels> {
        self.check_image(image)?;
        if image.size.rect().intersection(rectangle) != Some(rectangle) {
            return Err(Error::Message("pixel read is outside the image"));
        }
        let source = image.plane(province)?;
        let size = Size {
            width: rectangle.width,
            height: rectangle.height,
        };
        let channels = if province { 1 } else { 4 };
        let length = size
            .rgba_bytes()
            .ok_or(Error::Message("readback byte size overflow"))?
            / 4
            * channels;
        let mut data = Bytes::zeroed(length, &self.staging)?;
        // Crop boundaries do not change the shared texture's pixel grid.
        // A complete region on that texture can go straight into the result.
        if !province
            && source.size == image.size
            && let Some((texture, area)) = readback_view(source, rectangle)
        {
            unsafe {
                self.device.gl.bind_framebuffer(
                    glow::FRAMEBUFFER,
                    Some(texture.read_framebuffer_region(area)?),
                );
                self.device.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                self.device.gl.read_pixels(
                    area.left,
                    area.top,
                    area.width as i32,
                    area.height as i32,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelPackData::Slice(Some(data.as_mut_slice())),
                );
            }
            self.device.check()?;
            return Ok(ReadPixels {
                data,
                size,
                channels,
            });
        }
        let available = self.scratch_capacity().min(self.staging.available());
        let extent = readback_extent(size, self.tile_edge, available);
        let merged_reads = if available < 4 {
            usize::MAX
        } else {
            size.width.div_ceil(extent.width) as usize
                * size.height.div_ceil(extent.height) as usize
        };
        // Count only views intersecting this read. Constants need no GL read;
        // narrow CPU buffers can split one view into many synchronous reads.
        if source.size == image.size
            && native_read_count(
                source,
                rectangle,
                province,
                self.tile_edge,
                self.staging.available(),
            )
            .is_some_and(|reads| reads <= merged_reads)
        {
            self.readback_tiles(source, rectangle, province, data.as_mut_slice())?;
            return Ok(ReadPixels {
                data,
                size,
                channels,
            });
        }
        let sx = source.size.width as f32 / image.size.width as f32;
        let sy = source.size.height as f32 / image.size.height as f32;
        let mapping = [sx, 0., (sx - 1.) * 0.5, 0., sy, (sy - 1.) * 0.5];
        if available < 4 {
            drop(self.scratch.reserve(4)?);
            drop(self.staging.reserve(4)?);
        }
        let Size { width, height } = extent;
        let capacity = width as usize * height as usize * 4;
        let mut staging = Bytes::zeroed(capacity, &self.staging)?;
        // Keep one admitted target for every strip, including the narrower
        // final strip. Retired targets still hold their budget until collected.
        let texture = self
            .device
            .sample_texture(Size { width, height }, &self.scratch)?;
        let copy = Draw::copy(mapping, [true; 4]);
        for top in (0..size.height).step_by(height as usize) {
            for left in (0..size.width).step_by(width as usize) {
                let extent = Size {
                    width: (size.width - left).min(width),
                    height: (size.height - top).min(height),
                };
                let part = Rect {
                    left: rectangle.left + left as i32,
                    top: rectangle.top + top as i32,
                    ..extent.rect()
                };
                {
                    let target = Plane {
                        size: image.size,
                        budget: self.scratch.clone(),
                        tiles: vec![Tile {
                            backing: None,
                            rectangle: Rect {
                                width,
                                height,
                                ..part
                            },
                            texture: texture.clone(),
                        }],
                    };
                    if !crate::drawing::overwrites(&copy, Some(source), part) {
                        self.draw(
                            &target,
                            None,
                            part,
                            &Draw {
                                kind: 3.,
                                ..Draw::copy(mapping, [true; 4])
                            },
                        )?;
                    }
                    self.draw(&target, Some(source), part, &copy)?;
                }
                let output = &mut staging.as_mut_slice()[..extent.rgba_bytes().unwrap()];
                unsafe {
                    self.device.gl.bind_framebuffer(
                        glow::FRAMEBUFFER,
                        Some(texture.read_framebuffer_region(extent.rect())?),
                    );
                    self.device.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    self.device.gl.read_pixels(
                        0,
                        0,
                        extent.width as i32,
                        extent.height as i32,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(output)),
                    );
                }
                self.device.check()?;
                for (row, input) in output.chunks_exact(extent.width as usize * 4).enumerate() {
                    let offset =
                        ((top as usize + row) * size.width as usize + left as usize) * channels;
                    let target =
                        &mut data.as_mut_slice()[offset..offset + extent.width as usize * channels];
                    if province {
                        for (out, pixel) in target.iter_mut().zip(input.as_chunks::<4>().0) {
                            *out = pixel[0];
                        }
                    } else {
                        target.copy_from_slice(input);
                    }
                }
            }
        }
        Ok(ReadPixels {
            data,
            size,
            channels,
        })
    }
    /// Native pixels, including crop views and constant margins, need no
    /// temporary GPU canvas. Pack bounded rows directly into the result.
    fn readback_tiles(
        &self,
        source: &Plane,
        rectangle: Rect,
        province: bool,
        data: &mut [u8],
    ) -> Result<()> {
        let channels = if province { 1 } else { 4 };
        let put = |data: &mut [u8], row: Rect, input: &[u8]| {
            let offset = (((row.top - rectangle.top) as usize * rectangle.width as usize)
                + (row.left - rectangle.left) as usize)
                * channels;
            let output = &mut data[offset..offset + row.width as usize * channels];
            if province {
                for (out, pixel) in output.iter_mut().zip(input.as_chunks::<4>().0) {
                    *out = pixel[0];
                }
            } else {
                output.copy_from_slice(input);
            }
        };
        let mut staging = None::<Bytes>;
        for tile in &source.tiles {
            let Some(part) = tile.rectangle.intersection(rectangle) else {
                continue;
            };
            if let Some(color) = tile.texture.solid_color() {
                let pixel = [
                    (color >> 16) as u8,
                    (color >> 8) as u8,
                    color as u8,
                    (color >> 24) as u8,
                ];
                for top in part.top..part.top + part.height as i32 {
                    let offset = ((top - rectangle.top) as usize * rectangle.width as usize
                        + (part.left - rectangle.left) as usize)
                        * channels;
                    for out in data[offset..offset + part.width as usize * channels]
                        .chunks_exact_mut(channels)
                    {
                        out.copy_from_slice(&pixel[..channels]);
                    }
                }
                continue;
            }
            let backing = tile.sample_rectangle();
            if !province && part.width == rectangle.width {
                // A full output row is already contiguous in the result;
                // read it there directly without a second RGBA buffer.
                let area = Rect {
                    left: part.left - backing.left,
                    top: part.top - backing.top,
                    ..part
                };
                let offset = (part.top - rectangle.top) as usize * rectangle.width as usize * 4;
                let pixels =
                    &mut data[offset..offset + part.width as usize * part.height as usize * 4];
                unsafe {
                    self.device.gl.bind_framebuffer(
                        glow::FRAMEBUFFER,
                        Some(tile.texture.read_framebuffer_region(area)?),
                    );
                    self.device.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    self.device.gl.read_pixels(
                        area.left,
                        area.top,
                        area.width as i32,
                        area.height as i32,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(pixels)),
                    );
                }
                self.device.check()?;
                continue;
            }
            let stride = part.width as usize * 4;
            let previous = staging.as_ref().map_or(0, |buffer| buffer.as_slice().len());
            let capacity = (self.tile_edge as usize * self.tile_edge as usize * 4)
                .min(stride * part.height as usize)
                .min(self.staging.available().saturating_add(previous))
                .max(stride);
            if staging
                .as_ref()
                .is_none_or(|buffer| buffer.as_slice().len() < capacity)
            {
                drop(staging.take());
                staging = Some(Bytes::zeroed(capacity, &self.staging)?);
            }
            let buffer = staging.as_mut().unwrap();
            let rows = (buffer.as_slice().len() / stride).min(part.height as usize);
            for top in (0..part.height).step_by(rows) {
                let height = (part.height - top).min(rows as u32);
                let area = Rect {
                    left: part.left - backing.left,
                    top: part.top - backing.top + top as i32,
                    width: part.width,
                    height,
                };
                let pixels = &mut buffer.as_mut_slice()[..stride * height as usize];
                unsafe {
                    self.device.gl.bind_framebuffer(
                        glow::FRAMEBUFFER,
                        Some(tile.texture.read_framebuffer_region(area)?),
                    );
                    self.device.gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
                    self.device.gl.read_pixels(
                        area.left,
                        area.top,
                        area.width as i32,
                        area.height as i32,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelPackData::Slice(Some(pixels)),
                    );
                }
                self.device.check()?;
                for (row, input) in pixels.chunks_exact(stride).enumerate() {
                    put(
                        data,
                        Rect {
                            top: part.top + top as i32 + row as i32,
                            height: 1,
                            ..part
                        },
                        input,
                    );
                }
            }
        }
        Ok(())
    }
    pub fn pixel(&self, image: &Image, x: i32, y: i32, province: bool) -> Result<u32> {
        self.check_image(image)?;
        if let Some(value) = self.pixel_reads.borrow_mut().get(image, x, y, province) {
            return Ok(value);
        }
        if x >= 0 && y >= 0 && (x as u32) < image.size.width && (y as u32) < image.size.height {
            let area = crate::pixel_cache::area(image.size, x, y);
            match self.readback(image, area, province) {
                Ok(read) => {
                    let value = crate::pixel_cache::value(&read, area, x, y);
                    self.pixel_reads.borrow_mut().insert(
                        image,
                        area,
                        province,
                        read,
                        &self.staging,
                    );
                    return Ok(value);
                }
                // Caching is optional under a tight readback budget.
                Err(Error::Budget(_)) => self.pixel_reads.borrow_mut().clear(),
                Err(error) => return Err(error),
            }
        }
        let read = self.readback(
            image,
            Rect {
                left: x,
                top: y,
                width: 1,
                height: 1,
            },
            province,
        )?;
        let bytes = read.data.as_slice();
        if province {
            Ok(u32::from(bytes[0]))
        } else {
            Ok(u32::from_be_bytes([bytes[3], bytes[0], bytes[1], bytes[2]]))
        }
    }
}

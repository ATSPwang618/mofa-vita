use super::{Bounds, Document, Error, Result, reader::Reader};
use krkr_protocol::{
    budget::{Budget, Permit},
    pixels::{Bytes, Pixels},
};
use std::{
    collections::VecDeque,
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

#[derive(Clone, Copy, Debug)]
pub enum Image {
    Layer(usize),
    Raw(usize),
    Mask(usize),
    Merged,
}
struct Task {
    id: i16,
    bounds: Bounds,
    offset: u64,
    length: u64,
    compression: Option<u16>,
    rows: Vec<u16>,
    retain: bool,
}
struct Plane {
    id: i16,
    bounds: Bounds,
    bytes: Bytes,
    stride: usize,
}
struct Reading {
    task: Task,
    input: Box<dyn Read + Send>,
    compression: u16,
    data: Bytes,
    row: usize,
    stride: usize,
    zip_shared: bool,
    row_work: Option<Bytes>,
    _table_permit: Option<Permit>,
}
pub struct Decoder {
    doc: Arc<Document>,
    image: Image,
    budget: Budget,
    bounds: Bounds,
    tasks: VecDeque<Task>,
    active: Option<Reading>,
    planes: Vec<Plane>,
    output: Option<Pixels>,
    compose_row: usize,
    zip: Option<Box<dyn Read + Send>>,
    _tables: Vec<Permit>,
}
fn stride(bounds: Bounds, depth: u16) -> Result<usize> {
    Ok((bounds.size()?.width as usize * depth as usize).div_ceil(8))
}
impl Decoder {
    pub fn new(doc: Arc<Document>, image: Image, budget: Budget) -> Result<Self> {
        let (bounds, tasks) = match image {
            Image::Merged => {
                if !doc.has_merged() {
                    return Err(Error::Message("PSD has no stored merged image"));
                }
                (
                    Bounds {
                        top: 0,
                        left: 0,
                        right: doc.size.width as i32,
                        bottom: doc.size.height as i32,
                    },
                    VecDeque::new(),
                )
            }
            Image::Layer(index) | Image::Raw(index) | Image::Mask(index) => {
                let layer = doc
                    .layers
                    .get(index)
                    .ok_or(Error::Message("not such PSD layer"))?;
                let mask = matches!(image, Image::Mask(_));
                if layer.kind != 0 && !(mask && layer.kind == 2) {
                    return Err(Error::Message("PSD layer has no raster image"));
                }
                let (mask_bounds, _) = layer.mask_bounds();
                let bounds = if mask {
                    if mask_bounds.width() == 0 || mask_bounds.height() == 0 {
                        Bounds {
                            right: 1,
                            bottom: 1,
                            ..Bounds::default()
                        }
                    } else {
                        mask_bounds
                    }
                } else {
                    layer.bounds
                };
                let selected_mask = layer.channels.iter().rposition(|c| matches!(c.id, -2 | -3));
                let mut tasks = VecDeque::new();
                for (i, c) in layer.channels.iter().enumerate() {
                    let is_mask = Some(i) == selected_mask;
                    if (mask && !is_mask)
                        || (!mask && c.id < -1 && (!is_mask || matches!(image, Image::Raw(_))))
                    {
                        continue;
                    }
                    if !mask && c.id >= base_channels(doc.color_mode) {
                        continue;
                    }
                    let channel_bounds = if is_mask { mask_bounds } else { layer.bounds };
                    if channel_bounds.width() == 0 || channel_bounds.height() == 0 {
                        continue;
                    }
                    tasks.push_back(Task {
                        id: if is_mask { -2 } else { c.id },
                        bounds: channel_bounds,
                        offset: c.offset,
                        length: u64::from(c.length),
                        compression: None,
                        rows: Vec::new(),
                        retain: true,
                    });
                }
                (bounds, tasks)
            }
        };
        if !matches!(doc.color_mode, 0..=4) && !matches!(image, Image::Mask(_)) {
            return Err(Error::Message(
                "PSD multichannel, duotone and Lab raster conversion is not supported",
            ));
        }
        if doc.depth == 1 && doc.color_mode != 0 {
            return Err(Error::Message("PSD 1-bit data requires bitmap mode"));
        }
        let size = bounds.size()?;
        let bytes = size
            .rgba_bytes()
            .ok_or(Error::Message("PSD output size overflow"))?;
        let output = Pixels {
            size,
            main: Some(Bytes::zeroed(bytes, &budget)?),
            province: None,
        };
        let mut result = Self {
            doc,
            image,
            budget,
            bounds,
            tasks,
            active: None,
            planes: Vec::new(),
            output: Some(output),
            compose_row: 0,
            zip: None,
            _tables: Vec::new(),
        };
        if matches!(image, Image::Merged) {
            result.merged_tasks()?;
        }
        if !matches!(image, Image::Mask(_))
            && (0..base_channels(result.doc.color_mode))
                .any(|id| !result.tasks.iter().any(|task| task.id == id))
        {
            return Err(Error::Message(
                "PSD image is missing a required color channel",
            ));
        }
        Ok(result)
    }
    fn merged_tasks(&mut self) -> Result<()> {
        let mut r = Reader::new(self.doc.plan.open()?, self.doc.plan.bytes);
        r.seek(self.doc.merged)?;
        let compression = r.u16()?;
        if compression > 3 {
            return Err(Error::Message("unsupported PSD compression"));
        }
        let rows = self.doc.size.height as usize;
        let bytes = stride(self.bounds, self.doc.depth)? * rows;
        let mut offset = r.pos;
        if compression == 1 {
            let count = rows * self.doc.channels as usize;
            self._tables.push(self.budget.reserve(count * 2)?);
            offset = offset
                .checked_add(count as u64 * 2)
                .filter(|&end| end <= self.doc.plan.bytes)
                .ok_or(Error::Message("truncated PSD RLE row lengths"))?;
        }
        for ch in 0..self.doc.channels as usize {
            let row_counts = if compression == 1 {
                let mut counts = Vec::with_capacity(rows);
                for _ in 0..rows {
                    counts.push(r.u16()?);
                }
                counts
            } else {
                Vec::new()
            };
            let length = if compression == 1 {
                row_counts.iter().map(|&n| u64::from(n)).sum()
            } else {
                bytes as u64
            };
            let id = if self.doc.color_mode == 3 && ch == 3 {
                -1
            } else {
                ch as i16
            };
            self.tasks.push_back(Task {
                id,
                bounds: self.bounds,
                offset,
                length,
                compression: Some(compression),
                rows: row_counts,
                retain: id < base_channels(self.doc.color_mode),
            });
            if compression < 2 {
                offset = offset
                    .checked_add(length)
                    .ok_or(Error::Message("PSD merged offset overflow"))?;
                if offset > self.doc.plan.bytes {
                    return Err(Error::Message("truncated PSD merged channels"));
                }
            }
        }
        if compression >= 2 {
            let remaining = r.remaining();
            self.zip = Some(Box::new(flate2::read::ZlibDecoder::new(
                r.input.take(remaining),
            )));
        }
        Ok(())
    }
    fn start(&mut self, mut task: Task) -> Result<Reading> {
        let stride = stride(task.bounds, self.doc.depth)?;
        let height = task.bounds.size()?.height as usize;
        let shared = task.compression.is_some_and(|c| c >= 2);
        let mut permit = None;
        let (input, compression): (Box<dyn Read + Send>, _) = if shared {
            (
                self.zip
                    .take()
                    .ok_or(Error::Message("missing PSD merged ZIP decoder"))?,
                task.compression.unwrap(),
            )
        } else {
            let mut r = Reader::new(self.doc.plan.open()?, self.doc.plan.bytes);
            r.seek(task.offset)?;
            r.end = task
                .offset
                .checked_add(task.length)
                .filter(|&n| n <= r.end)
                .ok_or(Error::Message("PSD channel range exceeds source"))?;
            let compression = match task.compression {
                Some(c) => c,
                None => r.u16()?,
            };
            if compression > 3 {
                return Err(Error::Message("unsupported PSD channel compression"));
            }
            if compression == 1 && task.compression.is_none() {
                permit = Some(self.budget.reserve(height * 2)?);
                for _ in 0..height {
                    task.rows.push(r.u16()?);
                }
            }
            let len = r.remaining();
            let stream = r.input.take(len);
            if compression >= 2 {
                (
                    Box::new(flate2::read::ZlibDecoder::new(stream)),
                    compression,
                )
            } else {
                (Box::new(stream), compression)
            }
        };
        let data = Bytes::zeroed(stride * if task.retain { height } else { 1 }, &self.budget)?;
        let work_len = if compression == 1 {
            task.rows.iter().copied().max().unwrap_or(0) as usize
        } else if compression == 3 && self.doc.depth == 32 {
            stride
        } else {
            0
        };
        let row_work = (work_len != 0)
            .then(|| Bytes::zeroed(work_len, &self.budget))
            .transpose()?;
        Ok(Reading {
            task,
            input,
            compression,
            data,
            row: 0,
            stride,
            zip_shared: shared,
            row_work,
            _table_permit: permit,
        })
    }
    /// One scanline per advance, including ZIP prediction and color conversion.
    pub fn advance(&mut self) -> Result<bool> {
        if self.active.is_none()
            && let Some(task) = self.tasks.pop_front()
        {
            self.active = Some(self.start(task)?);
        }
        if let Some(active) = &mut self.active {
            let offset = if active.task.retain {
                active.row * active.stride
            } else {
                0
            };
            let row = &mut active.data.as_mut_slice()[offset..offset + active.stride];
            if active.compression == 1 {
                let count = *active
                    .task
                    .rows
                    .get(active.row)
                    .ok_or(Error::Message("missing PSD RLE row length"))?
                    as usize;
                let encoded = &mut active
                    .row_work
                    .as_mut()
                    .map_or(&mut [][..], Bytes::as_mut_slice)[..count];
                active.input.read_exact(encoded)?;
                unpack(encoded, row)?;
            } else {
                active.input.read_exact(row)?;
            }
            if active.compression == 3 {
                predict(
                    row,
                    self.doc.depth,
                    active
                        .row_work
                        .as_mut()
                        .map_or(&mut [][..], Bytes::as_mut_slice),
                )?;
            }
            active.row += 1;
            if active.row == active.task.bounds.size()?.height as usize {
                let mut done = self.active.take().unwrap();
                if done.compression >= 2
                    && (!done.zip_shared || self.tasks.is_empty())
                    && done.input.read(&mut [0])? != 0
                {
                    return Err(Error::Message("PSD ZIP expands past expected channel data"));
                }
                if done.zip_shared {
                    self.zip = Some(done.input);
                }
                if done.task.retain {
                    self.planes.push(Plane {
                        id: done.task.id,
                        bounds: done.task.bounds,
                        bytes: done.data,
                        stride: done.stride,
                    });
                }
            }
            return Ok(false);
        }
        if self.compose_row == self.bounds.size()?.height as usize {
            return Ok(true);
        }
        let width = self.bounds.size()?.width as usize;
        let y = self.compose_row;
        let output = self
            .output
            .as_mut()
            .unwrap()
            .main
            .as_mut()
            .unwrap()
            .as_mut_slice();
        let mask_only = matches!(self.image, Image::Mask(_));
        let alpha = self.planes.iter().find(|p| p.id == -1);
        let mask = self.planes.iter().find(|p| p.id == -2);
        let channels: [Option<&Plane>; 4] =
            std::array::from_fn(|id| self.planes.iter().find(|p| p.id == id as i16));
        if !mask_only
            && self.doc.color_mode == 3
            && self.doc.depth == 8
            && (alpha.is_none() || mask.is_none())
        {
            let row = |plane| plane_row(plane, y, width);
            let red = row(channels[0].expect("validated red channel"));
            let green = row(channels[1].expect("validated green channel"));
            let blue = row(channels[2].expect("validated blue channel"));
            let alpha = alpha.map(row);
            for (x, pixel) in output[y * width * 4..(y + 1) * width * 4]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                *pixel = [red[x], green[x], blue[x], alpha.map_or(255, |row| row[x])];
            }
            self.compose_row += 1;
            return Ok(false);
        }
        let mask_row = mask.map(|mask| {
            (
                i64::from(self.bounds.left) - i64::from(mask.bounds.left),
                y as i64 + i64::from(self.bounds.top) - i64::from(mask.bounds.top),
                mask.bounds.width(),
                mask.bounds.height(),
            )
        });
        let default_mask = match self.image {
            Image::Layer(i) | Image::Mask(i) => self.doc.layers[i].mask_bounds().1,
            _ => 255,
        };
        for x in 0..width {
            let pixel = &mut output[(y * width + x) * 4..(y * width + x + 1) * 4];
            if mask_only {
                let value = mask.map_or(default_mask, |p| sample8(p, x, y, self.doc.depth));
                pixel.copy_from_slice(&[value, value, value, 255]);
                continue;
            }
            let sample = |id: usize| channels[id].map_or(0, |p| sample8(p, x, y, self.doc.depth));
            let mut a = alpha.map_or(255, |p| sample8(p, x, y, self.doc.depth));
            match self.doc.color_mode {
                0 | 1 => {
                    let c = sample(0);
                    pixel.copy_from_slice(&[c, c, c, a]);
                }
                2 => {
                    let index = sample(0);
                    pixel.copy_from_slice(&self.doc.palette[index as usize]);
                    a = pixel[3];
                }
                3 => pixel.copy_from_slice(&[sample(0), sample(1), sample(2), a]),
                4 => {
                    if self.doc.depth == 32 {
                        let f = |id: usize| channels[id].map_or(0.0, |p| sample_float(p, x, y, 32));
                        let k = 1.0 - f(3);
                        for (i, c) in pixel[..3].iter_mut().enumerate() {
                            *c = ((1.0 - f(i)) * k * 255.0) as u8;
                        }
                        pixel[3] = a;
                    } else {
                        let k = u16::from(sample(3));
                        pixel.copy_from_slice(&[
                            ((u16::from(sample(0)) * k) >> 8) as u8,
                            ((u16::from(sample(1)) * k) >> 8) as u8,
                            ((u16::from(sample(2)) * k) >> 8) as u8,
                            a,
                        ]);
                    }
                }
                _ => unreachable!(),
            }
            // Match the plugin's stored-channel semantics: masks are folded
            // into an explicit transparency channel, not layer opacity.
            if let (Some(alpha), Some(mask)) = (alpha, mask) {
                let (left, my, width, height) = mask_row.unwrap();
                let mx = x as i64 + left;
                a = if mx >= 0 && my >= 0 && mx < width && my < height {
                    if self.doc.depth == 8 {
                        (f32::from(a)
                            * f32::from(sample8(mask, mx as usize, my as usize, 8))
                            * (1.0 / 255.0)) as u8
                    } else {
                        (sample_float(alpha, x, y, self.doc.depth)
                            * sample_float(mask, mx as usize, my as usize, self.doc.depth)
                            * 255.0) as u8
                    }
                } else if default_mask == 0 {
                    0
                } else {
                    a
                };
                pixel[3] = a;
            }
        }
        self.compose_row += 1;
        Ok(false)
    }
    pub fn finish(mut self) -> Pixels {
        self.output.take().expect("PSD output")
    }
}
fn plane_row(plane: &Plane, y: usize, width: usize) -> &[u8] {
    &plane.bytes.as_slice()[y * plane.stride..][..width]
}
fn base_channels(mode: u16) -> i16 {
    match mode {
        3 => 3,
        4 => 4,
        _ => 1,
    }
}
fn sample8(p: &Plane, x: usize, y: usize, depth: u16) -> u8 {
    let data = p.bytes.as_slice();
    match depth {
        1 => {
            if data[y * p.stride + x / 8] & (128 >> (x % 8)) == 0 {
                255
            } else {
                0
            }
        }
        8 => data[y * p.stride + x],
        16 => data[y * p.stride + x * 2],
        32 => (sample_float(p, x, y, depth) * 255.0) as u8,
        _ => unreachable!(),
    }
}
fn sample_float(p: &Plane, x: usize, y: usize, depth: u16) -> f32 {
    let data = p.bytes.as_slice();
    let at = y * p.stride + x * (depth as usize / 8);
    match depth {
        16 => f32::from(u16::from_be_bytes([data[at], data[at + 1]])) / 65535.0,
        32 => f32::from_be_bytes(data[at..at + 4].try_into().unwrap()).clamp(0.0, 1.0),
        _ => f32::from(sample8(p, x, y, depth)) / 255.0,
    }
}
fn unpack(input: &[u8], out: &mut [u8]) -> Result<()> {
    let mut from = 0;
    let mut to = 0;
    while from < input.len() {
        let control = input[from];
        from += 1;
        match control {
            0..=127 => {
                let count = control as usize + 1;
                if from + count > input.len() || to + count > out.len() {
                    return Err(Error::Message("invalid PSD PackBits literal"));
                }
                out[to..to + count].copy_from_slice(&input[from..from + count]);
                from += count;
                to += count;
            }
            128 => {}
            _ => {
                let count = 257 - control as usize;
                if from == input.len() || to + count > out.len() {
                    return Err(Error::Message("invalid PSD PackBits repeat"));
                }
                out[to..to + count].fill(input[from]);
                from += 1;
                to += count;
            }
        }
    }
    if to != out.len() {
        return Err(Error::Message("short PSD PackBits scanline"));
    }
    Ok(())
}
fn predict(row: &mut [u8], depth: u16, work: &mut [u8]) -> Result<()> {
    match depth {
        8 => {
            for i in 1..row.len() {
                row[i] = row[i].wrapping_add(row[i - 1]);
            }
        }
        16 => {
            let mut previous = 0u16;
            for p in row.as_chunks_mut::<2>().0.iter_mut() {
                previous = previous.wrapping_add(u16::from_be_bytes([p[0], p[1]]));
                p.copy_from_slice(&previous.to_be_bytes());
            }
        }
        32 => {
            for i in 1..row.len() {
                row[i] = row[i].wrapping_add(row[i - 1]);
            }
            let planar = &mut work[..row.len()];
            planar.copy_from_slice(row);
            let width = row.len() / 4;
            for x in 0..width {
                for c in 0..4 {
                    row[x * 4 + c] = planar[c * width + x];
                }
            }
        }
        _ => return Err(Error::Message("PSD prediction requires 8, 16 or 32 bits")),
    }
    Ok(())
}

/// A virtual BMP stream holds the same budget permits as a direct layer decode.
/// Its header and bottom-up BGRA view are synthesized without another image copy.
pub struct BmpStream {
    pixels: Pixels,
    header: [u8; 54],
    position: u64,
}
impl BmpStream {
    pub fn new(pixels: Pixels) -> Result<Self> {
        let len = pixels
            .size
            .rgba_bytes()
            .ok_or(Error::Message("PSD BMP size overflow"))?;
        let size =
            u32::try_from(len + 54).map_err(|_| Error::Message("PSD BMP exceeds format size"))?;
        let mut header = [0; 54];
        header[..2].copy_from_slice(b"BM");
        header[2..6].copy_from_slice(&size.to_le_bytes());
        header[10..14].copy_from_slice(&54u32.to_le_bytes());
        header[14..18].copy_from_slice(&40u32.to_le_bytes());
        header[18..22].copy_from_slice(&pixels.size.width.to_le_bytes());
        header[22..26].copy_from_slice(&pixels.size.height.to_le_bytes());
        header[26..28].copy_from_slice(&1u16.to_le_bytes());
        header[28..30].copy_from_slice(&32u16.to_le_bytes());
        header[34..38].copy_from_slice(&(len as u32).to_le_bytes());
        Ok(Self {
            pixels,
            header,
            position: 0,
        })
    }
}
impl Read for BmpStream {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let data = self.pixels.main.as_ref().unwrap().as_slice();
        let count = (data.len() as u64 + 54)
            .saturating_sub(self.position)
            .min(out.len() as u64) as usize;
        let stride = self.pixels.size.width as usize * 4;
        for byte in &mut out[..count] {
            let pos = self.position as usize;
            *byte = if pos < 54 {
                self.header[pos]
            } else {
                let at = pos - 54;
                let y = self.pixels.size.height as usize - 1 - at / stride;
                let x = at % stride;
                let channel = match x % 4 {
                    0 => 2,
                    2 => 0,
                    c => c,
                };
                data[y * stride + (x / 4) * 4 + channel]
            };
            self.position += 1;
        }
        Ok(count)
    }
}
impl Seek for BmpStream {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let pos = match from {
            SeekFrom::Start(n) => i128::from(n),
            SeekFrom::End(n) => self.pixels.size.rgba_bytes().unwrap() as i128 + 54 + i128::from(n),
            SeekFrom::Current(n) => i128::from(self.position) + i128::from(n),
        };
        self.position = u64::try_from(pos).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid PSD BMP seek")
        })?;
        Ok(self.position)
    }
}

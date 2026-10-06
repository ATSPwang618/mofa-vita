//! Bounded ICO and PE32/PE32+ RT_GROUP_ICON extraction. Never loads executable
//! code. Random reads avoid retaining an EXE's appended game archive.
//! Layout: https://learn.microsoft.com/en-us/windows/win32/debug/pe-format
use super::*;
use image::ImageDecoder;
use krkr_protocol::window::IconImage;
use std::io::{Seek, SeekFrom};

const MAX_FRAMES: usize = 1024;
const MAX_ENTRIES: usize = 4096;
const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
fn invalid() -> Error {
    Error::Message("invalid or unsupported ICO/PE icon resource")
}
fn u16le(bytes: &[u8], at: usize) -> Result<u16> {
    let data = bytes
        .get(at..at.checked_add(2).ok_or_else(invalid)?)
        .ok_or_else(invalid)?;
    Ok(u16::from_le_bytes([data[0], data[1]]))
}
fn u32le(bytes: &[u8], at: usize) -> Result<u32> {
    let data = bytes
        .get(at..at.checked_add(4).ok_or_else(invalid)?)
        .ok_or_else(invalid)?;
    Ok(u32::from_le_bytes([data[0], data[1], data[2], data[3]]))
}
fn extent(value: u8) -> u32 {
    if value == 0 { 256 } else { u32::from(value) }
}
struct Reader<'a, R> {
    stream: R,
    length: u64,
    budget: &'a Budget,
    cancelled: &'a AtomicBool,
}
impl<R: Read + Seek> Reader<'_, R> {
    fn read_into(&mut self, offset: u64, bytes: &mut [u8]) -> Result<()> {
        if offset
            .checked_add(bytes.len() as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err(invalid());
        }
        check(self.cancelled)?;
        self.stream.seek(SeekFrom::Start(offset))?;
        for chunk in bytes.chunks_mut(64 * 1024) {
            check(self.cancelled)?;
            self.stream.read_exact(chunk)?;
        }
        Ok(())
    }
    fn bytes(&mut self, offset: u64, length: usize) -> Result<Bytes> {
        let mut bytes = Bytes::zeroed(length, self.budget)?;
        self.read_into(offset, bytes.as_mut_slice())?;
        Ok(bytes)
    }
    fn header<const N: usize>(&mut self, offset: u64) -> Result<[u8; N]> {
        let mut bytes = [0; N];
        self.read_into(offset, &mut bytes)?;
        Ok(bytes)
    }
}

/// The caller supplies a VFS plan and the shared decode/native icon budget.
pub fn read(plan: ReadPlan, budget: Budget, cancelled: &AtomicBool) -> Result<IconImage> {
    let length = plan.bytes;
    decode_stream(plan.open()?, length, &budget, cancelled)
}
pub fn decode(bytes: &[u8], budget: &Budget, cancelled: &AtomicBool) -> Result<IconImage> {
    decode_stream(
        std::io::Cursor::new(bytes),
        bytes.len() as u64,
        budget,
        cancelled,
    )
}
fn decode_stream<R: Read + Seek>(
    stream: R,
    length: u64,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<IconImage> {
    let mut reader = Reader {
        stream,
        length,
        budget,
        cancelled,
    };
    let header = reader.header::<6>(0)?;
    let (entry, offset, length) = if header[..2] == *b"MZ" {
        pe_frame(&mut reader)?
    } else {
        if u16le(&header, 0)? != 0 || u16le(&header, 2)? != 1 {
            return Err(invalid());
        }
        let count = usize::from(u16le(&header, 4)?);
        if count == 0 || count > MAX_FRAMES {
            return Err(invalid());
        }
        let directory = reader.bytes(6, count * 16)?;
        let frame = best(directory.as_slice(), 16)?;
        let mut entry = [0; 16];
        entry.copy_from_slice(frame);
        let offset = u64::from(u32le(frame, 12)?);
        if offset < 6 + (count * 16) as u64 {
            return Err(invalid());
        }
        (entry, offset, u32le(frame, 8)? as usize)
    };
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(invalid());
    }
    let width = extent(entry[0]);
    let height = extent(entry[1]);
    let mut ico = Bytes::zeroed(22 + length, budget)?;
    let data = ico.as_mut_slice();
    data[..6].copy_from_slice(&[0, 0, 1, 0, 1, 0]);
    data[6..22].copy_from_slice(&entry);
    data[18..22].copy_from_slice(&22u32.to_le_bytes());
    reader.read_into(offset, &mut data[22..])?;
    // Bound PNG/DIB headers before the ecosystem decoder constructs buffers.
    let payload = &data[22..];
    let png = payload.starts_with(b"\x89PNG\r\n\x1a\n");
    if png {
        let ihdr = payload.get(8..24).ok_or_else(invalid)?;
        if ihdr[..8] != [0, 0, 0, 13, b'I', b'H', b'D', b'R']
            || u32::from_be_bytes(ihdr[8..12].try_into().map_err(|_| invalid())?) != width
            || u32::from_be_bytes(ihdr[12..16].try_into().map_err(|_| invalid())?) != height
        {
            return Err(invalid());
        }
    } else {
        let header_size = u32le(payload, 0)?;
        let dimensions = if header_size == 12 {
            (u32::from(u16le(payload, 4)?), u32::from(u16le(payload, 6)?))
        } else if matches!(header_size, 40 | 52 | 56 | 108 | 124) {
            (u32le(payload, 4)?, u32le(payload, 8)?)
        } else {
            return Err(invalid());
        };
        if dimensions != (width, height * 2) {
            return Err(invalid());
        }
    }
    // Covers decoder scanlines, palettes and compressed working buffers in
    // addition to encoded bytes and the final RGBA allocation below.
    let scratch_bytes = MAX_FRAME_BYTES;
    let _scratch = budget.reserve(scratch_bytes)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(256);
    limits.max_image_height = Some(256);
    limits.max_alloc = Some(scratch_bytes as u64);
    let mut rgba = Bytes::zeroed((width * height * 4) as usize, budget)?;
    check(cancelled)?;
    if png {
        // IcoDecoder::set_limits does not forward limits to its internal PNG
        // decoder. Construct PNG with limits before it parses compressed
        // metadata, so a tiny icon cannot expand metadata without admission.
        let decoder =
            image::codecs::png::PngDecoder::with_limits(std::io::Cursor::new(payload), limits)
                .map_err(|e| Error::Codec(e.to_string()))?;
        pixels(decoder, width, height, &mut rgba)?;
    } else {
        let decoder = image::codecs::ico::IcoDecoder::new(std::io::Cursor::new(ico.as_slice()))
            .map_err(|e| Error::Codec(e.to_string()))?;
        pixels(decoder, width, height, &mut rgba)?;
    }
    check(cancelled)?;
    Ok(IconImage {
        width,
        height,
        rgba,
    })
}

fn pixels(decoder: impl ImageDecoder, width: u32, height: u32, rgba: &mut Bytes) -> Result<()> {
    if decoder.dimensions() != (width, height) || decoder.color_type() != image::ColorType::Rgba8 {
        return Err(invalid());
    }
    decoder
        .read_image(rgba.as_mut_slice())
        .map_err(|e| Error::Codec(e.to_string()))
}

fn best(directory: &[u8], stride: usize) -> Result<&[u8]> {
    directory
        .chunks_exact(stride)
        .min_by_key(|entry| {
            (
                extent(entry[0]).abs_diff(32) + extent(entry[1]).abs_diff(32),
                std::cmp::Reverse(u16::from_le_bytes([entry[6], entry[7]])),
            )
        })
        .ok_or_else(invalid)
}

struct Pe {
    sections: Bytes,
    resources: u32,
    resource_bytes: u32,
}
impl Pe {
    fn file_offset(&self, rva: u32, length: usize) -> Result<u64> {
        let mut result = None;
        for section in self.sections.as_slice().as_chunks::<40>().0.iter() {
            let base = u32le(section, 12)?;
            let raw_size = u32le(section, 16)?;
            if let Some(relative) = rva.checked_sub(base)
                && u64::from(relative) + length as u64 <= u64::from(raw_size)
            {
                if result.is_some() {
                    return Err(invalid());
                }
                result = Some(u64::from(u32le(section, 20)?) + u64::from(relative));
            }
        }
        result.ok_or_else(invalid)
    }
    fn resource_offset(&self, relative: u32, length: usize) -> Result<u64> {
        if u64::from(relative) + length as u64 > u64::from(self.resource_bytes) {
            return Err(invalid());
        }
        self.file_offset(
            self.resources.checked_add(relative).ok_or_else(invalid)?,
            length,
        )
    }
    /// Exactly three resource directory levels are traversed; no recursion,
    /// user-selected path or cycle can cause unbounded work.
    fn entry<R: Read + Seek>(
        &self,
        reader: &mut Reader<'_, R>,
        directory: u32,
        id: Option<u32>,
        subdirectory: bool,
    ) -> Result<(u32, u32)> {
        let header = reader.header::<16>(self.resource_offset(directory, 16)?)?;
        let count = usize::from(u16le(&header, 12)?) + usize::from(u16le(&header, 14)?);
        if count == 0 || count > MAX_ENTRIES {
            return Err(invalid());
        }
        let start = directory.checked_add(16).ok_or_else(invalid)?;
        let entries = reader.bytes(self.resource_offset(start, count * 8)?, count * 8)?;
        for entry in entries.as_slice().as_chunks::<8>().0.iter() {
            let name = u32le(entry, 0)?;
            if id.is_some_and(|id| name != id) {
                continue;
            }
            let target = u32le(entry, 4)?;
            if (target & 0x8000_0000 != 0) != subdirectory {
                return Err(invalid());
            }
            return Ok((name, target & 0x7fff_ffff));
        }
        Err(Error::Message("icon resource not found"))
    }
    fn data<R: Read + Seek>(&self, reader: &mut Reader<'_, R>, entry: u32) -> Result<(u64, usize)> {
        let data = reader.header::<16>(self.resource_offset(entry, 16)?)?;
        let length = u32le(&data, 4)? as usize;
        if length == 0 || length > MAX_FRAME_BYTES {
            return Err(invalid());
        }
        Ok((self.file_offset(u32le(&data, 0)?, length)?, length))
    }
}
fn pe_frame<R: Read + Seek>(reader: &mut Reader<'_, R>) -> Result<([u8; 16], u64, usize)> {
    let dos = reader.header::<64>(0)?;
    let pe_offset = u64::from(u32le(&dos, 60)?);
    if pe_offset < 64 {
        return Err(invalid());
    }
    let coff = reader.header::<24>(pe_offset)?;
    if coff[..4] != *b"PE\0\0" {
        return Err(invalid());
    }
    let section_count = usize::from(u16le(&coff, 6)?);
    let optional_bytes = usize::from(u16le(&coff, 20)?);
    if section_count == 0 || section_count > 96 || optional_bytes > 4096 {
        return Err(invalid());
    }
    let optional = reader.bytes(pe_offset + 24, optional_bytes)?;
    let optional = optional.as_slice();
    let directories = match u16le(optional, 0)? {
        0x10b => 96,
        0x20b => 112,
        _ => return Err(invalid()),
    };
    if u32le(optional, directories - 4)? < 3 {
        return Err(invalid());
    }
    let pe = Pe {
        sections: reader.bytes(pe_offset + 24 + optional_bytes as u64, section_count * 40)?,
        resources: u32le(optional, directories + 16)?,
        resource_bytes: u32le(optional, directories + 20)?,
    };
    if pe.resources == 0 {
        return Err(Error::Message("icon resource not found"));
    }
    let (_, groups) = pe.entry(reader, 0, Some(14), true)?;
    let (_, group) = pe.entry(reader, groups, None, true)?;
    let (language, group_data) = pe.entry(reader, group, None, false)?;
    let (group_offset, group_length) = pe.data(reader, group_data)?;
    if group_length > 6 + MAX_FRAMES * 14 {
        return Err(invalid());
    }
    let group_bytes = reader.bytes(group_offset, group_length)?;
    let group_bytes = group_bytes.as_slice();
    if u16le(group_bytes, 0)? != 0 || u16le(group_bytes, 2)? != 1 {
        return Err(invalid());
    }
    let count = usize::from(u16le(group_bytes, 4)?);
    if count == 0 || count > MAX_FRAMES {
        return Err(invalid());
    }
    let directory = group_bytes.get(6..6 + count * 14).ok_or_else(invalid)?;
    let selected = best(directory, 14)?;
    let (_, icons) = pe.entry(reader, 0, Some(3), true)?;
    let (_, icon) = pe.entry(reader, icons, Some(u32::from(u16le(selected, 12)?)), true)?;
    // Prefer the group's language, then the first available language. Resource
    // names/groups preserve directory order, matching ExtractIcon index zero.
    let (_, icon_data) = pe
        .entry(reader, icon, Some(language), false)
        .or_else(|_| pe.entry(reader, icon, None, false))?;
    let (offset, length) = pe.data(reader, icon_data)?;
    if length != u32le(selected, 8)? as usize {
        return Err(invalid());
    }
    let mut entry = [0; 16];
    entry[..12].copy_from_slice(&selected[..12]);
    Ok((entry, offset, length))
}

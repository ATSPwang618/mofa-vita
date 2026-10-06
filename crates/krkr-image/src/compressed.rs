//! KTX1 ETC1, PVRTC1 and BC1/BC3 assets, including explicit native Vita layout. Blocks remain compressed until a CPU
//! pixel operation or a backend without native support requests their pixels.
//! https://registry.khronos.org/KTX/specs/1.0/ktxspec.v1.html
//! https://registry.khronos.org/OpenGL/extensions/OES/OES_compressed_ETC1_RGB8_texture.txt
use super::*;
use krkr_protocol::texture::{Compressed, Format};
mod bc;
pub(crate) mod metadata;
mod pvrtc;

pub const MAGIC: &[u8; 12] = b"\xabKTX 11\xbb\r\n\x1a\n";
const ETC1: u32 = 0x8d64;
pub(crate) const MODIFIERS: [[i16; 4]; 8] = [
    [2, 8, -2, -8],
    [5, 17, -5, -17],
    [9, 29, -9, -29],
    [13, 42, -13, -42],
    [18, 60, -18, -60],
    [24, 80, -24, -80],
    [33, 106, -33, -106],
    [47, 183, -47, -183],
];
pub(crate) struct Header {
    pub size: Size,
    pub tile_size: Size,
    pub offset: usize,
    pub format: Format,
    pub tags: Tags,
}
pub(crate) fn probe(data: &[u8]) -> Result<Header> {
    if data.len() < 68 || !data.starts_with(MAGIC) {
        return Err(Error::Message("truncated KTX header"));
    }
    let big = match &data[12..16] {
        [1, 2, 3, 4] => false,
        [4, 3, 2, 1] => true,
        _ => return Err(Error::Message("invalid KTX endianness")),
    };
    let word = |at: usize| -> Result<u32> {
        let bytes = data
            .get(at..at.saturating_add(4))
            .ok_or(Error::Message("truncated KTX field"))?;
        let bytes = bytes.try_into().unwrap();
        Ok(if big {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let format = match (word(28)?, word(32)?) {
        (ETC1, 0x1907) => Format::Etc1,
        (0x8c02, 0x1908) => Format::Pvrtc1Rgba4,
        (0x83f0, 0x1907) => Format::Bc1Rgb,
        (0x83f3, 0x1908) => Format::Bc3Rgba,
        (0x6000_0001, 0x1907) => Format::Bc1RgbVita,
        (0x6000_0002, 0x1908) => Format::Bc3RgbaVita,
        _ => return Err(Error::Message("unsupported KTX compressed format")),
    };
    if word(16)? != 0
        || word(20)? != 1
        || word(24)? != 0
        || word(44)? != 0
        || word(52)? != 1
        || word(56)? != 1
    {
        return Err(Error::Message("KTX requires one compressed 2D mip level"));
    }
    let tile_size = image_size(word(36)?, word(40)?)?;
    let arrays = word(48)?;
    let metadata = word(60)? as usize;
    if metadata > metadata::LIMIT || !metadata.is_multiple_of(4) {
        return Err(Error::Message("invalid KTX metadata size"));
    }
    let mut at = 64;
    let end = at + metadata;
    let mut canvas = None;
    let mut tags = None;
    while at < end {
        let length = word(at)? as usize;
        at += 4;
        let next = at
            .checked_add(length)
            .ok_or(Error::Message("KTX metadata overflow"))?;
        let entry = data
            .get(at..next)
            .filter(|_| next <= end)
            .ok_or(Error::Message("invalid KTX metadata"))?;
        let key_end = entry
            .iter()
            .position(|&b| b == 0)
            .ok_or(Error::Message("unterminated KTX key"))?;
        let value = &entry[key_end + 1..];
        match &entry[..key_end] {
            b"KTXorientation"
                if !matches!(
                    value,
                    b"S=r,T=d" | b"S=r,T=d\0" | b"S=r,T=d,R=i" | b"S=r,T=d,R=i\0"
                ) =>
            {
                return Err(Error::Message("KTX image requires S=r,T=d orientation"));
            }
            metadata::CANVAS => {
                if canvas.is_some() || value.len() != 8 {
                    return Err(Error::Message("invalid or duplicate KTX canvas"));
                }
                canvas = Some(image_size(
                    u32::from_le_bytes(value[..4].try_into().unwrap()),
                    u32::from_le_bytes(value[4..].try_into().unwrap()),
                )?);
            }
            metadata::TAGS => {
                if tags.is_some() {
                    return Err(Error::Message("duplicate texture tags"));
                }
                tags = Some(metadata::decode(value)?);
            }
            _ => {}
        }
        at = next
            .checked_add((4 - length % 4) % 4)
            .ok_or(Error::Message("KTX metadata overflow"))?;
        if at > end {
            return Err(Error::Message("invalid KTX metadata padding"));
        }
    }
    let size = canvas.unwrap_or(tile_size);
    let bytes = Compressed::payload_len(size, tile_size, format)
        .ok_or(Error::Message("KTX size overflow"))?;
    let count = bytes / format.byte_len(tile_size).unwrap();
    if (arrays == 0 && canvas.is_some())
        || (arrays != 0 && (canvas.is_none() || arrays as usize != count))
    {
        return Err(Error::Message("KTX array requires a matching tiled canvas"));
    }
    if word(end)? as usize != bytes || end.checked_add(4 + bytes) != Some(data.len()) {
        return Err(Error::Message("KTX payload size mismatch"));
    }
    // Differential base overflow is undefined on the GPU. Reject it before
    // handing untrusted archive bytes to either decoder.
    if format == Format::Etc1 {
        for block in data[end + 4..].as_chunks::<8>().0.iter() {
            bases(block)?;
        }
    }
    Ok(Header {
        size,
        tile_size,
        offset: end + 4,
        format,
        tags: tags.unwrap_or_default(),
    })
}
fn bases(block: &[u8]) -> Result<[[i16; 3]; 2]> {
    let mut colors = [[0; 3]; 2];
    for c in 0..3 {
        let byte = block[c];
        if block[3] & 2 == 0 {
            colors[0][c] = i16::from(byte >> 4) * 17;
            colors[1][c] = i16::from(byte & 15) * 17;
        } else {
            let a = i16::from(byte >> 3);
            let b = a + ((byte as i8) << 5 >> 5) as i16;
            if !(0..32).contains(&b) {
                return Err(Error::Message("invalid ETC1 differential block"));
            }
            colors[0][c] = (a << 3) | (a >> 2);
            colors[1][c] = (b << 3) | (b >> 2);
        }
    }
    Ok(colors)
}
pub fn decode(texture: &Compressed, budget: &Budget, cancelled: &AtomicBool) -> Result<Pixels> {
    decode_tiles(
        texture.data(),
        texture.size,
        texture.tile_size,
        texture.format,
        budget,
        cancelled,
    )
    .map(|main| Pixels {
        size: texture.size,
        main: Some(main),
        province: None,
    })
}
fn decode_tiles(
    data: &[u8],
    size: Size,
    tile: Size,
    format: Format,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    if Compressed::payload_len(size, tile, format) != Some(data.len()) {
        return Err(Error::Message("compressed tile payload mismatch"));
    }
    check(cancelled)?;
    if size == tile {
        return decode_format(data, size, format, budget, cancelled);
    }
    let mut out = Bytes::zeroed(
        size.rgba_bytes()
            .ok_or(Error::Message("tiled image size overflow"))?,
        budget,
    )?;
    let columns = size.width.div_ceil(tile.width);
    for (index, block) in data
        .chunks_exact(format.byte_len(tile).unwrap())
        .enumerate()
    {
        check(cancelled)?;
        let pixels = decode_format(block, tile, format, budget, cancelled)?;
        let x = index as u32 % columns * tile.width;
        let y = index as u32 / columns * tile.height;
        for (row, input) in pixels
            .as_slice()
            .chunks_exact(tile.width as usize * 4)
            .take((size.height - y).min(tile.height) as usize)
            .enumerate()
        {
            let at = ((y as usize + row) * size.width as usize + x as usize) * 4;
            let input = &input[..(size.width - x).min(tile.width) as usize * 4];
            out.as_mut_slice()[at..at + input.len()].copy_from_slice(input);
        }
    }
    Ok(out)
}
fn decode_format(
    data: &[u8],
    size: Size,
    format: Format,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    match format {
        Format::Etc1 => decode_blocks(data, size, budget, cancelled),
        Format::Pvrtc1Rgba4 => pvrtc::decode(data, size, budget, cancelled),
        Format::Bc1Rgb | Format::Bc3Rgba | Format::Bc1RgbVita | Format::Bc3RgbaVita => {
            bc::decode(data, size, format, budget, cancelled)
        }
    }
}
fn decode_blocks(
    data: &[u8],
    size: Size,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<Bytes> {
    check(cancelled)?;
    if Format::Etc1.byte_len(size) != Some(data.len()) {
        return Err(Error::Message("ETC1 payload size mismatch"));
    }
    let mut out = Bytes::zeroed(
        size.rgba_bytes()
            .ok_or(Error::Message("ETC1 image overflow"))?,
        budget,
    )?;
    let columns = size.width.div_ceil(4) as usize;
    for (row, blocks) in data.chunks_exact(columns * 8).enumerate() {
        check(cancelled)?;
        for (col, block) in blocks.as_chunks::<8>().0.iter().enumerate() {
            let colors = bases(block)?;
            let bits = u32::from_be_bytes(block[4..8].try_into().unwrap());
            for y in 0..4 {
                for x in 0..4 {
                    let (px, py) = (col * 4 + x, row * 4 + y);
                    if px >= size.width as usize || py >= size.height as usize {
                        continue;
                    }
                    let half = if block[3] & 1 == 0 { x / 2 } else { y / 2 };
                    let table = (block[3] >> if half == 0 { 5 } else { 2 }) & 7;
                    let shift = x * 4 + y;
                    let index = ((bits >> shift) & 1) | (((bits >> (16 + shift)) & 1) << 1);
                    let modifier = MODIFIERS[table as usize][index as usize];
                    let at = (py * size.width as usize + px) * 4;
                    let pixel = &mut out.as_mut_slice()[at..at + 4];
                    for c in 0..3 {
                        pixel[c] = (colors[half][c] + modifier).clamp(0, 255) as u8;
                    }
                    pixel[3] = 255;
                }
            }
        }
    }
    Ok(out)
}
pub(crate) fn decode_container(
    data: &[u8],
    mode: codec::Mode,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<(Bytes, Tags)> {
    if mode == codec::Mode::Province {
        return Err(Error::Message("compressed texture has no province palette"));
    }
    let header = probe(data)?;
    decode_prepared(&data[header.offset..], &header, mode, budget, cancelled)
}
pub(crate) fn decode_prepared(
    data: &[u8],
    header: &Header,
    mode: codec::Mode,
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<(Bytes, Tags)> {
    if mode == codec::Mode::Province {
        return Err(Error::Message("compressed texture has no province palette"));
    }
    let rgba = decode_tiles(
        data,
        header.size,
        header.tile_size,
        header.format,
        budget,
        cancelled,
    )?;
    if mode == codec::Mode::Main {
        return Ok((rgba, header.tags.clone()));
    }
    let mut mask = Bytes::zeroed(header.size.rgba_bytes().unwrap() / 4, budget)?;
    for (pixel, target) in rgba
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(mask.as_mut_slice())
    {
        *target = transform::gray(pixel[0], pixel[1], pixel[2]);
    }
    Ok((mask, Vec::new()))
}
/// Package pre-encoded ETC1 blocks. No mip chain, transcode or padding resize.
pub fn ktx(size: Size, blocks: &[u8]) -> Result<Vec<u8>> {
    ktx_format(size, Format::Etc1, blocks)
}

pub fn ktx_format(size: Size, format: Format, blocks: &[u8]) -> Result<Vec<u8>> {
    ktx_tiles(size, size, format, blocks, &Vec::new())
}

/// KTX1 array elements hold independent, equal-size native textures. The
/// krkr.canvas key places them in row-major order without changing script size.
pub fn ktx_tiles(
    size: Size,
    tile: Size,
    format: Format,
    blocks: &[u8],
    tags: &Tags,
) -> Result<Vec<u8>> {
    if Compressed::payload_len(size, tile, format) != Some(blocks.len()) {
        return Err(Error::Message("compressed payload size mismatch"));
    }
    let mut metadata = Vec::new();
    metadata::entry(&mut metadata, b"KTXorientation", b"S=r,T=d\0");
    if !tags.is_empty() {
        metadata::entry(&mut metadata, metadata::TAGS, &metadata::encode(tags)?);
    }
    let arrays = if size == tile {
        0
    } else {
        let mut canvas = size.width.to_le_bytes().to_vec();
        canvas.extend(size.height.to_le_bytes());
        metadata::entry(&mut metadata, metadata::CANVAS, &canvas);
        size.width
            .div_ceil(tile.width)
            .checked_mul(size.height.div_ceil(tile.height))
            .ok_or(Error::Message("too many texture tiles"))?
    };
    if metadata.len() > metadata::LIMIT || blocks.len() > u32::MAX as usize {
        return Err(Error::Message("KTX metadata or payload capacity exceeded"));
    }
    let capacity = blocks
        .len()
        .checked_add(68 + metadata.len())
        .ok_or(Error::Message("KTX container size overflow"))?;
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(MAGIC);
    for word in [
        0x04030201,
        0,
        1,
        0,
        format.gl_internal(),
        if format.opaque() { 0x1907 } else { 0x1908 },
        tile.width,
        tile.height,
        0,
        arrays,
        1,
        1,
        metadata.len() as u32,
    ] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend(metadata);
    bytes.extend_from_slice(&(blocks.len() as u32).to_le_bytes());
    bytes.extend_from_slice(blocks);
    probe(&bytes)?;
    Ok(bytes)
}

/// Combine offline-encoded tiles without decoding or recompressing blocks.
pub fn assemble(size: Size, tiles: &[Vec<u8>], tags: &Tags) -> Result<Vec<u8>> {
    let first = probe(tiles.first().ok_or(Error::Message("empty texture tiles"))?)?;
    let mut blocks = Vec::new();
    for tile in tiles {
        let header = probe(tile)?;
        if header.size != first.size
            || header.tile_size != first.size
            || header.format != first.format
            || !header.tags.is_empty()
        {
            return Err(Error::Message(
                "texture tiles differ in format or dimensions",
            ));
        }
        blocks.extend_from_slice(&tile[header.offset..]);
    }
    ktx_tiles(size, first.size, first.format, &blocks, tags)
}

pub fn validate_tags(tags: &Tags) -> Result<()> {
    metadata::encode(tags).map(|_| ())
}

/// Prepare every tile in its final Vita block layout while preserving tags.
/// The private internal-format IDs distinguish this from ordinary S3TC KTX.
pub fn vita_bc(data: &[u8]) -> Result<Vec<u8>> {
    let header = probe(data)?;
    let native = header
        .format
        .vita()
        .ok_or(Error::Message("expected BC1/BC3 KTX"))?;
    if header.format.is_vita() {
        return Err(Error::Message("BC texture is already in Vita layout"));
    }
    let input = &data[header.offset..];
    let mut blocks = vec![0; input.len()];
    let length = native
        .byte_len(header.tile_size)
        .ok_or(Error::Message("Vita BC tiles require POT dimensions >=8"))?;
    for (src, dst) in input
        .chunks_exact(length)
        .zip(blocks.chunks_exact_mut(length))
    {
        krkr_protocol::texture::reorder_bc(header.tile_size, header.format, src, dst, true)
            .map_err(Error::Message)?;
    }
    ktx_tiles(header.size, header.tile_size, native, &blocks, &header.tags)
}

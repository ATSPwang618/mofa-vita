//! KBCT v1: tagged BC tiles, stored natively (0) or using bc-crunch (1).
//! Storage codecs always produce budgeted native BC blocks for the GPU.
use super::*;
use krkr_protocol::texture::{Compressed, Format, reorder_bc};
pub const MAGIC: &[u8; 8] = b"KBCT\x01\0\0\0";
struct Container<'a> {
    header: compressed::Header,
    tiles: Vec<(u32, &'a [u8])>,
}
fn parse(data: &[u8]) -> Result<Container<'_>> {
    let word = |at: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(
            data.get(at..at + 4)
                .ok_or(Error::Message("truncated packed BC container"))?
                .try_into()
                .unwrap(),
        ))
    };
    if !data.starts_with(MAGIC) {
        return Err(Error::Message("invalid packed BC container signature"));
    }
    let size = image_size(word(8)?, word(12)?)?;
    let tile = image_size(word(16)?, word(20)?)?;
    let format = match word(24)? {
        1 => Format::Bc1RgbVita,
        3 => Format::Bc3RgbaVita,
        _ => return Err(Error::Message("invalid packed BC format")),
    };
    if tile.width > 1024 || tile.height > 1024 {
        return Err(Error::Message("packed BC tile exceeds 1024"));
    }
    let bytes = Compressed::payload_len(size, tile, format)
        .ok_or(Error::Message("invalid packed BC tiled dimensions"))?;
    // Same decoded block ceiling as the Vita resource reader, independent of
    // how small a malicious or highly repetitive packed BC input happens to be.
    if bytes > 32 * 1024 * 1024 - 65536 {
        return Err(Error::Message("packed BC decoded blocks exceed limit"));
    }
    let count = bytes / format.byte_len(tile).unwrap();
    let tags_len = word(28)? as usize;
    if tags_len > compressed::metadata::LIMIT {
        return Err(Error::Message("packed BC tags exceed limit"));
    }
    let mut at = 32 + tags_len;
    let tags = compressed::metadata::decode(
        data.get(32..at)
            .ok_or(Error::Message("truncated packed BC tags"))?,
    )?;
    if count > 4096 || count > data.len().saturating_sub(at) / 8 {
        return Err(Error::Message("invalid packed BC tile table size"));
    }
    let mut tiles = Vec::with_capacity(count);
    for _ in 0..count {
        let kind = word(at)?;
        let len = word(at + 4)? as usize;
        at += 8;
        let end = at
            .checked_add(len)
            .ok_or(Error::Message("packed BC tile overflow"))?;
        let payload = data
            .get(at..end)
            .ok_or(Error::Message("truncated packed BC tile"))?;
        match kind {
            0 if Some(len) == format.byte_len(tile) => {}
            1 if len >= 4 && len < format.byte_len(tile).unwrap() => {}
            _ => return Err(Error::Message("invalid packed BC tile encoding")),
        }
        tiles.push((kind, payload));
        at = end;
    }
    if at != data.len() {
        return Err(Error::Message("trailing packed BC container bytes"));
    }
    Ok(Container {
        header: compressed::Header {
            size,
            tile_size: tile,
            format,
            offset: 0,
            tags,
        },
        tiles,
    })
}
pub(crate) fn probe(data: &[u8]) -> Result<compressed::Header> {
    Ok(parse(data)?.header)
}
pub fn storage_stats(data: &[u8]) -> Result<(usize, usize, usize)> {
    let parsed = parse(data)?;
    let packed = parsed.tiles.iter().filter(|(kind, _)| *kind != 0).count();
    let bytes = Compressed::payload_len(
        parsed.header.size,
        parsed.header.tile_size,
        parsed.header.format,
    )
    .unwrap();
    Ok((packed, parsed.tiles.len() - packed, bytes))
}
fn package(
    size: Size,
    tile: Size,
    format: Format,
    tiles: &[(u32, &[u8])],
    tags: &Tags,
) -> Result<Vec<u8>> {
    let tags = compressed::metadata::encode(tags)?;
    let mut out = MAGIC.to_vec();
    for word in [
        size.width,
        size.height,
        tile.width,
        tile.height,
        if format.linear() == Format::Bc3Rgba {
            3
        } else {
            1
        },
        tags.len() as u32,
    ] {
        out.extend(word.to_le_bytes());
    }
    out.extend(tags);
    for &(kind, bytes) in tiles {
        out.extend(kind.to_le_bytes());
        out.extend(
            u32::try_from(bytes.len())
                .map_err(|_| Error::Message("packed BC tile too large"))?
                .to_le_bytes(),
        );
        out.extend(bytes);
    }
    parse(&out)?;
    Ok(out)
}
pub fn wrap_bc(ktx: &[u8]) -> Result<Vec<u8>> {
    let header = compressed::probe(ktx)?;
    let native = if header.format.is_vita() {
        ktx.to_vec()
    } else {
        compressed::vita_bc(ktx)?
    };
    let header = compressed::probe(&native)?;
    if header.size != header.tile_size {
        return Err(Error::Message("expected single BC fallback tile"));
    }
    package(
        header.size,
        header.tile_size,
        header.format,
        &[(0, &native[header.offset..])],
        &header.tags,
    )
}
fn packed_format(format: Format) -> bc_crunch::Format {
    if format.linear() == Format::Bc3Rgba {
        bc_crunch::Format::Bc3
    } else {
        bc_crunch::Format::Bc1
    }
}
/// Losslessly pack a single linear BC tile. Incompressible tiles retain native
/// blocks; compression never changes color endpoints, selectors or alpha.
pub fn wrap_packed_bc(ktx: &[u8]) -> Result<Vec<u8>> {
    let header = compressed::probe(ktx)?;
    if header.size != header.tile_size || !matches!(header.format, Format::Bc1Rgb | Format::Bc3Rgba)
    {
        return Err(Error::Message("bc-crunch requires one linear BC1/BC3 tile"));
    }
    let source = &ktx[header.offset..];
    let packed = bc_crunch::compress(
        header.size.width,
        header.size.height,
        packed_format(header.format),
        source,
    )
    .map_err(|e| Error::Codec(e.to_string()))?;
    if packed.len() >= source.len() {
        return wrap_bc(ktx);
    }
    package(
        header.size,
        header.tile_size,
        header.format,
        &[(1, &packed)],
        &header.tags,
    )
}
pub fn assemble(size: Size, tiles: &[Vec<u8>], tags: &Tags) -> Result<Vec<u8>> {
    let parsed = tiles.iter().map(|t| parse(t)).collect::<Result<Vec<_>>>()?;
    let first = parsed
        .first()
        .ok_or(Error::Message("empty packed BC tiles"))?;
    let mut parts = Vec::with_capacity(parsed.len());
    for tile in &parsed {
        if tile.header.size != first.header.size
            || tile.header.size != tile.header.tile_size
            || tile.header.format != first.header.format
            || !tile.header.tags.is_empty()
            || tile.tiles.len() != 1
        {
            return Err(Error::Message("incompatible packed BC tiles"));
        }
        parts.push(tile.tiles[0]);
    }
    package(size, first.header.size, first.header.format, &parts, tags)
}
pub(crate) fn transcode(
    data: &[u8],
    budget: &Budget,
    cancelled: &AtomicBool,
) -> Result<(Bytes, compressed::Header)> {
    let _profile =
        krkr_protocol::profile::span_detail("image.bc_unpack", || format!("bytes={}", data.len()));
    let container = parse(data)?;
    let header = container.header;
    let bytes = Compressed::payload_len(header.size, header.tile_size, header.format).unwrap();
    check(cancelled)?;
    let mut output = Bytes::zeroed(bytes, budget)?;
    let tile_bytes = header.format.byte_len(header.tile_size).unwrap();
    let has_packed = container.tiles.iter().any(|t| t.0 == 1);
    // Fixed probability models/dictionary plus one linear tile. Both are charged
    // before allocation; no temporary RGBA image is needed for native upload.
    let _scratch = if has_packed {
        Some(reserve(budget, 64 * 1024)?)
    } else {
        None
    };
    let mut linear = if has_packed {
        Some(Bytes::zeroed(tile_bytes, budget)?)
    } else {
        None
    };
    let packed = container
        .tiles
        .iter()
        .filter(|(kind, _)| *kind == 1)
        .count();
    // Decode large independent tiles directly into disjoint native BC ranges.
    // One helper bounds CPU concurrency and adds at most one tile workspace;
    // small images and tight staging budgets keep the serial path.
    const STACK: usize = 128 * 1024;
    if packed >= 4 && bytes >= 2 * 1024 * 1024 {
        let extra = Bytes::zeroed(tile_bytes, budget).and_then(|linear| {
            budget
                .reserve(64 * 1024 + STACK)
                .map(|permit| (linear, permit))
        });
        if let Ok((mut other, _permit)) = extra {
            let result = std::thread::scope(|scope| {
                let split = container.tiles.len() / 2;
                let (left, right) = container.tiles.split_at(split);
                let (first, second) = output.as_mut_slice().split_at_mut(split * tile_bytes);
                let worker = std::thread::Builder::new()
                    .name("krkr-image-bc".into())
                    .stack_size(STACK)
                    .spawn_scoped(scope, || {
                        decode_tiles(&header, right, second, other.as_mut_slice(), cancelled)
                    });
                let Ok(worker) = worker else { return None };
                let result = decode_tiles(
                    &header,
                    left,
                    first,
                    linear.as_mut().unwrap().as_mut_slice(),
                    cancelled,
                );
                let other = worker
                    .join()
                    .unwrap_or(Err(Error::Message("BC decoder worker panicked")));
                Some(result.and(other))
            });
            if let Some(result) = result {
                result?;
                return Ok((output, header));
            }
        }
    }
    decode_tiles(
        &header,
        &container.tiles,
        output.as_mut_slice(),
        linear.as_mut().map_or(&mut [], Bytes::as_mut_slice),
        cancelled,
    )?;
    Ok((output, header))
}

fn decode_tiles(
    header: &compressed::Header,
    tiles: &[(u32, &[u8])],
    output: &mut [u8],
    linear: &mut [u8],
    cancelled: &AtomicBool,
) -> Result<()> {
    let tile_bytes = header.format.byte_len(header.tile_size).unwrap();
    for (&(kind, data), out) in tiles.iter().zip(output.chunks_exact_mut(tile_bytes)) {
        check(cancelled)?;
        if kind == 0 {
            out.copy_from_slice(data);
        } else {
            {
                let _profile = krkr_protocol::profile::span("image.bc_decode");
                bc_crunch::decompress_into(
                    header.tile_size.width,
                    header.tile_size.height,
                    packed_format(header.format),
                    data,
                    linear,
                )
                .map_err(|e| Error::Codec(e.to_string()))?;
            }
            {
                let _profile = krkr_protocol::profile::span("image.bc_reorder");
                reorder_bc(header.tile_size, header.format, linear, out, true)
                    .map_err(Error::Message)?;
            }
        }
    }
    Ok(())
}

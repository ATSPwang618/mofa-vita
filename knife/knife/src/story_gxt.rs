/// Exact key for an offline-processed GXT appended after the original NAM ordinals.
pub(crate) fn variant_stem(
    index: usize, grade: i32, bright: f32, contrast: f32, raw: bool, strip: Option<(usize, usize)>,
) -> Option<String> {
    if index > 0xf_ffff || !(0..=82).contains(&grade) || raw
        || !bright.is_finite() || !contrast.is_finite()
    {
        return None;
    }
    let language = match strip {
        None => 0,
        Some((0, 2)) => 1,
        Some((1, 2)) => 2,
        _ => return None,
    };
    Some(format!("KQ{index:05X}{grade:02X}{language:X}{:08X}{:08X}",
        bright.to_bits(), contrast.to_bits()))
}

pub(crate) const TILE_INTERIOR: u32 = 504;
pub(crate) const GUARD: u32 = 4;

#[derive(Clone, Copy)]
pub(crate) struct GxtDescriptor {
    pub(crate) index: usize,
    pub(crate) size: [u32; 2],
    pub(crate) format: u32,
    pub(crate) payload: usize,
}

pub(crate) struct TiledImage {
    pub(crate) logical: [u32; 2],
    pub(crate) physical: [u32; 2],
    pub(crate) columns: u32,
    pub(crate) fallback: GxtDescriptor,
    pub(crate) tiles: Vec<GxtDescriptor>,
}

impl TiledImage {
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        let mut fields = bytes.strip_prefix(b"KQTL")?.chunks_exact(4);
        if !fields.remainder().is_empty() { return None; }
        let mut next = move || fields.next().map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        if next()? != 1 { return None; }
        let logical = [next()?, next()?];
        let physical = [next()?, next()?];
        let columns = next()?;
        let rows = next()?;
        if (0..2).any(|i| physical[i] == 0 || physical[i] > logical[i] || logical[i] > 32768)
            || columns != physical[0].div_ceil(TILE_INTERIOR)
            || rows != physical[1].div_ceil(TILE_INTERIOR)
            || columns.checked_mul(rows)? > 256
        {
            return None;
        }
        let length = 32 + (1 + (rows * columns) as usize) * 20;
        if bytes.len() != length
            && (bytes.len() != length.next_multiple_of(2048)
                || !bytes.get(length..)?.iter().all(|&byte| byte == 0))
        {
            return None;
        }
        let mut descriptor = || {
            let index = next()? as usize;
            let size = [next()?, next()?];
            let format = next()?;
            let payload = next()? as usize;
            let divisor = match format { 0x8300_0000 | 0x8500_0000 => 2, 0x8700_0000 => 1, _ => return None };
            if size.iter().any(|&v| v == 0 || v > 2048)
                || size[0] * size[1] > 960 * 1080
                || payload != (size[0].next_power_of_two().max(4) * size[1].next_power_of_two().max(4) / divisor) as usize
                || payload > 2 * 1024 * 1024
            {
                return None;
            }
            Some(GxtDescriptor { index, size, format, payload })
        };
        let fallback = descriptor()?;
        // The low-memory image remains CPU-decodable; PVRTC is native tile-only here.
        if fallback.format == 0x8300_0000 { return None; }
        if (0..2).any(|i| fallback.size[i] > physical[i]) { return None; }
        let mut tiles = Vec::with_capacity((rows * columns) as usize);
        for i in 0..rows * columns {
            let (_, crop) = tile_rects(physical, columns, i);
            let tile = descriptor()?;
            if tile.size != [crop[2] - crop[0], crop[3] - crop[1]] { return None; }
            tiles.push(tile);
        }
        Some(Self { logical, physical, columns, fallback, tiles })
    }
}

pub(crate) fn tile_rects([width, height]: [u32; 2], columns: u32, tile: u32) -> ([u32; 4], [u32; 4]) {
    let x = (tile % columns) * TILE_INTERIOR;
    let y = (tile / columns) * TILE_INTERIOR;
    let right = (x + TILE_INTERIOR).min(width);
    let bottom = (y + TILE_INTERIOR).min(height);
    ([x, y, right, bottom],
        [x.saturating_sub(GUARD), y.saturating_sub(GUARD), (right + GUARD).min(width), (bottom + GUARD).min(height)])
}

#[cfg(test)]
mod pvrtc_tests {
    use super::*;

    fn grid() -> Vec<u8> {
        let mut data = b"KQTL".to_vec();
        let words: [u32; 22] = [1, 1024, 1008, 512, 504, 2, 1,
            10, 256, 254, 0x8700_0000, 65536,
            11, 508, 504, 0x8300_0000, 131072,
            12, 12, 504, 0x8300_0000, 4096];
        for word in words { data.extend(word.to_le_bytes()); }
        data
    }

    #[test]
    fn pvrtc2_tiles_retain_logical_size_guards_and_dxt_fallback() {
        let parsed = TiledImage::parse(&grid()).unwrap();
        assert_eq!(parsed.logical, [1024, 1008]);
        assert_eq!(parsed.physical, [512, 504]);
        assert_eq!(parsed.fallback.format, 0x8700_0000);
        assert_eq!(parsed.tiles[1].size, [12, 504]);
        assert_eq!(parsed.tiles[1].payload, 4096);
    }

    #[test]
    fn rejects_pvrtc_fallback_and_unpadded_payload() {
        let mut bytes = grid();
        bytes[44..48].copy_from_slice(&0x8300_0000u32.to_le_bytes());
        bytes[48..52].copy_from_slice(&32768u32.to_le_bytes());
        assert!(TiledImage::parse(&bytes).is_none());
        let mut bytes = grid();
        bytes[68..72].copy_from_slice(&(508u32*508/2).to_le_bytes());
        assert!(TiledImage::parse(&bytes).is_none());
    }
}

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
            let divisor = match format { 0x8500_0000 => 2, 0x8700_0000 => 1, _ => return None };
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

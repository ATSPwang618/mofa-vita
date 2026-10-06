//! Budget-bounded native texture tiling; encoding uses the in-process rgbcx backend.
use krkr_protocol::graphics::Size;
use rayon::prelude::*;

/// POT native tiles, with the complete raster rounded only to tile boundaries.
/// A 5000-pixel strip needs 5120 pixels of storage rather than an 8192-wide level.
pub fn storage_size(size: Size) -> Option<Size> {
    let axis = |v: u32| {
        let n = if v <= 1024 {
            v.max(8).next_power_of_two()
        } else {
            v.checked_next_multiple_of(1024)?
        };
        (v != 0 && n < 65536).then_some(n)
    };
    let size = Size {
        width: axis(size.width)?,
        height: axis(size.height)?,
    };
    // Vita's resource reader accepts 32 MiB, including the KTX metadata.
    (size.rgba_bytes()? / 4 <= 32 * 1024 * 1024 - 65536 - 68).then_some(size)
}

/// A denser BC grid can preserve detail that a coarse grid cannot represent.
/// Only offer alternatives still at least 25% smaller than the lossless raster;
/// prefer the direction that needs fewer native tiles. Never retry recursively.
pub fn quality_sizes(
    stored: Size,
    lossless: Size,
    format: krkr_protocol::texture::Format,
) -> Vec<Size> {
    let Some(raw) = lossless.rgba_bytes() else {
        return Vec::new();
    };
    let mut sizes = Vec::new();
    for (width, height) in [
        (stored.width.checked_mul(2), Some(stored.height)),
        (Some(stored.width), stored.height.checked_mul(2)),
    ] {
        let (Some(width), Some(height)) = (width, height) else {
            continue;
        };
        let size = Size { width, height };
        if storage_size(size) == Some(size)
            && format
                .linear()
                .byte_len(size)
                .is_some_and(|n| (n as u64) * 4 <= (raw as u64) * 3)
        {
            sizes.push(size);
        }
    }
    sizes.sort_by_key(|size| size.width.div_ceil(1024) * size.height.div_ceil(1024));
    sizes
}

pub fn encode_tiled(
    size: Size,
    rgba: &[u8],
    tags: &krkr_image::Tags,
    encoder: impl Fn(Size, &[u8]) -> Result<Vec<u8>, String> + Sync,
) -> Result<Vec<u8>, String> {
    encode_tiles(size, rgba, tags, encoder)
}

#[derive(Debug, PartialEq, Eq)]
pub enum EncodeError {
    Failed(String),
    Quality(String),
}
impl From<String> for EncodeError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

/// Reject a failed image before encoding all its other tiles. Encoder and
/// decoding errors remain fatal; quality rejection alone permits lossless
/// fallback. Rayon cancels pending work after any later tile rejects too.
pub fn encode_tiled_checked(
    size: Size,
    rgba: &[u8],
    tags: &krkr_image::Tags,
    encoder: impl Fn(Size, &[u8]) -> Result<Vec<u8>, String> + Sync,
    check: impl Fn(Size, &[u8], &[u8]) -> Result<Option<String>, String> + Sync,
) -> Result<Vec<u8>, EncodeError> {
    encode_tiles(size, rgba, tags, |size, pixels| {
        let data = encoder(size, pixels)?;
        if let Some(reason) = check(size, pixels, &data)? {
            return Err(EncodeError::Quality(reason));
        }
        Ok(data)
    })
}

fn encode_tiles<E: From<String> + Send>(
    size: Size,
    rgba: &[u8],
    tags: &krkr_image::Tags,
    encoder: impl Fn(Size, &[u8]) -> Result<Vec<u8>, E> + Sync,
) -> Result<Vec<u8>, E> {
    if storage_size(size) != Some(size) || size.rgba_bytes() != Some(rgba.len()) {
        return Err("invalid compressed tile storage dimensions"
            .to_owned()
            .into());
    }
    let tile = Size {
        width: size.width.min(1024),
        height: size.height.min(1024),
    };
    if tile == size {
        let encoded = encoder(size, rgba)?;
        return if tags.is_empty() {
            Ok(encoded)
        } else {
            assemble(size, &[encoded], tags).map_err(|e| e.to_string().into())
        };
    }
    let columns = size.width / tile.width;
    let count = columns * (size.height / tile.height);
    let extract = |pixels: &mut [u8], index: u32| {
        let left = (index % columns) * tile.width;
        let top = (index / columns) * tile.height;
        for (row, out) in pixels.chunks_exact_mut(tile.width as usize * 4).enumerate() {
            let at = ((top as usize + row) * size.width as usize + left as usize) * 4;
            out.copy_from_slice(&rgba[at..at + out.len()]);
        }
    };
    // One checked tile first bounds wasted encoding for images that cannot be
    // published. Across images the outer file pool still runs concurrently.
    let first = {
        let mut pixels = vec![0; tile.rgba_bytes().unwrap()];
        extract(&mut pixels, 0);
        encoder(tile, &pixels)?
    };
    let rest = (1..count)
        .into_par_iter()
        .map_init(
            || vec![0; tile.rgba_bytes().unwrap()],
            |pixels, index| {
                extract(pixels, index);
                encoder(tile, pixels)
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    let mut tiles = Vec::with_capacity(count as usize);
    tiles.push(first);
    tiles.extend(rest);
    assemble(size, &tiles, tags).map_err(|e| e.to_string().into())
}

fn assemble(size: Size, tiles: &[Vec<u8>], tags: &krkr_image::Tags) -> krkr_image::Result<Vec<u8>> {
    if tiles
        .first()
        .is_some_and(|t| t.starts_with(krkr_image::packed_bc::MAGIC))
    {
        krkr_image::packed_bc::assemble(size, tiles, tags)
    } else {
        krkr_image::compressed::assemble(size, tiles, tags)
    }
}

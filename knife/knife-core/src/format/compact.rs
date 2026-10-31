//! Reduced-density knife images with independent layout and sampling dimensions.

use anyhow::{Context, Result, ensure};

pub const HEADER_SIZE: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Jpeg = 1,
    Png = 2,
    Bc7Srgb = 3,
    Bc7Unorm = 4,
    PngUnorm = 7,
    /// Vita-only GXT with the default Alter colour transform already applied.
    /// The payload keeps straight alpha and is sampled as sRGB exactly once.
    GxtAlterSrgb = 8,
    /// Vita native encoded bytes in GPU-compressed blocks; sampled as UNORM.
    /// This is a distinct asset ABI: legacy codec8 carries pre-Alter colours.
    GxtNativeUnorm = 9,
}

#[derive(Debug)]
pub struct Image<'a> {
    pub logical: [u32; 2],
    pub size: [u32; 2],
    pub encoding: Encoding,
    pub payload: &'a [u8],
}

pub fn is_compact(data: &[u8]) -> bool {
    data.starts_with(b"KIMG")
}

pub fn parse(data: &[u8]) -> Result<Image<'_>> {
    ensure!(data.len() >= HEADER_SIZE && is_compact(data), "not a KIMG image");
    let word = |offset| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    ensure!(word(4) == 1, "unsupported KIMG version");
    let logical = [word(8), word(12)];
    let size = [word(16), word(20)];
    for (original, physical) in logical.into_iter().zip(size) {
        ensure!(physical > 0 && physical <= original && original <= 32768, "invalid KIMG dimensions");
    }
    ensure!(u64::from(size[0]) * u64::from(size[1]) <= 268_435_456, "KIMG image exceeds pixel budget");
    let encoding = match word(24) {
        1 => Encoding::Jpeg,
        2 => Encoding::Png,
        3 => Encoding::Bc7Srgb,
        4 => Encoding::Bc7Unorm,
        7 => Encoding::PngUnorm,
        8 => Encoding::GxtAlterSrgb,
        9 => Encoding::GxtNativeUnorm,
        _ => anyhow::bail!("unsupported KIMG encoding"),
    };
    let len = word(28) as usize;
    ensure!(len > 0, "empty KIMG payload");
    let payload = data.get(HEADER_SIZE..HEADER_SIZE.checked_add(len).context("KIMG length overflow")?)
        .context("truncated KIMG payload")?;
    if matches!(encoding, Encoding::Bc7Srgb | Encoding::Bc7Unorm) {
        let blocks = u64::from(size[0].div_ceil(4)) * u64::from(size[1].div_ceil(4)) * 16;
        ensure!(len as u64 == blocks, "KIMG BC7 block length mismatch");
    }
    Ok(Image { logical, size, encoding, payload })
}

/// Wrap already encoded pixels. Parsing the result checks the same limits as the runtime.
pub fn encode(logical: [u32; 2], size: [u32; 2], encoding: Encoding, payload: &[u8]) -> Result<Vec<u8>> {
    let len = u32::try_from(payload.len()).context("KIMG payload too large")?;
    let mut data = Vec::with_capacity(HEADER_SIZE + payload.len());
    data.extend_from_slice(b"KIMG");
    for value in [1, logical[0], logical[1], size[0], size[1], encoding as u32, len] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    data.extend_from_slice(payload);
    parse(&data)?;
    Ok(data)
}

impl Image<'_> {
    pub fn is_srgb(&self) -> bool {
        !matches!(self.encoding, Encoding::Bc7Unorm | Encoding::PngUnorm | Encoding::GxtNativeUnorm)
    }

    /// A BC7 image can use the existing GPU path, or its CPU decoder for colour grading / language strips.
    pub fn texture(&self) -> Option<super::bntx::Texture> {
        use super::bntx::{TexFormat, Texture};
        let format = match self.encoding {
            Encoding::Bc7Srgb => TexFormat::Bc7Srgb,
            Encoding::Bc7Unorm => TexFormat::Bc7Unorm,
            _ => return None,
        };
        Some(Texture { name: "KIMG".into(), format, width: self.size[0], height: self.size[1], mips: 1, data: self.payload.to_vec() })
    }
}

/// Named, individually reduced images from a UI BNTX. Order and original layout sizes are retained.
pub fn is_atlas(data: &[u8]) -> bool {
    data.starts_with(b"KATL")
}

pub fn parse_atlas(data: &[u8]) -> Result<Vec<(&str, &[u8])>> {
    ensure!(data.len() >= 12 && is_atlas(data), "not a KATL atlas");
    let word = |o: usize| -> Result<usize> {
        Ok(u32::from_le_bytes(data.get(o..o + 4).context("truncated KATL word")?.try_into()?) as usize)
    };
    ensure!(word(4)? == 1, "unsupported KATL version");
    let count = word(8)?;
    ensure!(count <= 4096, "KATL texture count exceeds budget");
    let mut out = Vec::with_capacity(count);
    let mut p = 12usize;
    for _ in 0..count {
        let (name_len, image_len) = (word(p)?, word(p + 4)?);
        ensure!(name_len > 0 && name_len <= 1024, "invalid KATL name length");
        p += 8;
        let end = p.checked_add(name_len).context("KATL name length overflow")?;
        let name = std::str::from_utf8(data.get(p..end).context("truncated KATL name")?)?;
        p = end;
        let end = p.checked_add(image_len).context("KATL image length overflow")?;
        let image = data.get(p..end).context("truncated KATL image")?;
        parse(image)?;
        out.push((name, image));
        p = end;
    }
    ensure!(p == data.len(), "unexpected bytes after KATL atlas");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        // A 7x5 BC7 image occupies four 4x4 blocks (64 bytes), laid out as 14x10.
        let mut bytes = b"KIMG".to_vec();
        for value in [1u32, 14, 10, 7, 5, 3, 64] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        bytes.extend_from_slice(&[0u8; 64]);
        bytes
    }

    #[test]
    fn reduced_texture_keeps_layout_without_sampling_archive_padding() {
        let mut bytes = fixture();
        bytes.resize(2048, 0);
        let image = parse(&bytes).unwrap();
        assert_eq!(image.logical, [14, 10]);
        assert_eq!(image.size, [7, 5]);
        assert_eq!(image.encoding, Encoding::Bc7Srgb);
        assert_eq!(image.payload.len(), 64);
    }

    #[test]
    fn rejects_truncated_or_inconsistent_bc7_blocks() {
        let bytes = fixture();
        for cut in [0, 3, 15, 31, 32, 63, 95] {
            assert!(parse(&bytes[..cut]).is_err(), "accepted {cut} bytes");
        }
        let mut bytes = fixture();
        bytes[28..32].copy_from_slice(&16u32.to_le_bytes());
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn portable_header_keeps_sampling_layout_and_colour_space_independent() {
        let mut bytes = fixture();
        for encoding in [1u32, 2, 7] {
            bytes[24..28].copy_from_slice(&encoding.to_le_bytes());
            let image = parse(&bytes).expect("PNG and JPEG payloads must be supported");
            assert_eq!(image.logical, [14, 10]);
            assert_eq!(image.size, [7, 5]);
            assert_eq!(image.is_srgb(), encoding != 7);
            assert!(image.texture().is_none(), "PNG/JPEG must decode before uploading");
        }
    }

    #[test]
    fn vita_gxt_is_borrowed_with_layout_and_does_not_enter_the_bc7_decoder() {
        let mut bytes = fixture();
        bytes[24..28].copy_from_slice(&8u32.to_le_bytes());
        let image = parse(&bytes).unwrap();
        assert_eq!(image.logical, [14, 10]);
        assert_eq!(image.size, [7, 5]);
        assert_eq!(image.encoding as u32, 8);
        assert!(image.is_srgb());
        assert!(image.texture().is_none());
        assert!(std::ptr::eq(image.payload.as_ptr(), bytes[HEADER_SIZE..].as_ptr()));
        // The Vita GXT parser validates this payload before GPU allocation;
        // KIMG is only the common dimensions/container framing.
        assert!(parse(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn native_gxt_has_distinct_unorm_colour_contract() {
        let mut bytes = fixture();
        bytes[24..28].copy_from_slice(&9u32.to_le_bytes());
        let image = parse(&bytes).unwrap();
        assert_eq!(image.encoding, Encoding::GxtNativeUnorm);
        assert!(!image.is_srgb()); assert!(image.texture().is_none());
        assert!(std::ptr::eq(image.payload.as_ptr(), bytes[HEADER_SIZE..].as_ptr()));
    }

    #[test]
    fn named_atlas_preserves_order_and_rejects_truncation() {
        let image = fixture();
        let mut atlas = b"KATL".to_vec();
        atlas.extend_from_slice(&1u32.to_le_bytes());
        atlas.extend_from_slice(&2u32.to_le_bytes());
        for name in ["button_zc", "button_ja"] {
            atlas.extend_from_slice(&(name.len() as u32).to_le_bytes());
            atlas.extend_from_slice(&(image.len() as u32).to_le_bytes());
            atlas.extend_from_slice(name.as_bytes());
            atlas.extend_from_slice(&image);
        }
        let items = parse_atlas(&atlas).unwrap();
        assert_eq!(items.iter().map(|(n, _)| *n).collect::<Vec<_>>(), ["button_zc", "button_ja"]);
        assert_eq!(parse(items[1].1).unwrap().logical, [14, 10]);
        for cut in [0, 11, 12, 20, atlas.len() - 1] {
            assert!(parse_atlas(&atlas[..cut]).is_err());
        }
        atlas.extend_from_slice(&[0]);
        assert!(parse_atlas(&atlas).is_err());
    }

    #[test]
    fn rejects_invalid_dimensions_version_and_encoding() {
        for (offset, value) in [(4, 2u32), (8, 0), (16, 0), (16, 15), (20, 32769), (24, 99)] {
            let mut bytes = fixture();
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(parse(&bytes).is_err(), "accepted offset {offset}, value {value}");
        }
    }
}

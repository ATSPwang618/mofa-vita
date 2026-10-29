//! Strict borrowed view of one little-endian GXT v3 compressed texture.
//! This module does not decompress, transcode, allocate a payload, or validate GPU contents.

const HEADER_LEN: usize = 32;
const RECORD_LEN: usize = 32;
const METADATA_LEN: usize = HEADER_LEN + RECORD_LEN;
const MAX_FILE_LEN: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GxtFormat {
    Pvrtc4,
    Dxt1,
    Dxt5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GxtError {
    TooLarge,
    Truncated,
    InvalidHeader,
    InvalidOffset,
    UnsupportedFormat,
    UnsupportedLayout,
    InvalidDimensions,
    LengthMismatch,
}

impl std::fmt::Display for GxtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GXT v3: {self:?}")
    }
}

impl std::error::Error for GxtError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GxtTexture<'a> {
    pub width: u16,
    pub height: u16,
    pub backing_width: u16,
    pub backing_height: u16,
    pub texture_type: u32,
    pub format: GxtFormat,
    /// Exact compressed blocks, excluding GXT's checked zero alignment tail.
    pub data: &'a [u8],
}

impl<'a> GxtTexture<'a> {
    pub fn parse(file: &'a [u8]) -> Result<Self, GxtError> {
        if file.len() > MAX_FILE_LEN {
            return Err(GxtError::TooLarge);
        }
        if file.len() < METADATA_LEN {
            return Err(GxtError::Truncated);
        }
        let word = |at: usize| u32::from_le_bytes(file[at..at + 4].try_into().unwrap());
        if word(0) != 0x0054_5847 || word(4) != 0x1000_0003
            || word(8) != 1 || word(20) != 0 || word(24) != 0 || word(28) != 0
            || word(40) != u32::MAX || word(44) != 0 || file[61..64] != [0, 0, 0]
        {
            return Err(GxtError::InvalidHeader);
        }
        let data_offset = usize::try_from(word(12)).map_err(|_| GxtError::InvalidOffset)?;
        let data_size = usize::try_from(word(16)).map_err(|_| GxtError::InvalidOffset)?;
        if data_offset < METADATA_LEN || data_offset % 16 != 0
            || data_offset > MAX_FILE_LEN || data_size > MAX_FILE_LEN {
            return Err(GxtError::InvalidOffset);
        }
        if word(32) != word(12) || word(36) != word(16)
            || file[METADATA_LEN..data_offset.min(file.len())].iter().any(|&b| b != 0)
        {
            return Err(GxtError::InvalidHeader);
        }
        if data_offset > file.len() {
            return Err(GxtError::Truncated);
        }
        let data_end = data_offset.checked_add(data_size).ok_or(GxtError::InvalidOffset)?;
        if data_end > file.len() {
            return Err(GxtError::Truncated);
        }
        if data_end != file.len() {
            return Err(GxtError::LengthMismatch);
        }
        let width = u16::from_le_bytes(file[56..58].try_into().unwrap());
        let height = u16::from_le_bytes(file[58..60].try_into().unwrap());
        if width == 0 || height == 0 || width > 2048 || height > 2048 {
            return Err(GxtError::InvalidDimensions);
        }
        let texture_type = word(48);
        if !matches!(texture_type, 0 | 0xa000_0000)
            || (texture_type == 0 && (!width.is_power_of_two() || !height.is_power_of_two()))
            || file[60] != 1
        {
            return Err(GxtError::UnsupportedLayout);
        }
        let format = match word(52) {
            0x8100_0000 => GxtFormat::Pvrtc4,
            0x8500_0000 => GxtFormat::Dxt1,
            0x8700_0000 => GxtFormat::Dxt5,
            _ => return Err(GxtError::UnsupportedFormat),
        };
        let min = if format == GxtFormat::Pvrtc4 { 8u32 } else { 4u32 };
        let backing_width = u32::from(width).next_power_of_two().max(min);
        let backing_height = u32::from(height).next_power_of_two().max(min);
        let pixels = backing_width.checked_mul(backing_height).ok_or(GxtError::InvalidDimensions)?;
        let expected = match format {
            GxtFormat::Pvrtc4 | GxtFormat::Dxt1 => pixels / 2,
            GxtFormat::Dxt5 => pixels,
        };
        let payload_size = usize::try_from(expected).map_err(|_| GxtError::LengthMismatch)?;
        let storage_size = payload_size.checked_add(15).ok_or(GxtError::LengthMismatch)? & !15;
        if data_size != storage_size
            || file[data_offset + payload_size.min(data_size)..data_end].iter().any(|&b| b != 0)
        {
            return Err(GxtError::LengthMismatch);
        }
        Ok(Self {
            width,
            height,
            backing_width: backing_width as u16,
            backing_height: backing_height as u16,
            texture_type,
            format,
            data: &file[data_offset..data_offset + payload_size],
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    const PVRTC: u32 = 0x8100_0000;
    const DXT1: u32 = 0x8500_0000;
    const DXT5: u32 = 0x8700_0000;
    const ARBITRARY: u32 = 0xa000_0000;

    fn put32(b: &mut [u8], at: usize, n: u32) { b[at..at + 4].copy_from_slice(&n.to_le_bytes()); }
    fn put16(b: &mut [u8], at: usize, n: u16) { b[at..at + 2].copy_from_slice(&n.to_le_bytes()); }

    // GXT storage is 16-byte aligned even for one 8-byte DXT1 block. The
    // alignment tail is zero; only the true blocks may reach native upload.
    fn fixture(width: u16, height: u16, kind: u32, format: u32, offset: usize, payload: usize) -> Vec<u8> {
        let storage = (payload + 15) & !15;
        let mut b = vec![0u8; offset + storage];
        put32(&mut b, 0, 0x0054_5847);
        put32(&mut b, 4, 0x1000_0003);
        put32(&mut b, 8, 1);
        put32(&mut b, 12, offset as u32);
        put32(&mut b, 16, storage as u32);
        put32(&mut b, 32, offset as u32);
        put32(&mut b, 36, storage as u32);
        put32(&mut b, 40, u32::MAX);
        put32(&mut b, 48, kind);
        put32(&mut b, 52, format);
        put16(&mut b, 56, width);
        put16(&mut b, 58, height);
        b[60] = 1;
        b[offset..offset + payload].fill(0xa5);
        b
    }

    fn invalid(mut f: Vec<u8>, at: usize, n: u32) -> Vec<u8> { put32(&mut f, at, n); f }

    #[test]
    fn borrows_compressed_payload_with_minimum_pvrtc_backing_and_padding() {
        let bytes = fixture(1, 1, ARBITRARY, PVRTC, 80, 32);
        let g = GxtTexture::parse(&bytes).unwrap();
        assert_eq!((g.width, g.height, g.backing_width, g.backing_height), (1, 1, 8, 8));
        assert_eq!(g.format, GxtFormat::Pvrtc4);
        assert_eq!(g.texture_type, ARBITRARY);
        assert_eq!(g.data.len(), 32);
        assert!(std::ptr::eq(g.data.as_ptr(), bytes[80..].as_ptr()));
    }

    #[test]
    fn block_compressed_formats_have_distinct_exact_lengths() {
        let one = fixture(4, 4, 0, DXT1, 64, 8);
        let three = fixture(4, 4, 0, DXT5, 64, 16);
        assert_eq!((GxtTexture::parse(&one).unwrap().format, GxtTexture::parse(&one).unwrap().data.len()), (GxtFormat::Dxt1, 8));
        assert_eq!((GxtTexture::parse(&three).unwrap().format, GxtTexture::parse(&three).unwrap().data.len()), (GxtFormat::Dxt5, 16));
        assert_eq!(GxtTexture::parse(&fixture(4, 4, 0, DXT1, 64, 16)), Err(GxtError::LengthMismatch));
        let mut short_dxt5 = fixture(4, 4, 0, DXT5, 64, 8);
        short_dxt5.truncate(72);
        put32(&mut short_dxt5, 16, 8);
        put32(&mut short_dxt5, 36, 8);
        assert_eq!(GxtTexture::parse(&short_dxt5), Err(GxtError::LengthMismatch));
    }

    #[test]
    fn official_single_dxt1_block_excludes_only_the_canonical_zero_alignment_tail() {
        // Actual local psp2gxt 2.1 output: gxt-small-review/dxt1-4x4.gxt,
        // SHA-256 a7bbeba0a489169d2a77d51ec1c615bdb9f542cb4ba75bf66ed92a561d80dcfa.
        let mut file = fixture(4, 4, 0, DXT1, 64, 8);
        file[64..72].copy_from_slice(&[0x59, 0x5c, 0x59, 0x5c, 0, 0, 0, 0]);
        let g = GxtTexture::parse(&file).unwrap();
        assert_eq!(file.len(), 80);
        assert_eq!(g.data, &file[64..72]);
        assert!(std::ptr::eq(g.data.as_ptr(), file[64..].as_ptr()));
        let mut unaligned = file[..72].to_vec();
        put32(&mut unaligned, 16, 8);
        put32(&mut unaligned, 36, 8);
        assert_eq!(GxtTexture::parse(&unaligned), Err(GxtError::LengthMismatch));
        file[79] = 1;
        assert_eq!(GxtTexture::parse(&file), Err(GxtError::LengthMismatch));
    }

    #[test]
    fn npot_requires_arbitrary_layout_and_next_power_of_two_storage() {
        let bytes = fixture(9, 5, ARBITRARY, DXT5, 64, 128);
        let g = GxtTexture::parse(&bytes).unwrap();
        assert_eq!((g.width, g.height, g.backing_width, g.backing_height), (9, 5, 16, 8));
        assert_eq!(GxtTexture::parse(&fixture(9, 5, 0, DXT5, 64, 128)), Err(GxtError::UnsupportedLayout));
        assert_eq!(GxtTexture::parse(&fixture(8, 8, ARBITRARY, DXT1, 64, 32)).unwrap().backing_width, 8);
    }

    #[test]
    fn payload_must_be_exact_and_never_overlap_metadata_or_hide_trailing_bytes() {
        let mut f = fixture(4, 4, 0, DXT1, 64, 8);
        assert_eq!(GxtTexture::parse(&f[..63]), Err(GxtError::Truncated));
        assert_eq!(GxtTexture::parse(&f[..71]), Err(GxtError::Truncated));
        f.push(0);
        assert_eq!(GxtTexture::parse(&f), Err(GxtError::LengthMismatch));
        let f = fixture(4, 4, 0, DXT1, 80, 8);
        assert!(GxtTexture::parse(&f).is_ok());
        let mut f = f; f[72] = 1;
        assert_eq!(GxtTexture::parse(&f), Err(GxtError::InvalidHeader));
        assert_eq!(GxtTexture::parse(&invalid(fixture(4, 4, 0, DXT1, 64, 8), 12, 48)), Err(GxtError::InvalidOffset));
        assert_eq!(GxtTexture::parse(&invalid(fixture(4, 4, 0, DXT1, 64, 8), 12, 65)), Err(GxtError::InvalidOffset));
        assert_eq!(GxtTexture::parse(&invalid(fixture(4, 4, 0, DXT1, 64, 8), 12, 0xffff_fff0)), Err(GxtError::InvalidOffset));
    }

    #[test]
    fn header_record_and_all_reserved_fields_are_checked() {
        for (at, val) in [(0, 0), (4, 2), (8, 2), (20, 1), (24, 1), (28, 1), (40, 0), (44, 1), (61, 1)] {
            let mut b = fixture(4, 4, 0, DXT1, 64, 8);
            if at == 61 { b[at] = val as u8; } else { put32(&mut b, at, val); }
            assert_eq!(GxtTexture::parse(&b), Err(GxtError::InvalidHeader), "at {at}");
        }
        for (at, val) in [(32, 80), (36, 32)] {
            assert_eq!(GxtTexture::parse(&invalid(fixture(4, 4, 0, DXT1, 64, 8), at, val)), Err(GxtError::InvalidHeader));
        }
        let mut b = fixture(4, 4, 0, DXT1, 64, 8); b[60] = 2;
        assert_eq!(GxtTexture::parse(&b), Err(GxtError::UnsupportedLayout));
    }

    #[test]
    fn unknown_format_alpha_swizzle_type_dimensions_and_size_limit_fail_closed() {
        assert_eq!(GxtTexture::parse(&fixture(4, 4, 0, 0x8900_0000, 64, 8)), Err(GxtError::UnsupportedFormat));
        assert_eq!(GxtTexture::parse(&fixture(4, 4, 0, PVRTC | 1, 64, 8)), Err(GxtError::UnsupportedFormat));
        assert_eq!(GxtTexture::parse(&fixture(4, 4, 0, 0x8500_1000, 64, 8)), Err(GxtError::UnsupportedFormat));
        assert_eq!(GxtTexture::parse(&fixture(4, 4, 0x6000_0000, DXT1, 64, 8)), Err(GxtError::UnsupportedLayout));
        assert_eq!(GxtTexture::parse(&fixture(0, 4, 0, DXT1, 64, 8)), Err(GxtError::InvalidDimensions));
        assert_eq!(GxtTexture::parse(&fixture(2049, 4, ARBITRARY, DXT1, 64, 8)), Err(GxtError::InvalidDimensions));
        assert_eq!(GxtTexture::parse(&vec![0; 16 * 1024 * 1024 + 1]), Err(GxtError::TooLarge));
    }
    #[test]
    fn boundary_sized_payload_and_every_short_header_are_bounded() {
        let largest = fixture(2048, 2048, 0, DXT5, 64, 2048 * 2048);
        let g = GxtTexture::parse(&largest).unwrap();
        assert_eq!((g.backing_width, g.backing_height, g.data.len()), (2048, 2048, 4 * 1024 * 1024));
        let small = fixture(4, 4, 0, DXT1, 64, 8);
        for len in 0..64 {
            assert_eq!(GxtTexture::parse(&small[..len]), Err(GxtError::Truncated), "short header {len}");
        }
    }
}

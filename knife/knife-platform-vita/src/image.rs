//! Owned RGBA8 images decoded by the Vita native PNG/JPEG boundary.

use std::{marker::PhantomData, rc::Rc};

/// Approximate cross-thread snapshot of cumulative native counters. These
/// temporary CDRAM bytes are separate from GXM mappings and the process heap.
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct ImageDecodeStats {
    pub jpeg_hardware_candidates: u32,
    pub jpeg_hardware_ok: u32,
    pub jpeg_fallback_profile: u32,
    pub jpeg_fallback_busy: u32,
    pub jpeg_fallback_budget: u32,
    pub jpeg_fallback_init: u32,
    pub jpeg_fallback_alloc: u32,
    pub jpeg_fallback_driver: u32,
    pub jpeg_fallback_contract: u32,
    pub software_png_ok: u32,
    pub software_jpeg_ok: u32,
    pub hw_current_cdram: u32,
    pub hw_peak_cdram: u32,
    pub last_hw_error: u32,
    pub last_contract_mask: u32,
    pub last_info_size: u32,
    pub last_info_y_pitch: u32,
    pub last_info_chroma_pitch: u32,
    pub last_info_color_space: u32,
}
pub fn decode_stats() -> ImageDecodeStats {
    unsafe extern "C" { fn kv_image_decode_stats(out: *mut ImageDecodeStats); }
    let mut stats = ImageDecodeStats::default();
    // SAFETY: exclusively borrowed repr(C) structure matches the 19 u32 C fields.
    unsafe { kv_image_decode_stats(&mut stats) };
    stats
}

/// C decoder errors retain their exact signed status; Rust contract failures are separate.
#[derive(Debug, PartialEq, Eq)]
pub enum ImageDecodeError {
    EmptyInput,
    InputLimit { length: usize },
    Native(i32),
    InvalidOutput(&'static str),
}

impl std::fmt::Display for ImageDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyInput => f.write_str("empty PNG/JPEG input"),
            Self::InputLimit { length } => write!(
                f,
                "encoded image length {length} exceeds {IMAGE_ENCODED_LIMIT} bytes"
            ),
            Self::InvalidOutput(reason) => write!(f, "native image contract: {reason}"),
            Self::Native(code) => {
                let reason = match code {
                    -1 => "invalid argument",
                    -2 => "encoded input limit",
                    -3 => "expected PNG or JPEG",
                    -4 => "invalid image dimensions",
                    -5 => "malformed or truncated image",
                    -6 => "allocation failed",
                    -7 => "unsupported image features",
                    -8 => "decoder working budget exhausted",
                    _ => "unknown native status",
                };
                write!(f, "PNG/JPEG decoder {code}: {reason}")
            }
        }
    }
}

impl std::error::Error for ImageDecodeError {}

pub const IMAGE_ENCODED_LIMIT: u32 = 32 * 1024 * 1024;
pub const IMAGE_WIDTH_LIMIT: u32 = 960;
pub const IMAGE_HEIGHT_LIMIT: u32 = 1080;

#[repr(C)]
struct RawDecodedImage {
    width: u32,
    height: u32,
    stride: u32,
    length: u32,
    pixels: *mut u8,
}

impl Default for RawDecodedImage {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            stride: 0,
            length: 0,
            pixels: std::ptr::null_mut(),
        }
    }
}

// The C implementation checks this ABI when compiled by VitaSDK. All lengths crossing
// this boundary are u32; the only pointer is the C allocation at offset 16.
#[cfg(target_os = "vita")]
const _: () = {
    assert!(std::mem::size_of::<RawDecodedImage>() == 20);
    assert!(std::mem::align_of::<RawDecodedImage>() == 4);
    assert!(std::mem::offset_of!(RawDecodedImage, pixels) == 16);
    assert!(usize::BITS == 32);
};

unsafe extern "C" {
    fn kv_image_decode(encoded: *const u8, length: u32, out: *mut RawDecodedImage) -> i32;
    fn kv_image_decode_bounded(encoded: *const u8, length: u32, out: *mut RawDecodedImage, max_rgba: u32) -> i32;
    fn kv_image_free(image: *mut RawDecodedImage);
}

fn validated_output_length(raw: &RawDecodedImage) -> Result<usize, ImageDecodeError> {
    let invalid = ImageDecodeError::InvalidOutput;
    if raw.pixels.is_null() {
        return Err(invalid("missing pixel allocation"));
    }
    if raw.width == 0
        || raw.width > IMAGE_WIDTH_LIMIT
        || raw.height == 0
        || raw.height > IMAGE_HEIGHT_LIMIT
    {
        return Err(invalid("dimensions exceed the 960x1080 image boundary"));
    }
    let stride = raw
        .width
        .checked_mul(4)
        .ok_or(invalid("RGBA stride overflow"))?;
    let length = stride
        .checked_mul(raw.height)
        .ok_or(invalid("RGBA length overflow"))?;
    if raw.stride != stride || raw.length != length {
        return Err(invalid("expected tightly packed RGBA8 pixels"));
    }
    let length =
        usize::try_from(length).map_err(|_| invalid("RGBA length exceeds address space"))?;
    if length > isize::MAX as usize || (raw.pixels as usize).checked_add(length).is_none() {
        return Err(invalid("pixel slice exceeds pointer range"));
    }
    Ok(length)
}

/// Owns one immutable, tightly packed, straight-alpha RGBA8 allocation from C.
///
/// The encoded input is borrowed only during `decode`. Pixels are borrowed from
/// this owner and released only by `kv_image_free`, including on a failed native
/// status or an inconsistent successful result. No C pointer enters a Rust Vec.
/// The owner stays on its creating thread (`!Send` and `!Sync`).
pub struct DecodedImage {
    raw: RawDecodedImage,
    // Keep ownership local even if a future refactor changes the pointer field.
    _thread_bound: PhantomData<Rc<()>>,
}

impl DecodedImage {
    pub fn decode(encoded: &[u8]) -> Result<Self, ImageDecodeError> {
        Self::decode_inner(encoded, None)
    }

    pub fn decode_bounded(encoded: &[u8], max_rgba: u32) -> Result<Self, ImageDecodeError> {
        Self::decode_inner(encoded, Some(max_rgba))
    }

    fn decode_inner(encoded: &[u8], max_rgba: Option<u32>) -> Result<Self, ImageDecodeError> {
        let length = checked_input_length(encoded.len())?;
        let mut image = Self {
            raw: RawDecodedImage::default(),
            _thread_bound: PhantomData,
        };
        // SAFETY: input is nonempty, its u32 length is checked and bounded, and it
        // remains alive for this synchronous call. Output is aligned, zeroed and
        // uniquely borrowed. C never retains the encoded input pointer.
        let status = unsafe {
            match max_rgba {
                Some(cap) => kv_image_decode_bounded(encoded.as_ptr(), length, &mut image.raw, cap),
                None => kv_image_decode(encoded.as_ptr(), length, &mut image.raw),
            }
        };
        if status != 0 {
            return Err(ImageDecodeError::Native(status));
        }
        validated_output_length(&image.raw)?;
        Ok(image)
    }

    pub fn width(&self) -> u32 {
        self.raw.width
    }
    pub fn height(&self) -> u32 {
        self.raw.height
    }
    pub fn stride(&self) -> u32 {
        self.raw.stride
    }
    pub fn byte_len(&self) -> u32 {
        self.raw.length
    }

    pub fn pixels(&self) -> &[u8] {
        // SAFETY: decode admitted only a non-null, fully initialized C allocation
        // of this validated byte length. Raw fields are private and immutable,
        // and the borrowed slice cannot outlive this owner or its C destructor.
        unsafe { std::slice::from_raw_parts(self.raw.pixels, self.raw.length as usize) }
    }
}

impl Drop for DecodedImage {
    fn drop(&mut self) {
        // SAFETY: this handle was zero initialized and belongs exclusively to
        // this owner. C accepts empty failure outputs and clears every field.
        unsafe { kv_image_free(&mut self.raw) };
    }
}

pub(crate) fn checked_input_length(length: usize) -> Result<u32, ImageDecodeError> {
    if length == 0 {
        return Err(ImageDecodeError::EmptyInput);
    }
    if length > IMAGE_ENCODED_LIMIT as usize {
        return Err(ImageDecodeError::InputLimit { length });
    }
    u32::try_from(length).map_err(|_| ImageDecodeError::InputLimit { length })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> RawDecodedImage {
        RawDecodedImage {
            width: 960,
            height: 1080,
            stride: 3840,
            length: 4_147_200,
            pixels: std::ptr::NonNull::<u8>::dangling().as_ptr(),
        }
    }

    #[test]
    fn maximum_language_strip_and_canvas_layouts_are_admitted() {
        let mut raw = shape();
        assert_eq!(validated_output_length(&raw), Ok(4_147_200));
        raw.height = 540;
        raw.length = 2_073_600;
        assert_eq!(validated_output_length(&raw), Ok(2_073_600));
    }

    #[test]
    fn inconsistent_native_metadata_is_rejected_before_forming_a_slice() {
        for mutation in 0..8 {
            let mut raw = shape();
            match mutation {
                0 => raw.pixels = std::ptr::null_mut(),
                1 => raw.width = 0,
                2 => raw.height = 0,
                3 => raw.width = 961,
                4 => raw.height = 1081,
                5 => raw.stride -= 1,
                6 => raw.length -= 1,
                7 => raw.pixels = usize::MAX as *mut u8,
                _ => unreachable!(),
            }
            assert!(
                validated_output_length(&raw).is_err(),
                "accepted mutation {mutation}"
            );
        }
    }

    #[test]
    fn raw_fields_follow_the_c_header_on_each_pointer_width() {
        assert_eq!(std::mem::offset_of!(RawDecodedImage, width), 0);
        assert_eq!(std::mem::offset_of!(RawDecodedImage, height), 4);
        assert_eq!(std::mem::offset_of!(RawDecodedImage, stride), 8);
        assert_eq!(std::mem::offset_of!(RawDecodedImage, length), 12);
        assert_eq!(std::mem::offset_of!(RawDecodedImage, pixels), 16);
        assert_eq!(
            std::mem::size_of::<RawDecodedImage>(),
            16 + std::mem::size_of::<*mut u8>()
        );
    }
}

//! Vita platform boundary. Host tests cover validation; SDK execution is Vita-only.

pub mod storage;
pub mod asset_check;
pub mod boot_error;
pub mod gxt;
pub mod movie_geometry;
pub mod movie_view;
pub mod image_worker;
pub mod frame_timing;
pub mod perf_switch;
pub mod console_codec;
#[cfg(target_os = "vita")]
pub mod audit;

#[cfg(target_os = "vita")]
mod native;
#[cfg(target_os = "vita")]
pub use native::*;
#[cfg(target_os = "vita")]
pub mod image;
#[cfg(target_os = "vita")]
mod image_probe;
#[cfg(target_os = "vita")]
pub use image_probe::run_image_probe;
#[cfg(target_os = "vita")]
pub mod gxm;
#[cfg(target_os = "vita")]
pub mod audio_decode;
#[cfg(target_os = "vita")]
pub mod audio_output;
#[cfg(target_os = "vita")]
pub mod movie;
#[cfg(target_os = "vita")]
mod audio_probe;
#[cfg(target_os = "vita")]
pub use audio_probe::run_audio_probe;
#[cfg(target_os = "vita")]
mod gxm_probe;
#[cfg(target_os = "vita")]
pub use gxm_probe::run_gxm_probe;
#[cfg(target_os = "vita")]
mod effects_probe;
#[cfg(target_os = "vita")]
pub use effects_probe::run_effects_probe;
#[cfg(target_os = "vita")]
mod compressed_probe;
#[cfg(target_os = "vita")]
pub use compressed_probe::run_compressed_probe;
#[cfg(target_os = "vita")]
mod pipeline_probe;
#[cfg(target_os = "vita")]
pub use pipeline_probe::run_pipeline_probe;

pub const DATA_ROOT: &str = "ux0:data/knife-woh-dev";

/// Only relative, normalized components may be appended to a writable root.
pub fn data_path(relative: &str) -> Result<String, &'static str> {
    if relative.contains(['\\', ':', '\0'])
        || relative.split('/').any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("expected a normalized relative data path");
    }
    Ok(format!("{DATA_ROOT}/{relative}"))
}

/// C allocates this buffer; Rust only borrows it and C releases it.
#[repr(C)]
pub struct IoRequest {
    pub offset: u64,
    pub buffer: *mut u8,
    pub length: u32,
    pub actual: u32,
    pub status: i32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub struct AbiLayout {
    pub version: u32,
    pub pointer_bits: u32,
    pub size: u32,
    pub alignment: u32,
    pub offsets: [u32; 5],
}

impl AbiLayout {
    pub fn rust() -> Self {
        Self {
            version: 1,
            pointer_bits: usize::BITS,
            size: std::mem::size_of::<IoRequest>() as u32,
            alignment: std::mem::align_of::<IoRequest>() as u32,
            offsets: [
                std::mem::offset_of!(IoRequest, offset) as u32,
                std::mem::offset_of!(IoRequest, buffer) as u32,
                std::mem::offset_of!(IoRequest, length) as u32,
                std::mem::offset_of!(IoRequest, actual) as u32,
                std::mem::offset_of!(IoRequest, status) as u32,
            ],
        }
    }

    pub fn validate(&self, native: &Self) -> Result<(), String> {
        macro_rules! compare {
            ($($field:ident),+) => {$(
                if self.$field != native.$field {
                    return Err(format!("ABI {}: Rust {:?}, C {:?}", stringify!($field), self.$field, native.$field));
                }
            )+};
        }
        compare!(version, pointer_bits, size, alignment, offsets);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_confined_to_the_selected_data_directory() {
        assert_eq!(data_path("saves/slot0.dat").unwrap(), "ux0:data/knife-woh-dev/saves/slot0.dat");
        for input in ["", "/saves", "../save", "saves/../save", "ux0:data/elsewhere", "a\\b", "a\0b", "a//b", "./save"] {
            assert!(data_path(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn abi_comparison_rejects_the_exact_incompatible_field() {
        let expected = AbiLayout::rust();
        assert!(expected.validate(&expected).is_ok());
        let mut wrong = expected;
        wrong.offsets[2] += 4;
        assert!(expected.validate(&wrong).unwrap_err().contains("offsets"));
        wrong = expected;
        wrong.pointer_bits = 64 + expected.pointer_bits;
        assert!(expected.validate(&wrong).unwrap_err().contains("pointer_bits"));
    }

    #[test]
    fn io_requests_have_fixed_width_fields_and_native_pointer_alignment() {
        let layout = AbiLayout::rust();
        assert_eq!(layout.offsets[0], 0);
        assert_eq!(layout.offsets[1], 8);
        assert_eq!(layout.offsets[3] - layout.offsets[2], 4);
        assert_eq!(layout.offsets[4] - layout.offsets[3], 4);
        assert_eq!(layout.size % layout.alignment, 0);
    }
}

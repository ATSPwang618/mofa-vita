//! Compile this standalone harness with rustc and the actual native decoder.
//! The platform crate exports the wrapper only for Vita; no host codec is shipped.
#[path = "../../knife-platform-vita/src/image.rs"]
mod image;
#[path = "../../knife-platform-vita/src/image_probe.rs"]
mod image_probe;

use image::{DecodedImage, IMAGE_ENCODED_LIMIT, ImageDecodeError};

const PNG: &[u8] = include_bytes!("../../knife-platform-vita/src/image_fixtures/alpha.png");
const JPEG: &[u8] = include_bytes!("../../knife-platform-vita/src/image_fixtures/colors.jpg");
const RGBA: &[u8] = &[
    255, 17, 0, 0, 0, 255, 31, 64, 7, 11, 255, 255, 89, 45, 12, 127,
];

unsafe extern "C" {
    fn kv_test_free_calls() -> u32;
    fn kv_test_uncleared_frees() -> u32;
}

fn frees() -> u32 {
    // SAFETY: host-only instrumentation returns a scalar and borrows nothing.
    unsafe { kv_test_free_calls() }
}

#[test]
fn png_pixels_keep_straight_alpha_and_outlive_the_encoded_input() {
    let owned = {
        let borrowed_only_during_decode = PNG.to_vec();
        DecodedImage::decode(&borrowed_only_during_decode).unwrap()
    };
    assert_eq!((owned.width(), owned.height(), owned.stride()), (2, 2, 8));
    assert_eq!(owned.pixels(), RGBA);
    let before = frees();
    drop(owned);
    assert_eq!(frees(), before + 1, "Drop must call the C allocator's free");
}

#[test]
fn jpeg_pixels_have_the_expected_color_order_and_opaque_alpha() {
    let image = DecodedImage::decode(JPEG).unwrap();
    assert_eq!((image.width(), image.height(), image.stride()), (16, 8, 64));
    assert_eq!(image.pixels().len(), 16 * 8 * 4);
    assert_eq!(image.byte_len(), 16 * 8 * 4);
    for (index, pixel) in image.pixels().chunks_exact(4).enumerate() {
        let expected = if index % 16 < 8 {
            [240_u8, 20, 10]
        } else {
            [15, 225, 35]
        };
        for component in 0..3 {
            assert!(pixel[component].abs_diff(expected[component]) <= 3);
        }
        assert_eq!(pixel[3], 255);
    }
}

#[test]
fn invalid_input_fails_without_transferring_or_leaking_ownership() {
    let before = frees();
    assert!(matches!(
        DecodedImage::decode(&[]),
        Err(ImageDecodeError::EmptyInput)
    ));
    assert_eq!(frees(), before, "empty input must not enter C");
    assert!(matches!(
        DecodedImage::decode(b"not an image"),
        Err(ImageDecodeError::Native(-3))
    ));
    assert_eq!(
        frees(),
        before + 1,
        "failed C output is still disposed safely"
    );
    assert!(matches!(
        DecodedImage::decode(&PNG[..PNG.len() - 1]),
        Err(ImageDecodeError::Native(-5))
    ));
    assert!(matches!(
        DecodedImage::decode(&JPEG[..JPEG.len() - 1]),
        Err(ImageDecodeError::Native(-5))
    ));
    assert_eq!(frees(), before + 3);
}

#[test]
fn every_truncated_fixture_is_rejected_and_the_empty_handle_is_freed() {
    for encoded in [PNG, JPEG] {
        for length in 1..encoded.len() {
            let before = frees();
            assert!(
                DecodedImage::decode(&encoded[..length]).is_err(),
                "accepted {length} byte prefix"
            );
            assert_eq!(frees(), before + 1);
        }
    }
}

#[test]
fn repeated_rust_lifetimes_release_exactly_one_c_buffer_each() {
    let before = frees();
    for _ in 0..128 {
        for encoded in [PNG, JPEG] {
            let image = DecodedImage::decode(encoded).unwrap();
            assert!(!image.pixels().is_empty());
            drop(image);
        }
    }
    assert_eq!(frees(), before + 256);
    // SAFETY: host-only instrumentation observes the fields after C free.
    assert_eq!(unsafe { kv_test_uncleared_frees() }, 0);
}

#[test]
fn encoded_lengths_are_checked_before_a_slice_or_c_length_is_created() {
    assert_eq!(
        image::checked_input_length(0),
        Err(ImageDecodeError::EmptyInput)
    );
    assert_eq!(image::checked_input_length(1), Ok(1));
    assert_eq!(
        image::checked_input_length(IMAGE_ENCODED_LIMIT as usize),
        Ok(IMAGE_ENCODED_LIMIT)
    );
    for length in [
        IMAGE_ENCODED_LIMIT as usize + 1,
        u32::MAX as usize,
        usize::MAX,
    ] {
        assert_eq!(
            image::checked_input_length(length),
            Err(ImageDecodeError::InputLimit { length })
        );
    }
}

#[test]
fn the_graphics_independent_probe_checks_the_real_decoder_and_records_its_scope() {
    let mut log = Vec::new();
    image_probe::run_image_probe(&mut log).unwrap();
    let text = String::from_utf8(log).unwrap();
    print!("{text}");
    for expected in [
        "PNG straight-alpha",
        "JPEG RGB",
        "bad headers and truncated",
        "32 PNG/JPEG lifetimes",
    ] {
        assert!(text.contains(expected), "missing check: {expected}; {text}");
    }
}

#[test]
fn exclusive_image_owner_moves_without_copy_and_stays_not_sync() {
    let mut owner = DecodedImage::decode(PNG).unwrap();
    let address = owner.pixels().as_ptr() as usize;
    owner.pixels_mut()[0] = 19;
    let before = frees();
    let thread = std::thread::spawn(move || {
        assert_eq!(owner.pixels().as_ptr() as usize, address);
        assert_eq!(owner.pixels()[0], 19);
        assert_eq!(owner.bytes_per_pixel(), 4);
        drop(owner);
    });
    thread.join().unwrap();
    assert_eq!(frees(), before + 1);

    trait AmbiguousIfSync<A> {
        fn check() {}
    }
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
    let _ = <DecodedImage as AmbiguousIfSync<_>>::check;
}

#[test]
fn freezing_consumes_exclusive_owner_and_frees_only_after_last_shared_reader() {
    let mut owner=DecodedImage::decode(PNG).unwrap();
    owner.pixels_mut()[0]=42;
    let address=owner.pixels().as_ptr() as usize;
    let frozen=std::sync::Arc::new(owner.freeze());
    let before=frees();
    let readers:Vec<_>=(0..4).map(|_|{
        let frozen=frozen.clone();std::thread::spawn(move||{
            assert_eq!(frozen.as_ref().as_ref().as_ptr() as usize,address);
            assert_eq!(frozen.as_ref().as_ref()[0],42);
        })
    }).collect();
    for reader in readers {reader.join().unwrap();}
    assert_eq!(frees(),before,"last shared cache pin still owns the C buffer");
    drop(frozen);assert_eq!(frees(),before+1);
}

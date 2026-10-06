#[path = "../src/video_frame_layout.rs"]
mod layout;
use layout::{FrameLayout, GRANULARITY};

#[test]
fn observed_two_megabyte_request_uses_native_granularity() {
    for alignment in [0, 1, 16, 256, 4096, GRANULARITY] {
        let layout = FrameLayout::new(alignment, 1_566_720).unwrap();
        assert_eq!(layout.bytes, 2 * GRANULARITY);
        assert_eq!(layout.offset(0x8010_0000), Some(0));
    }
}

#[test]
fn stronger_alignment_fits_every_native_block_position() {
    for alignment in [2 * GRANULARITY, 4 * GRANULARITY, 8 * GRANULARITY] {
        for payload in [1, GRANULARITY - 1, GRANULARITY, GRANULARITY + 1] {
            let layout = FrameLayout::new(alignment, payload).unwrap();
            for step in 0..16 {
                let base = 0x8000_0000 + step * GRANULARITY as usize;
                let offset = layout.offset(base).unwrap();
                assert_eq!((base + offset) % alignment as usize, 0);
                assert!(offset + payload as usize <= layout.bytes as usize);
                assert_eq!(layout.bytes % GRANULARITY, 0);
            }
        }
    }
}

#[test]
fn invalid_requests_and_address_overflow_are_rejected() {
    for (alignment, size) in [(3, 1), (0x100001, 1), (0, 0), (1, u32::MAX), (1 << 31, 1)] {
        assert!(FrameLayout::new(alignment, size).is_err());
    }
    let layout = FrameLayout::new(4096, GRANULARITY).unwrap();
    assert_eq!(layout.offset(0), None);
    assert_eq!(layout.offset(0x8000_0001), None);
    assert_eq!(
        layout.offset(usize::MAX & !(GRANULARITY as usize - 1)),
        None
    );
}

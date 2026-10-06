use krkr_image::scale;
use krkr_protocol::{budget::Budget, graphics::Size, pixels::Bytes};
use std::sync::atomic::AtomicBool;

#[test]
fn expansion_matches_integer_sampling_for_thin_odd_and_repeated_rows() {
    let budget = Budget::new(16 * 1024 * 1024);
    let stop = AtomicBool::new(false);
    for (w, h) in [(1, 1), (1, 9), (7, 3), (13, 11), (960, 544)] {
        let stored = Size {
            width: w,
            height: h,
        };
        let mut source = Bytes::zeroed(stored.rgba_bytes().unwrap(), &budget).unwrap();
        for (i, byte) in source.as_mut_slice().iter_mut().enumerate() {
            *byte = (i.wrapping_mul(71) >> 3) as u8;
        }
        for (w, h) in [(1, 1), (2, 2), (7, 4), (17, 9), (1280, 720)] {
            let logical = Size {
                width: w,
                height: h,
            };
            let result = scale::expand(&source, stored, logical, &budget, &stop).unwrap();
            for y in 0..h {
                for x in 0..w {
                    let sx = (u64::from(2 * x + 1) * u64::from(stored.width) / u64::from(2 * w))
                        as usize;
                    let sy = (u64::from(2 * y + 1) * u64::from(stored.height) / u64::from(2 * h))
                        as usize;
                    let src = (sy * stored.width as usize + sx) * 4;
                    let dst = (y as usize * w as usize + x as usize) * 4;
                    assert_eq!(
                        &result.as_slice()[dst..dst + 4],
                        &source.as_slice()[src..src + 4]
                    );
                }
            }
        }
        assert!(scale::expand(&source, stored, stored, &budget, &AtomicBool::new(true)).is_err());
    }
    assert_eq!(budget.used(), 0);
}

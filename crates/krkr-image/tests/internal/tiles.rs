use super::*;

#[test]
fn tiling_matches_modulo_sampling_across_clipped_and_repeated_periods() {
    let budget = Budget::new(1024 * 1024);
    let stop = AtomicBool::new(false);
    for (sw, sh) in [(1, 1), (3, 2), (17, 7)] {
        for (dw, dh) in [(1, 1), (2, 3), (31, 19), (960, 544)] {
            let mut source = Bytes::zeroed(sw * sh, &budget).unwrap();
            for (i, byte) in source.as_mut_slice().iter_mut().enumerate() {
                *byte = (i * 71) as u8;
            }
            let expected: Vec<_> = (0..dh)
                .flat_map(|y| (0..dw).map(move |x| (((y % sh) * sw + x % sw) * 71) as u8))
                .collect();
            let result = tile(
                source,
                Size {
                    width: sw as u32,
                    height: sh as u32,
                },
                Size {
                    width: dw as u32,
                    height: dh as u32,
                },
                &budget,
                &stop,
            )
            .unwrap();
            assert_eq!(result.as_slice(), expected);
        }
    }
    let source = Bytes::zeroed(1, &budget).unwrap();
    assert!(
        tile(
            source,
            Size {
                width: 1,
                height: 1
            },
            Size {
                width: 9,
                height: 7
            },
            &budget,
            &AtomicBool::new(true)
        )
        .is_err()
    );
    assert_eq!(budget.used(), 0);
    assert!(
        tile(
            Bytes::zeroed(0, &budget).unwrap(),
            Size {
                width: 0,
                height: 1
            },
            Size {
                width: 3,
                height: 1
            },
            &budget,
            &stop,
        )
        .is_err()
    );
}

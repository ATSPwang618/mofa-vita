use super::*;

#[test]
fn bmp32_rows_preserve_alpha_and_luminance_in_both_orientations() {
    let size = Size {
        width: 5,
        height: 3,
    };
    let stop = AtomicBool::new(false);
    let budget = Budget::new(4096);
    let raw: Vec<u8> = (0..15u8)
        .flat_map(|n| [n * 13, n * 11, n * 7, n * 17])
        .collect();
    for dib in [false, true] {
        for bottom_up in [false, true] {
            let offset = if dib { 0 } else { 14 };
            let mut bytes = vec![0; offset + 40];
            if !dib {
                bytes[..2].copy_from_slice(b"BM");
                bytes[10..14].copy_from_slice(&54u32.to_le_bytes());
            }
            bytes[offset..offset + 4].copy_from_slice(&40u32.to_le_bytes());
            bytes[offset + 8..offset + 12]
                .copy_from_slice(&(if bottom_up { 3i32 } else { -3i32 }).to_le_bytes());
            bytes[offset + 14..offset + 16].copy_from_slice(&32u16.to_le_bytes());
            bytes.extend_from_slice(&raw);
            for mode in [Mode::Main, Mode::Mask] {
                let decoded = decode(&bytes, size, mode, &budget, &stop).unwrap().unwrap();
                let mut expected = Vec::new();
                for y in 0..3 {
                    let source_y = if bottom_up { 2 - y } else { y };
                    for p in raw[source_y * 20..(source_y + 1) * 20].as_chunks::<4>().0 {
                        if mode == Mode::Main {
                            expected.extend([p[2], p[1], p[0], p[3]]);
                        } else {
                            expected.push(transform::gray(p[2], p[1], p[0]));
                        }
                    }
                }
                assert_eq!(decoded.as_slice(), expected);
            }
            assert!(decode(&bytes, size, Mode::Main, &budget, &AtomicBool::new(true)).is_err());
            bytes.pop();
            assert!(decode(&bytes, size, Mode::Main, &budget, &stop).is_err());
        }
    }
    assert_eq!(budget.used(), 0);
}

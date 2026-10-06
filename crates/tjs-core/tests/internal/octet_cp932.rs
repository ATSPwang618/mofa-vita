use super::*;

fn legacy(bytes: &[u8]) -> NativeResult<Vec<u16>> {
    let bytes: Vec<_> = bytes.iter().copied().filter(|&b| b != 0).collect();
    let invalid = || NativeError::Message("invalid CP932 string");
    let mut pos = 0;
    while let Some(&byte) = bytes.get(pos) {
        pos += match byte {
            0x01..=0x7f | 0xa1..=0xdf => 1,
            0x81..=0x9f | 0xe0..=0xef | 0xfa..=0xfc => 2,
            _ => return Err(invalid()),
        };
    }
    let text = encoding_rs::SHIFT_JIS
        .decode_without_bom_handling_and_without_replacement(&bytes)
        .ok_or_else(invalid)?;
    Ok(text.encode_utf16().collect())
}

#[test]
fn direct_cp932_preserves_all_byte_pairs_and_zero_removal() {
    for a in 0..=255u8 {
        for b in 0..=255u8 {
            for input in [&[a, b][..], &[a, 0, b, 0][..]] {
                let expected = legacy(input);
                let actual = decode_text(input);
                assert_eq!(actual.is_ok(), expected.is_ok(), "{input:?}");
                if let (Ok(actual), Ok(expected)) = (actual, expected) {
                    assert_eq!(actual, expected, "{input:?}");
                }
            }
        }
    }
}

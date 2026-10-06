use krkr_assets::{Error, name::units, text};

#[test]
fn text_containers_preserve_all_code_units_and_length_fields() {
    let input: Vec<u16> = (0..=u16::MAX).collect();
    for mode in ["", "c1", "z0", "z1", "z9"] {
        let output = text::encode(&input, &units(mode), 1024 * 1024).unwrap();
        assert_eq!(
            text::decode(&output, &units("utf-8"), 1024 * 1024).unwrap(),
            input
        );
        if mode.starts_with('z') {
            assert_eq!(&output[..5], &[254, 254, 2, 255, 254]);
            assert_eq!(
                u64::from_le_bytes(output[5..13].try_into().unwrap()),
                output.len() as u64 - 21
            );
            assert_eq!(
                u64::from_le_bytes(output[13..21].try_into().unwrap()),
                input.len() as u64 * 2
            );
        }
    }
    assert_eq!(
        text::encode(&[0x1234, 0xd800, 0], &[], 27).unwrap(),
        [255, 254, 0x34, 0x12, 0, 0xd8, 0, 0]
    );
    assert!(matches!(
        text::encode(&[0; 4], &units("z1"), 28),
        Err(Error::Limit("text bytes"))
    ));
    assert!(text::encode(&[0xd800], &units("UTF-8"), 32).is_err());
    for mode in ["", "c1", "z0", "z9"] {
        let empty = text::encode(&[], &units(mode), 128).unwrap();
        assert_eq!(text::decode(&empty, &[], 128).unwrap(), []);
    }
}

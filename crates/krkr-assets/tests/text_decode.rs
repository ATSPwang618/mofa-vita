use krkr_assets::{Error, name::units, text};

#[test]
fn direct_text_decode_matches_encoding_rs_and_keeps_error_precedence() {
    for encoding in [
        encoding_rs::UTF_8,
        encoding_rs::SHIFT_JIS,
        encoding_rs::WINDOWS_1252,
        encoding_rs::GBK,
        encoding_rs::ISO_2022_JP,
    ] {
        let input = if encoding == encoding_rs::WINDOWS_1252 {
            "café, Straße\r\n"
        } else {
            "日本語\r\nmenu文字\0末尾"
        };
        let (bytes, _, malformed) = encoding.encode(input);
        assert!(!malformed);
        let expected: Vec<_> = input.encode_utf16().collect();
        assert_eq!(
            text::decode(&bytes, &units(encoding.name()), 4096).unwrap(),
            expected
        );
        let exact = bytes.len().max(expected.len() * 2);
        assert_eq!(
            text::decode(&bytes, &units(encoding.name()), exact).unwrap(),
            expected
        );
    }
    let bytes = [vec![b'a'; 8192], vec![255]].concat();
    assert!(matches!(
        text::decode(&bytes, &units("utf-8"), bytes.len()),
        Err(Error::Format("invalid text encoding"))
    ));
    assert!(matches!(
        text::decode(&[b'a'; 8192], &units("utf-8"), 8192),
        Err(Error::Limit("decoded text"))
    ));
    assert_eq!(text::decode(b"", &units("utf-8"), 0).unwrap(), []);
    assert_eq!(
        text::decode(b"\xef\xbb\xbfabc", &units("unknown"), 6).unwrap(),
        units("abc")
    );
    assert!(text::decode(&[0x81], &units("shift-jis"), 32).is_err());
    assert_eq!(
        text::decode(&[0xff, 0xfe, 0, 0xd8], &units("utf-8"), 4).unwrap(),
        [0xd800]
    );
}

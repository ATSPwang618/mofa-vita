use super::*;

fn with_chunks(mut image: Vec<u8>, extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
    let end = image.split_off(image.len() - 12);
    for (kind, data) in extra {
        chunk(&mut image, kind, data);
    }
    image.extend(end);
    image
}
fn zip(data: &[u8]) -> Vec<u8> {
    let mut stream = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    stream.write_all(data).unwrap();
    stream.finish().unwrap()
}
fn itxt(key: &[u8], text: &[u8], compressed: bool) -> Vec<u8> {
    let mut out = key.to_vec();
    out.extend([0, u8::from(compressed), 0]);
    out.extend(b"ja\0");
    out.extend("翻訳\0".as_bytes());
    out.extend(if compressed { zip(text) } else { text.to_vec() });
    out
}
fn size() -> Size {
    Size {
        width: 7,
        height: 5,
    }
}

#[test]
fn png_text_variants_restore_atlas_tags_without_changing_pixels_or_planes() {
    let budget = Budget::new(8 * 1024 * 1024);
    let cancel = AtomicBool::new(false);
    let chunks = vec![
        (b"tEXt", b"mode\0alpha".to_vec()),
        (
            b"zTXt",
            [b"divideArea\0\0".as_slice(), &zip(b"0,0,4,3/4,0,3,5")].concat(),
        ),
        (b"iTXt", itxt(b"note", "透明・図集".as_bytes(), true)),
        (b"iTXt", itxt(b"note2", "無圧縮".as_bytes(), false)),
        (b"tEXt", b"Cr\xe9ateur\0Andr\xe9".to_vec()),
    ];
    for color in [
        png::ColorType::Indexed,
        png::ColorType::Grayscale,
        png::ColorType::Rgba,
    ] {
        let original = fixture(size(), color, 8, true);
        let restored = with_chunks(original.clone(), &chunks);
        for mode in [Mode::Main, Mode::Mask, Mode::Province] {
            let before = decode(&original, size(), mode, 0x02ffffff, &budget, &cancel);
            let after = decode(&restored, size(), mode, 0x02ffffff, &budget, &cancel);
            if color == png::ColorType::Rgba && mode == Mode::Province {
                assert!(before.is_err() && after.is_err());
                continue;
            }
            let (before, _) = before.unwrap();
            let (after, tags) = after.unwrap();
            assert_eq!(before.as_slice(), after.as_slice());
            if mode == Mode::Main {
                for pair in [
                    ("mode", "alpha"),
                    ("divideArea", "0,0,4,3/4,0,3,5"),
                    ("note", "透明・図集"),
                    ("note2", "無圧縮"),
                    ("Créateur", "André"),
                ] {
                    assert!(tags.contains(&(pair.0.into(), pair.1.into())));
                }
            } else {
                assert!(tags.is_empty());
            }
        }
        assert_eq!(budget.used(), 0);
    }
}
#[test]
fn duplicate_text_uses_last_value_and_cannot_override_native_coordinate_chunks() {
    let data = with_chunks(
        fixture(size(), png::ColorType::Rgba, 8, false),
        &[
            (b"tEXt", b"mode\0opaque".to_vec()),
            (b"iTXt", itxt(b"mode", b"alpha", false)),
            (b"tEXt", b"offs_x\0999".to_vec()),
        ],
    );
    let (_, tags) = decode(
        &data,
        size(),
        Mode::Main,
        0x02ffffff,
        &Budget::new(1024 * 1024),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(tags.iter().filter(|(k, _)| k == "mode").count(), 1);
    assert!(tags.contains(&("mode".into(), "alpha".into())));
    assert!(tags.contains(&("offs_x".into(), "-2".into())));
}
#[test]
fn malformed_and_oversized_text_cannot_allocate_unbounded_metadata() {
    let original = fixture(size(), png::ColorType::Indexed, 8, false);
    let budget = Budget::new(1024 * 1024);
    for (kind, value) in [
        (b"tEXt", b"invalid no separator".to_vec()),
        (b"tEXt", b" bad\0alpha".to_vec()),
        (b"zTXt", b"mode\0\x02anything".to_vec()),
        (
            b"zTXt",
            [b"mode\0\0".as_slice(), &zip(&vec![b'x'; 64 * 1024 + 1])].concat(),
        ),
        (b"iTXt", itxt(b"mode", &[0xff], false)),
        (b"iTXt", itxt(b"mode", b"bad\0value", false)),
    ] {
        let data = with_chunks(original.clone(), &[(kind, value)]);
        assert!(
            decode(
                &data,
                size(),
                Mode::Main,
                0x02ffffff,
                &budget,
                &AtomicBool::new(false)
            )
            .is_err()
        );
        // Metadata is irrelevant to a transition rule or province plane.
        assert!(
            decode(
                &data,
                size(),
                Mode::Province,
                0,
                &budget,
                &AtomicBool::new(false)
            )
            .is_ok()
        );
        assert_eq!(budget.used(), 0);
    }
    let many: Vec<_> = (0..1024)
        .map(|_| (b"tEXt", b"mode\0alpha".to_vec()))
        .collect();
    assert!(
        decode(
            &with_chunks(original, &many),
            size(),
            Mode::Main,
            0x02ffffff,
            &budget,
            &AtomicBool::new(false)
        )
        .is_err()
    );
}
#[test]
fn png_export_roundtrips_unicode_values_and_keeps_logical_size_out_of_tags() {
    let budget = Budget::new(8 * 1024 * 1024);
    let cancel = AtomicBool::new(false);
    let original = fixture(size(), png::ColorType::Rgba, 8, false);
    let (rgba, _) = decode(&original, size(), Mode::Main, 0x02ffffff, &budget, &cancel).unwrap();
    let expected = rgba.as_slice().to_vec();
    let tags = vec![
        ("mode".into(), "alpha".into()),
        ("width".into(), "1024".into()),
        ("note".into(), "元画像の寸法".into()),
    ];
    let mut options = crate::export::Options::default();
    options.tags = tags.clone();
    let encoded = crate::export::Request {
        pixels: std::sync::Arc::new(krkr_protocol::pixels::Pixels {
            size: size(),
            main: Some(rgba),
            province: None,
        }),
        target: None,
        format: crate::export::Format::Png {
            rgba: true,
            unfiltered: false,
        },
        options,
        budget: budget.clone(),
    }
    .run(&cancel, &std::sync::atomic::AtomicU8::new(0))
    .unwrap()
    .unwrap();
    assert_eq!(probe(encoded.as_slice()).unwrap(), size());
    let (pixels, decoded) = decode(
        encoded.as_slice(),
        size(),
        Mode::Main,
        0x02ffffff,
        &budget,
        &cancel,
    )
    .unwrap();
    assert_eq!(pixels.as_slice(), expected);
    assert_eq!(decoded, tags);
}

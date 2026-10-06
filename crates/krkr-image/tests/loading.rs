use krkr_assets::Vfs;
use krkr_image::{Decoded, resolve};
use krkr_protocol::{budget::Budget, graphics::Size};
use std::{io::Cursor, sync::atomic::AtomicBool};
use tempfile::TempDir;

const NONE: u32 = 0x1fffffff;

#[test]
fn decode_admission_includes_png_workspace_with_and_without_mask() {
    let directory = tempfile::tempdir().unwrap();
    let size = Size {
        width: 960,
        height: 540,
    };
    std::fs::write(
        directory.path().join("frame.png"),
        png(
            size.width,
            size.height,
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            &vec![127; size.rgba_bytes().unwrap()],
        ),
    )
    .unwrap();
    for mask in [false, true] {
        if mask {
            std::fs::write(
                directory.path().join("frame_m.png"),
                png(
                    size.width,
                    size.height,
                    png::ColorType::Grayscale,
                    png::BitDepth::Eight,
                    &vec![191; size.rgba_bytes().unwrap() / 4],
                ),
            )
            .unwrap();
        }
        let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
        let budget = Budget::new(16 * 1024 * 1024);
        let prepared = resolve::request(
            &mut vfs,
            &"frame".encode_utf16().collect::<Vec<_>>(),
            NONE,
            None,
            budget.clone(),
        )
        .unwrap()
        .probe(&AtomicBool::new(false))
        .unwrap();
        let required = prepared.decode_staging_bytes().unwrap();
        assert!(required > size.rgba_bytes().unwrap());
        let lock = budget.reserve(budget.available() - required).unwrap();
        let decoded = prepared.decode_compact(&AtomicBool::new(false)).unwrap();
        assert_eq!(
            decoded.pixels.main.unwrap().as_slice()[3],
            if mask { 191 } else { 127 }
        );
        drop(lock);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn transition_rules_decode_luminance_and_tile_or_crop_without_loading_sidecars() {
    let directory = tempfile::tempdir().unwrap();
    let data = png(
        3,
        1,
        png::ColorType::Indexed,
        png::BitDepth::Two,
        &[0b00011000],
    );
    std::fs::write(directory.path().join("rule.png"), data).unwrap();
    std::fs::write(directory.path().join("rule_m.png"), b"not an image").unwrap();
    let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let budget = Budget::new(1024 * 1024);
    for (size, expected) in [
        (
            Size {
                width: 2,
                height: 1,
            },
            vec![53, 182],
        ),
        (
            Size {
                width: 5,
                height: 2,
            },
            vec![53, 182, 18, 53, 182, 53, 182, 18, 53, 182],
        ),
    ] {
        let request = resolve::grayscale(
            &mut vfs,
            &"rule".encode_utf16().collect::<Vec<_>>(),
            size,
            budget.clone(),
        )
        .unwrap();
        let decoded = request
            .probe(&AtomicBool::new(false))
            .unwrap()
            .decode(&AtomicBool::new(false))
            .unwrap();
        assert!(decoded.pixels.main.is_none());
        assert_eq!(decoded.pixels.province.unwrap().as_slice(), expected);
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn grayscale_transparency_precedes_16_bit_stripping_and_palette_keeps_original_alpha_rules() {
    let directory = tempfile::tempdir().unwrap();
    let budget = Budget::new(4 * 1024 * 1024);
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, 2, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Sixteen);
        encoder.set_trns(vec![0x12, 0x34]);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0x12, 0x34, 0x12, 0x35])
            .unwrap();
    }
    std::fs::write(directory.path().join("gray.png"), encoded).unwrap();
    let decoded = load(&directory, "gray", NONE, None, &budget).unwrap();
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [0x12, 0x12, 0x12, 0, 0x12, 0x12, 0x12, 255]
    );
    assert!(
        load(
            &directory,
            "gray",
            NONE,
            Some(Size {
                width: 2,
                height: 1
            }),
            &budget
        )
        .is_err()
    );
    std::fs::write(
        directory.path().join("palette.png"),
        png(
            3,
            1,
            png::ColorType::Indexed,
            png::BitDepth::Two,
            &[0b00011000],
        ),
    )
    .unwrap();
    let decoded = load(&directory, "palette", NONE, None, &budget).unwrap();
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [255, 0, 0, 20, 0, 255, 0, 40, 0, 0, 255, 60]
    );
    let decoded = load(&directory, "palette", 0x03000002, None, &budget).unwrap();
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [255, 0, 0, 2, 0, 255, 0, 255, 0, 0, 255, 255]
    );
    assert_eq!(budget.used(), 0);
}
fn png(
    width: u32,
    height: u32,
    color: png::ColorType,
    depth: png::BitDepth,
    data: &[u8],
) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut output, width, height);
        encoder.set_color(color);
        encoder.set_depth(depth);
        if color == png::ColorType::Indexed {
            encoder.set_palette([255, 0, 0, 0, 255, 0, 0, 0, 255].to_vec());
            encoder.set_trns([20, 40, 60].to_vec());
        }
        let mut writer = encoder.write_header().unwrap();
        writer
            .write_chunk(
                png::chunk::ChunkType(*b"oFFs"),
                &[255, 255, 255, 254, 0, 0, 0, 3, 0],
            )
            .unwrap();
        writer.write_image_data(data).unwrap();
    }
    output
}
fn load(
    directory: &TempDir,
    name: &str,
    key: u32,
    size: Option<Size>,
    budget: &Budget,
) -> krkr_image::Result<Decoded> {
    let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let cancelled = AtomicBool::new(false);
    resolve::request(
        &mut vfs,
        &name.encode_utf16().collect::<Vec<_>>(),
        key,
        size,
        budget.clone(),
    )?
    .probe(&cancelled)?
    .decode(&cancelled)
}

#[test]
fn companions_color_keys_matte_metadata_and_palettized_province() {
    let directory = tempfile::tempdir().unwrap();
    let budget = Budget::new(32 * 1024 * 1024);
    let rgba = [10, 20, 30, 7, 10, 20, 30, 9, 90, 80, 70, 3];
    std::fs::write(
        directory.path().join("art.png"),
        png(3, 1, png::ColorType::Rgba, png::BitDepth::Eight, &rgba),
    )
    .unwrap();
    let decoded = load(&directory, "art", NONE, None, &budget).unwrap();
    assert_eq!(decoded.pixels.main.unwrap().as_slice(), rgba);
    assert_eq!(
        decoded.tags,
        [
            ("offs_x".into(), "-2".into()),
            ("offs_y".into(), "3".into()),
            ("offs_unit".into(), "pixel".into())
        ]
    );
    for key in [0x0a141e, 0x01ffffff] {
        let decoded = load(&directory, "art.png", key, None, &budget).unwrap();
        assert_eq!(
            decoded.pixels.main.unwrap().as_slice(),
            [10, 20, 30, 0, 10, 20, 30, 0, 90, 80, 70, 255]
        );
    }
    std::fs::write(
        directory.path().join("art_m.png"),
        png(
            3,
            1,
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            &[80, 90, 0, 80, 90, 128, 80, 90, 255],
        ),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("art_p.png"),
        png(
            2,
            1,
            png::ColorType::Indexed,
            png::BitDepth::Two,
            &[0b01100000],
        ),
    )
    .unwrap();
    let decoded = load(&directory, "art", NONE, None, &budget).unwrap();
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [10, 20, 30, 0, 10, 20, 30, 128, 90, 80, 70, 255]
    );
    assert_eq!(decoded.pixels.province.unwrap().as_slice(), [1, 2, 1]);
    let decoded = load(&directory, "art", 0x04ffffff, None, &budget).unwrap();
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [255, 255, 255, 255, 132, 137, 142, 255, 90, 80, 70, 255]
    );
    drop(decoded.pixels.province);
    let decoded = load(
        &directory,
        "art_p.png",
        NONE,
        Some(Size {
            width: 3,
            height: 2,
        }),
        &budget,
    )
    .unwrap();
    assert!(decoded.pixels.main.is_none());
    assert_eq!(
        decoded.pixels.province.unwrap().as_slice(),
        [1, 2, 1, 1, 2, 1]
    );
    // The original PNG loader installs a single tRNS entry with alpha=keyidx.
    let decoded = load(&directory, "art_p.png", 0x03000002, None, &budget).unwrap();
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [0, 255, 0, 255, 0, 0, 255, 255]
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn bmp_jpeg_and_both_tlg_versions_decode_through_the_same_vfs_path() {
    let directory = tempfile::tempdir().unwrap();
    let budget = Budget::new(32 * 1024 * 1024);
    let rgb = [37, 91, 183].repeat(16 * 9);
    let mut bmp = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut bmp)
        .encode(&rgb, 16, 9, image::ExtendedColorType::Rgb8)
        .unwrap();
    std::fs::write(directory.path().join("sample.bmp"), &bmp).unwrap();
    std::fs::write(directory.path().join("dib.dib"), &bmp[14..]).unwrap();
    for name in ["sample.bmp", "dib.dib"] {
        let decoded = load(&directory, name, NONE, None, &budget).unwrap();
        assert_eq!(
            decoded.pixels.main.unwrap().as_slice(),
            [37, 91, 183, 255].repeat(16 * 9)
        );
    }
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 100)
        .encode(&rgb, 16, 9, image::ExtendedColorType::Rgb8)
        .unwrap();
    std::fs::write(directory.path().join("photo.jpg"), jpeg).unwrap();
    let decoded = load(&directory, "photo", NONE, None, &budget).unwrap();
    for pixel in decoded
        .pixels
        .main
        .unwrap()
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
    {
        assert!(
            pixel[..3]
                .iter()
                .zip([37u8, 91, 183])
                .all(|(&a, b)| a.abs_diff(b) <= 2)
        );
        assert_eq!(pixel[3], 255);
    }
    let tlg = libtlg_rs::Tlg {
        width: 16,
        height: 9,
        version: 5,
        color: libtlg_rs::TlgColorType::Bgra32,
        data: (0..16 * 9).flat_map(|i| [i as u8, 91, 37, 113]).collect(),
        tags: [(b"origin".to_vec(), b"fixture".to_vec())].into(),
    };
    let mut encoded = Cursor::new(Vec::new());
    libtlg_rs::save_tlg(&tlg, &mut encoded).unwrap();
    std::fs::write(directory.path().join("tlg5.tlg"), encoded.into_inner()).unwrap();
    let decoded = load(&directory, "tlg5", NONE, None, &budget).unwrap();
    assert_eq!(decoded.tags, [("origin".into(), "fixture".into())]);
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        (0..16 * 9)
            .flat_map(|i| [37, 91, i as u8, 113])
            .collect::<Vec<_>>()
    );
    // Independent raw TLG6 fixture: 1x1, three channels, MED filter, one
    // zero-valued Golomb run per channel. The TLG5 encoder cannot emit TLG6.
    let mut tlg6 = b"TLG6.0\0raw\x1a".to_vec();
    tlg6.extend([3, 0, 0, 0]);
    for value in [1u32, 1, 2, 2] {
        tlg6.extend(value.to_le_bytes());
    }
    tlg6.extend([0, 0]); // literal LZSS filter byte
    for _ in 0..3 {
        tlg6.extend(2u32.to_le_bytes());
        tlg6.push(2);
    }
    std::fs::write(directory.path().join("tlg6.tlg6"), tlg6).unwrap();
    let decoded = load(&directory, "tlg6", NONE, None, &budget).unwrap();
    assert_eq!(decoded.pixels.main.unwrap().as_slice(), [0, 0, 0, 255]);
    assert!(
        load(
            &directory,
            "tlg6",
            NONE,
            Some(Size {
                width: 1,
                height: 1
            }),
            &budget
        )
        .is_err()
    );
    assert_eq!(budget.used(), 0);
}

#[test]
fn legacy_tlg_tags_normalize_to_utf8_without_changing_compressed_pixels() {
    let directory = tempfile::tempdir().unwrap();
    let budget = Budget::new(4 * 1024 * 1024);
    let expected = "@a@a2@光眼鏡a";
    let (legacy, _, errors) = encoding_rs::SHIFT_JIS.encode(expected);
    assert!(!errors);
    let tlg = libtlg_rs::Tlg {
        width: 2,
        height: 1,
        version: 5,
        color: libtlg_rs::TlgColorType::Bgra32,
        data: vec![3, 2, 1, 255, 30, 20, 10, 128],
        tags: [
            (b"names".to_vec(), legacy.into_owned()),
            (b"left".to_vec(), b"90".to_vec()),
            ("注釈".as_bytes().to_vec(), "日本語".as_bytes().to_vec()),
            (vec![0x8c, 0xf5], b"legacy key".to_vec()),
        ]
        .into(),
    };
    let mut encoded = Cursor::new(Vec::new());
    libtlg_rs::save_tlg(&tlg, &mut encoded).unwrap();
    let mut original = encoded.into_inner();
    let pixel_end = 15 + u32::from_le_bytes(original[11..15].try_into().unwrap()) as usize;
    let unknown = b"note\x03\0\0\0\0\xff\x7f";
    original.extend_from_slice(unknown);
    // Original Kirikiri ignores an incomplete trailing chunk name.
    original.extend_from_slice(b":2,");
    let normalized = krkr_image::normalize_tlg_metadata(&original)
        .unwrap()
        .unwrap();
    assert_eq!(&normalized[..pixel_end], &original[..pixel_end]);
    assert!(normalized.ends_with(unknown));
    let tag_length =
        u32::from_le_bytes(normalized[pixel_end + 4..pixel_end + 8].try_into().unwrap()) as usize;
    let text = std::str::from_utf8(&normalized[pixel_end + 8..pixel_end + 8 + tag_length]).unwrap();
    assert!(text.contains(expected));
    assert!(
        krkr_image::normalize_tlg_metadata(&normalized)
            .unwrap()
            .is_none()
    );
    for (name, bytes) in [("legacy.tlg", original), ("utf8.tlg", normalized)] {
        std::fs::write(directory.path().join(name), bytes).unwrap();
        let decoded = load(&directory, name, NONE, None, &budget).unwrap();
        assert!(decoded.tags.contains(&("names".into(), expected.into())));
        assert!(decoded.tags.contains(&("光".into(), "legacy key".into())));
        assert!(decoded.tags.contains(&("注釈".into(), "日本語".into())));
        assert_eq!(
            decoded.pixels.main.unwrap().as_slice(),
            [1, 2, 3, 255, 10, 20, 30, 128]
        );
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn failures_cancellation_and_output_ownership_release_the_budget() {
    let directory = tempfile::tempdir().unwrap();
    let encoded = png(3, 1, png::ColorType::Rgb, png::BitDepth::Eight, &[23; 9]);
    std::fs::write(directory.path().join("small.png"), &encoded).unwrap();
    let budget = Budget::new(4 * 1024 * 1024);
    let decoded = load(&directory, "small", NONE, None, &budget).unwrap();
    assert_eq!(budget.used(), 12);
    drop(decoded);
    assert_eq!(budget.used(), 0);
    let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let request = resolve::request(
        &mut vfs,
        &"small.png".encode_utf16().collect::<Vec<_>>(),
        NONE,
        None,
        budget.clone(),
    )
    .unwrap();
    let prepared = request.probe(&AtomicBool::new(false)).unwrap();
    assert!(budget.used() > 0);
    assert!(prepared.decode(&AtomicBool::new(true)).is_err());
    assert_eq!(budget.used(), 0);
    let tiny = Budget::new(encoded.len());
    assert!(load(&directory, "small", NONE, None, &tiny).is_err());
    assert_eq!(tiny.used(), 0);
    std::fs::write(
        directory.path().join("small_m.png"),
        png(
            1,
            1,
            png::ColorType::Grayscale,
            png::BitDepth::Eight,
            &[255],
        ),
    )
    .unwrap();
    assert!(load(&directory, "small", NONE, None, &budget).is_err());
    assert_eq!(budget.used(), 0);
    std::fs::write(
        directory.path().join("broken.png"),
        &encoded[..encoded.len() - 15],
    )
    .unwrap();
    assert!(load(&directory, "broken", NONE, None, &budget).is_err());
    assert_eq!(budget.used(), 0);
}

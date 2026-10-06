use krkr_assets::{Vfs, name::units};
use krkr_convert::bc::{Encoder, Quality};
use krkr_protocol::{budget::Budget, graphics::Size, texture::Format};
use std::sync::atomic::AtomicBool;

#[test]
fn packed_bc_mixed_tiles_keep_tags_native_upload_and_alpha_endpoints() {
    let size = Size {
        width: 32,
        height: 32,
    };
    let rgba: Vec<u8> = (0..1024)
        .flat_map(|i| [255, 0, 0, if i % 32 < 8 { 0 } else { 255 }])
        .collect();
    let encoder = Encoder::new(Quality::Balanced);
    let packed = encoder.encode_packed(size, &rgba, Format::Bc3Rgba).unwrap();
    assert!(packed.starts_with(krkr_image::packed_bc::MAGIC));
    let bc = krkr_image::packed_bc::wrap_bc(&encoder.encode(size, &rgba, Format::Bc3Rgba).unwrap())
        .unwrap();
    let tags = vec![
        ("face".to_owned(), "笑顔".to_owned()),
        ("x".to_owned(), "17".to_owned()),
    ];
    let combined = krkr_image::packed_bc::assemble(
        Size {
            width: 64,
            height: 32,
        },
        &[packed, bc],
        &tags,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("face.kbct"), &combined).unwrap();
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    let budget = Budget::new(2 << 20);
    let image =
        krkr_image::resolve::request(&mut vfs, &units("face"), 0x02ffffff, None, budget.clone())
            .unwrap()
            .probe(&AtomicBool::new(false))
            .unwrap();
    assert_eq!(image.format_name(), "kbct");
    assert!(image.is_compressed_upload());
    assert_eq!(image.compressed_upload_bytes(), Some(2048));
    let (texture, actual_tags) = image.into_compressed_with_tags().unwrap();
    assert_eq!(actual_tags, tags);
    assert_eq!(texture.format, Format::Bc3RgbaVita);
    assert_eq!(
        budget.used(),
        2048,
        "retain BC blocks only, release packed BC and scratch"
    );
    let pixels = krkr_image::compressed::decode(&texture, &budget, &AtomicBool::new(false))
        .unwrap()
        .main
        .unwrap();
    for (i, p) in pixels.as_slice().as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(p[3], if i % 32 < 8 { 0 } else { 255 });
        if p[3] == 255 {
            assert!(p[0] > 245 && p[1] < 8 && p[2] < 8);
        }
    }
    drop(pixels);
    drop(texture);
    assert_eq!(budget.used(), 0);

    for length in [0, 7, 31, combined.len() - 1] {
        std::fs::write(dir.path().join("broken.kbct"), &combined[..length]).unwrap();
        let result = krkr_image::resolve::request(
            &mut vfs,
            &units("broken.kbct"),
            0x02ffffff,
            None,
            budget.clone(),
        )
        .unwrap()
        .probe(&AtomicBool::new(false));
        assert!(result.is_err());
        assert_eq!(budget.used(), 0);
    }
}

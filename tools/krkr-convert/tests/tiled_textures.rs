use krkr_assets::{Vfs, name::units};
use krkr_convert::{
    helper::{Prepared, Target},
    psv, texture,
};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
    texture::Format,
};
use std::{fs, path::Path, sync::atomic::AtomicBool};

#[test]
fn denser_textures_still_save_memory_and_prefer_fewer_draws() {
    let stored = Size {
        width: 1024,
        height: 128,
    };
    let lossless = Size {
        width: 960,
        height: 120,
    };
    let alternatives = texture::quality_sizes(stored, lossless, Format::Bc3Rgba);
    assert_eq!(
        alternatives,
        vec![
            Size {
                width: 1024,
                height: 256
            },
            Size {
                width: 2048,
                height: 128
            },
        ]
    );
    for size in alternatives {
        assert!(Format::Bc3Rgba.byte_len(size).unwrap() <= lossless.rgba_bytes().unwrap() * 3 / 4);
    }
    // POT padding already consumes most of the saving for this alpha image.
    // Its RGB counterpart still has enough headroom for the same retry.
    let stored = Size {
        width: 256,
        height: 256,
    };
    let lossless = Size {
        width: 192,
        height: 192,
    };
    assert!(texture::quality_sizes(stored, lossless, Format::Bc3Rgba).is_empty());
    assert_eq!(
        texture::quality_sizes(stored, lossless, Format::Bc1Rgb).len(),
        2
    );
}

#[test]
fn denser_textures_reject_oversized_inputs_without_overflow() {
    let huge = Size {
        width: u32::MAX,
        height: u32::MAX,
    };
    let normal = Size {
        width: 1024,
        height: 1024,
    };
    assert!(texture::quality_sizes(huge, normal, Format::Bc1Rgb).is_empty());
}

#[test]
fn rejected_first_tile_does_not_encode_the_rest_of_a_large_image() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let size = Size {
        width: 8192,
        height: 8,
    };
    let pixels = [80, 100, 120, 255].repeat((size.width * size.height) as usize);
    let encodes = AtomicUsize::new(0);
    let result = texture::encode_tiled_checked(
        size,
        &pixels,
        &Vec::new(),
        |size, _| {
            encodes.fetch_add(1, Ordering::SeqCst);
            krkr_image::compressed::ktx_format(
                size,
                Format::Bc1Rgb,
                &vec![0; Format::Bc1Rgb.byte_len(size).unwrap()],
            )
            .map_err(|e| e.to_string())
        },
        |_, _, _| Ok(Some("opacity was damaged".into())),
    );
    assert_eq!(
        result,
        Err(texture::EncodeError::Quality("opacity was damaged".into()))
    );
    assert_eq!(
        encodes.load(Ordering::SeqCst),
        1,
        "remaining seven encodes are wasted work"
    );
}

#[test]
fn checked_tiles_do_not_turn_encoder_failures_into_lossless_fallback() {
    let size = Size {
        width: 2048,
        height: 8,
    };
    let pixels = vec![0; size.rgba_bytes().unwrap()];
    let result = texture::encode_tiled_checked(
        size,
        &pixels,
        &Vec::new(),
        |_, _| Err("encoder process failed".into()),
        |_, _, _| panic!("there is no encoded data to validate"),
    );
    assert_eq!(
        result,
        Err(texture::EncodeError::Failed(
            "encoder process failed".into()
        ))
    );
}

#[test]
fn rejection_of_a_later_tile_stops_pending_tiles() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let size = Size {
        width: 8192,
        height: 8,
    };
    let pixels = [80, 100, 120, 255].repeat((size.width * size.height) as usize);
    let encodes = AtomicUsize::new(0);
    let checks = AtomicUsize::new(0);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let result = pool.install(|| {
        texture::encode_tiled_checked(
            size,
            &pixels,
            &Vec::new(),
            |size, _| {
                encodes.fetch_add(1, Ordering::SeqCst);
                krkr_image::compressed::ktx_format(
                    size,
                    Format::Bc1Rgb,
                    &vec![0; Format::Bc1Rgb.byte_len(size).unwrap()],
                )
                .map_err(|e| e.to_string())
            },
            |_, _, _| {
                Ok((checks.fetch_add(1, Ordering::SeqCst) == 1)
                    .then(|| "second tile failed".into()))
            },
        )
    });
    assert_eq!(
        result,
        Err(texture::EncodeError::Quality("second tile failed".into()))
    );
    assert_eq!(encodes.load(Ordering::SeqCst), 2);
}

fn source(path: &Path, size: Size, alpha: bool) -> krkr_image::Tags {
    let budget = Budget::new(64 * 1024 * 1024);
    let mut pixels = Bytes::zeroed(size.rgba_bytes().unwrap(), &budget).unwrap();
    for (i, p) in pixels
        .as_mut_slice()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .enumerate()
    {
        let x = i % size.width as usize;
        // Constant alpha isolates tile storage and metadata from the lossy
        // encoder's quality limits; gradient accuracy is tested separately.
        p.copy_from_slice(&[
            if alpha {
                60
            } else if x < size.width as usize / 2 {
                180
            } else {
                30
            },
            60,
            120,
            if alpha { 119 } else { 255 },
        ]);
    }
    let tags = vec![
        ("groundLevel".into(), "454".into()),
        ("centerCorrect".into(), "-34".into()),
        ("names".into(), "@a@a2@b".into()),
    ];
    krkr_image::save::Request {
        target: krkr_assets::WritePlan::local(path, 64 * 1024 * 1024).unwrap(),
        format: krkr_image::save::Format::Tlg { six: false, alpha },
        pixels: Pixels {
            size,
            main: Some(pixels),
            province: None,
        },
        tags: tags.clone(),
        budget,
    }
    .write(&AtomicBool::new(false))
    .unwrap();
    tags
}

fn helper_roundtrip(alpha: bool) {
    let root = tempfile::tempdir().unwrap();
    let game = root.path().join("game");
    let assets = root.path().join("assets");
    let output = root.path().join("output");
    fs::create_dir(&game).unwrap();
    fs::create_dir(&assets).unwrap();
    let sizes = [
        Size {
            width: 64,
            height: 64,
        },
        Size {
            width: 1100,
            height: 64,
        },
    ];
    let mut expected = Vec::new();
    for (i, size) in sizes.iter().enumerate() {
        expected.push(source(&assets.join(format!("actor{i}.tlg")), *size, alpha));
    }
    krkr_assets::xp3::offline::pack_directory(
        &assets,
        &game.join("fg.xp3"),
        krkr_assets::xp3::Compression::None,
        Default::default(),
    )
    .unwrap();
    let original = fs::read(game.join("fg.xp3")).unwrap();
    let prepared = Prepared::new(&game, &output, None).unwrap();
    let inventory = prepared.inspect(&Default::default()).unwrap();
    let mut options = psv::Options::vita(Size {
        width: 960,
        height: 544,
    });
    options.texture_auto = true;
    let report = prepared
        .convert(inventory, Target::Psv(options), &Default::default())
        .unwrap();
    assert_eq!(
        report
            .parts
            .iter()
            .map(|p| p.compressed_textures)
            .sum::<usize>(),
        2,
        "{}",
        serde_json::to_string(&report).unwrap()
    );
    assert_eq!(
        report
            .parts
            .iter()
            .map(|p| p.transparent_textures)
            .sum::<usize>(),
        if alpha { 2 } else { 0 }
    );
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    for (i, size) in sizes.iter().enumerate() {
        let prepared = krkr_image::resolve::request(
            &mut vfs,
            &units(&format!("fg.xp3>actor{i}.tlg")),
            0x02ffffff,
            None,
            Budget::new(256 * 1024),
        )
        .unwrap()
        .probe(&AtomicBool::new(false))
        .unwrap();
        assert_eq!(prepared.size, *size);
        assert_eq!(
            prepared.upload_size(),
            texture::storage_size(*size).unwrap()
        );
        let (native, tags) = prepared.into_compressed_with_tags().unwrap();
        for tag in &expected[i] {
            assert!(tags.contains(tag), "lost {tag:?}");
        }
        assert_eq!(native.tiles().count(), i + 1);
        assert_eq!(
            native.format,
            if alpha {
                Format::Bc3RgbaVita
            } else {
                Format::Bc1RgbVita
            }
        );
        let decoded = krkr_image::compressed::decode(
            &native,
            &Budget::new(4 * 1024 * 1024),
            &AtomicBool::new(false),
        )
        .unwrap()
        .main
        .unwrap();
        if alpha {
            assert!(
                decoded
                    .as_slice()
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[3].abs_diff(119) <= 4)
            );
        } else {
            // Check across the tile boundary, not just the first pixel.
            for (x, red) in [(0, 180u8), (native.size.width - 1, 30u8)] {
                assert!(decoded.as_slice()[x as usize * 4].abs_diff(red) <= 12);
            }
        }
    }
    assert_eq!(fs::read(game.join("fg.xp3")).unwrap(), original);
}

#[test]
fn helper_compresses_tagged_and_large_images_without_changing_script_size() {
    helper_roundtrip(false);
}

#[test]
fn helper_compresses_tagged_transparent_tiles_with_real_encoder() {
    helper_roundtrip(true);
}

#[test]
fn tile_storage_bounds_avoid_whole_image_power_of_two_expansion() {
    assert_eq!(
        texture::storage_size(Size {
            width: 5000,
            height: 201
        }),
        Some(Size {
            width: 5120,
            height: 256
        })
    );
    for size in [
        Size {
            width: 0,
            height: 100,
        },
        Size {
            width: u32::MAX,
            height: 8,
        },
        Size {
            width: 16384,
            height: 16384,
        },
    ] {
        assert!(texture::storage_size(size).is_none());
    }
}

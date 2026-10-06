use krkr_assets::{Vfs, name::units};
use krkr_protocol::{budget::Budget, graphics::Size};
use std::sync::atomic::AtomicBool;

#[test]
fn mahoyo_cropped_ktx_link_and_scale_keep_visible_canvas_and_native_backing() {
    use krkr_image::scale::Metadata;
    use krkr_protocol::texture::Format;
    let canvas = Size {
        width: 904,
        height: 512,
    };
    let backing = Size {
        width: 1024,
        height: 512,
    };
    let logical = Size {
        width: 1024,
        height: 576,
    };
    // Same geometry and link/sidecar arrangement as the packaged 5b-15 still.
    // Synthetic red BC1 blocks keep this regression independent of game data.
    let block = [0x00, 0xf8, 0x00, 0xf8, 0, 0, 0, 0];
    let blocks = block.repeat(Format::Bc1Rgb.byte_len(backing).unwrap() / block.len());
    let linear =
        krkr_image::compressed::ktx_tiles(canvas, backing, Format::Bc1Rgb, &blocks, &Vec::new())
            .unwrap();
    let native = krkr_image::compressed::vita_bc(&linear).unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("still.kbct"), native).unwrap();
    std::fs::write(
        dir.path().join("still.png.krkr-link"),
        b"KRKR-LINK-1\nstill.kbct",
    )
    .unwrap();
    let scale = Metadata {
        logical,
        stored: canvas,
    }
    .encode()
    .unwrap();
    for name in ["still.png.krkr-scale", "still.kbct.krkr-scale"] {
        std::fs::write(dir.path().join(name), scale).unwrap();
    }
    let budget = Budget::new(1024 * 1024);
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("still.png"),
        0x02ffffff,
        None,
        budget.clone(),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(prepared.format_name(), "ktx");
    assert_eq!(prepared.size, logical);
    assert_eq!(prepared.upload_size(), canvas);
    assert_eq!(prepared.compressed_upload_bytes(), Some(262144));
    let texture = prepared.into_compressed().unwrap();
    assert_eq!(texture.size, canvas);
    assert_eq!(texture.tile_size, backing);
    assert_eq!(texture.format, Format::Bc1RgbVita);
    let tiles: Vec<_> = texture.tiles().collect();
    assert_eq!(tiles.len(), 1);
    assert_eq!(tiles[0].0, backing.rect());
    assert_eq!(tiles[0].1.len(), 262144);
    drop(texture);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cropped_native_bc_canvas_keeps_padding_outside_cpu_pixels() {
    use krkr_protocol::texture::Format;
    let tile = Size {
        width: 16,
        height: 8,
    };
    let canvas = Size {
        width: 13,
        height: 7,
    };
    // Distinct blocks expose a wrong row stride when cropping the right edge.
    let mut blocks = Vec::new();
    for color in [0xf800u16, 0x07e0, 0x001f, 0xffff]
        .into_iter()
        .cycle()
        .take(8)
    {
        blocks.extend_from_slice(&color.to_le_bytes());
        blocks.extend_from_slice(&color.to_le_bytes());
        blocks.extend_from_slice(&[0; 4]);
    }
    let linear =
        krkr_image::compressed::ktx_tiles(canvas, tile, Format::Bc1Rgb, &blocks, &Vec::new())
            .unwrap();
    let native = krkr_image::compressed::vita_bc(&linear).unwrap();
    let budget = Budget::new(64 * 1024);
    let prepared = prepare(&native, 0x02ffffff, &budget).unwrap();
    assert_eq!(prepared.size, canvas);
    assert_eq!(prepared.compressed_upload_bytes(), Some(64));
    let texture = prepared.into_compressed().unwrap();
    let tiles: Vec<_> = texture.tiles().collect();
    assert_eq!(tiles.len(), 1);
    assert_eq!((tiles[0].0.width, tiles[0].0.height), (16, 8));
    let decoded = prepare(&native, 0x02ffffff, &budget)
        .unwrap()
        .decode(&AtomicBool::new(false))
        .unwrap();
    let full = krkr_image::compressed::ktx_format(tile, Format::Bc1Rgb, &blocks).unwrap();
    let reference = prepare(&full, 0x02ffffff, &budget)
        .unwrap()
        .decode(&AtomicBool::new(false))
        .unwrap();
    let cropped = decoded.pixels.main.unwrap();
    let reference = reference.pixels.main.unwrap();
    assert_eq!(cropped.as_slice().len(), 13 * 7 * 4);
    for y in 0..7 {
        assert_eq!(
            &cropped.as_slice()[y * 13 * 4..(y + 1) * 13 * 4],
            &reference.as_slice()[y * 16 * 4..y * 16 * 4 + 13 * 4]
        );
    }
}

#[test]
fn native_bc_decode_matches_linear_blocks_on_rectangular_grids() {
    use krkr_protocol::{
        pixels::Bytes,
        texture::{Compressed, Format, vita_block_index},
    };
    let budget = Budget::new(2 * 1024 * 1024);
    let cancelled = AtomicBool::new(false);
    for (width, height) in [(8, 8), (8, 128), (128, 8), (64, 256), (256, 64), (128, 128)] {
        let size = Size { width, height };
        for format in [Format::Bc1Rgb, Format::Bc3Rgba] {
            let length = format.byte_len(size).unwrap();
            let stride = if format == Format::Bc1Rgb { 8 } else { 16 };
            let mut linear = Bytes::zeroed(length, &budget).unwrap();
            let mut seed = 42u32;
            for byte in linear.as_mut_slice() {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *byte = (seed >> 24) as u8;
            }
            let mut native = Bytes::zeroed(length, &budget).unwrap();
            let (columns, rows) = (width / 4, height / 4);
            for y in 0..rows {
                for x in 0..columns {
                    let from = (y * columns + x) as usize * stride;
                    // Independent bit-by-bit addressing is the reference;
                    // distinct block contents expose either axis being wrong.
                    let to = vita_block_index(x, y, columns, rows) * stride;
                    native.as_mut_slice()[to..to + stride]
                        .copy_from_slice(&linear.as_slice()[from..from + stride]);
                }
            }
            let expected = krkr_image::compressed::decode(
                &Compressed::new(size, format, linear, 0).unwrap(),
                &budget,
                &cancelled,
            )
            .unwrap();
            let actual = krkr_image::compressed::decode(
                &Compressed::new(size, format.vita().unwrap(), native, 0).unwrap(),
                &budget,
                &cancelled,
            )
            .unwrap();
            assert_eq!(
                actual.main.unwrap().as_slice(),
                expected.main.unwrap().as_slice(),
                "{format:?} {size:?}"
            );
        }
    }
    assert_eq!(budget.used(), 0);
}

#[test]
fn pvrtextool_2d_orientation_retains_compressed_payload() {
    let size = Size {
        width: 32,
        height: 16,
    };
    let pvr = include_bytes!("data/pvrtc-gradient.pvr");
    let original = krkr_image::compressed::ktx_format(
        size,
        krkr_protocol::texture::Format::Pvrtc1Rgba4,
        &pvr[52..],
    )
    .unwrap();
    let payload = 64 + u32::from_le_bytes(original[60..64].try_into().unwrap()) as usize;
    for orientation in ["S=r,T=d,R=i", "S=r,T=u,R=i"] {
        let value = format!("KTXorientation\0{orientation}\0");
        let mut meta = (value.len() as u32).to_le_bytes().to_vec();
        meta.extend_from_slice(value.as_bytes());
        meta.resize((meta.len() + 3) & !3, 0);
        let mut external = original[..64].to_vec();
        external[60..64].copy_from_slice(&(meta.len() as u32).to_le_bytes());
        external.extend_from_slice(&meta);
        external.extend_from_slice(&original[payload..]);
        let normalized = krkr_image::compressed::assemble(size, &[external], &Vec::new());
        if orientation == "S=r,T=d,R=i" {
            assert_eq!(normalized.unwrap(), original);
        } else {
            assert!(
                normalized.is_err(),
                "upside-down data must not be silently accepted"
            );
        }
    }
}

#[test]
fn pvrtc_rgba_matches_reference_decoder_and_retains_native_payload() {
    let pvr = include_bytes!("data/pvrtc-gradient.pvr");
    let reference = include_bytes!("data/pvrtc-gradient.rgba");
    let size = Size {
        width: 32,
        height: 16,
    };
    let format = krkr_protocol::texture::Format::Pvrtc1Rgba4;
    let ktx = krkr_image::compressed::ktx_format(size, format, &pvr[52..]).unwrap();
    let budget = Budget::new(16384);
    let image = prepare(&ktx, 0x02ffffff, &budget).unwrap();
    assert!(image.is_compressed_upload());
    let texture = image.into_compressed().unwrap();
    assert_eq!(texture.format, format);
    assert_eq!(texture.data(), &pvr[52..]);
    let rgba = krkr_image::compressed::decode(&texture, &budget, &AtomicBool::new(false))
        .unwrap()
        .main
        .unwrap();
    // The SDK's preview expands RGB443 through RGB444 then floats. Native
    // PVRTC interpolation uses RGB555/A4, so low-bit RGB endpoints can differ
    // by a few levels. Alpha differs only by float rounding (one level).
    let maxima: [u8; 4] = std::array::from_fn(|c| {
        rgba.as_slice()
            .iter()
            .zip(reference)
            .skip(c)
            .step_by(4)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap()
    });
    assert!(
        maxima[0] <= 5 && maxima[1] <= 5 && maxima[2] <= 7 && maxima[3] <= 1,
        "reference decode differs: {maxima:?}"
    );
    let container = prepare(&ktx, 0x02ffffff, &budget)
        .unwrap()
        .decode(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(container.pixels.main.unwrap().as_slice(), rgba.as_slice());
    assert!(
        rgba.as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] < 128)
    );
}

#[test]
fn pot_storage_larger_than_logical_image_preserves_sampling_and_rejects_mismatch() {
    use krkr_image::scale::{Metadata, SUFFIX};
    let stored = Size {
        width: 32,
        height: 16,
    };
    let logical = Size {
        width: 30,
        height: 14,
    };
    let metadata = Metadata { logical, stored };
    let pvr = include_bytes!("data/pvrtc-gradient.pvr");
    let ktx = krkr_image::compressed::ktx_format(
        stored,
        krkr_protocol::texture::Format::Pvrtc1Rgba4,
        &pvr[52..],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("image.ktx"), &ktx).unwrap();
    let sidecar = dir.path().join(format!("image.ktx{SUFFIX}"));
    std::fs::write(&sidecar, metadata.encode().unwrap()).unwrap();
    let budget = Budget::new(32768);
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    let cancelled = AtomicBool::new(false);
    let image = krkr_image::resolve::request(
        &mut vfs,
        &units("image.ktx"),
        0x02ffffff,
        None,
        budget.clone(),
    )
    .unwrap()
    .probe(&cancelled)
    .unwrap();
    assert_eq!(image.size, logical);
    assert_eq!(image.upload_size(), stored);
    assert!(image.is_compressed_upload());
    let decoded = image.decode(&cancelled).unwrap();
    assert_eq!(decoded.pixels.size, logical);
    let actual = decoded.pixels.main.unwrap();
    let full = prepare(&ktx, 0x02ffffff, &budget)
        .unwrap()
        .decode(&cancelled)
        .unwrap()
        .pixels
        .main
        .unwrap();
    for y in 0..logical.height as usize {
        for x in 0..logical.width as usize {
            let sx = (2 * x + 1) * stored.width as usize / (2 * logical.width as usize);
            let sy = (2 * y + 1) * stored.height as usize / (2 * logical.height as usize);
            let dst = (y * logical.width as usize + x) * 4;
            let src = (sy * stored.width as usize + sx) * 4;
            assert_eq!(
                &actual.as_slice()[dst..dst + 4],
                &full.as_slice()[src..src + 4]
            );
        }
    }
    assert!(metadata.validate(logical).is_err());
    let wrong = Metadata {
        logical,
        stored: logical,
    };
    std::fs::write(&sidecar, wrong.encode().unwrap()).unwrap();
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    assert!(
        krkr_image::resolve::request(&mut vfs, &units("image.ktx"), 0x02ffffff, None, budget,)
            .unwrap()
            .probe(&cancelled)
            .is_err()
    );
}

#[test]
fn pvrtc_punchthrough_and_layout_validation() {
    use krkr_protocol::texture::Format;
    let size = Size {
        width: 8,
        height: 8,
    };
    // Equal white opaque endpoints, all modulation codes use punch-through.
    let block = [0xaa, 0xaa, 0xaa, 0xaa, 0xff, 0xff, 0xff, 0xff];
    let ktx =
        krkr_image::compressed::ktx_format(size, Format::Pvrtc1Rgba4, &block.repeat(4)).unwrap();
    let decoded = prepare(&ktx, 0x02ffffff, &Budget::new(4096))
        .unwrap()
        .decode(&AtomicBool::new(false))
        .unwrap();
    assert!(
        decoded
            .pixels
            .main
            .unwrap()
            .as_slice()
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| *p == [255, 255, 255, 0])
    );
    for bad in [
        Size {
            width: 7,
            height: 8,
        },
        Size {
            width: 12,
            height: 16,
        },
        Size {
            width: 0,
            height: 8,
        },
    ] {
        assert!(Format::Pvrtc1Rgba4.byte_len(bad).is_none());
        assert!(krkr_image::compressed::ktx_format(bad, Format::Pvrtc1Rgba4, &[0; 32]).is_err());
    }
    let mut malformed = ktx;
    malformed[32..36].copy_from_slice(&0x1907u32.to_le_bytes());
    assert!(prepare(&malformed, 0x02ffffff, &Budget::new(4096)).is_err());
}

fn prepare(data: &[u8], key: u32, budget: &Budget) -> krkr_image::Result<krkr_image::Prepared> {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("image.tlg"), data).unwrap();
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    krkr_image::resolve::request(&mut vfs, &units("image.tlg"), key, None, budget.clone())?
        .probe(&AtomicBool::new(false))
}
#[test]
fn gpu_blocks_keep_the_original_budgeted_allocation_without_rgba_expansion() {
    let size = Size {
        width: 128,
        height: 64,
    };
    let blocks = [0x12, 0x34, 0x56, 0, 0, 0, 0, 0].repeat(512);
    let ktx = krkr_image::compressed::ktx(size, &blocks).unwrap();
    let budget = Budget::new(5000);
    let prepared = prepare(&ktx, 0x02ffffff, &budget).unwrap();
    assert_eq!(prepared.format_name(), "ktx");
    assert!(prepared.is_compressed_upload());
    assert_eq!(prepared.upload_size(), size);
    let held = budget.used();
    let texture = prepared.into_compressed().unwrap();
    assert_eq!(budget.used(), held);
    assert_eq!(texture.data(), blocks);
    assert!(krkr_image::compressed::decode(&texture, &budget, &AtomicBool::new(false)).is_err());
    drop(texture);
    assert_eq!(budget.used(), 0);
}
#[test]
fn individual_and_differential_blocks_decode_positions_modifiers_and_color_keys() {
    let budget = Budget::new(1024 * 1024);
    let size = Size {
        width: 7,
        height: 4,
    };
    let blocks = [
        0x12, 0x34, 0x56, 0, 0, 0, 0, 0, 0x82, 0x8f, 0x94, 3, 0, 0, 0, 0,
    ];
    let ktx = krkr_image::compressed::ktx(size, &blocks).unwrap();
    let decoded = prepare(&ktx, 0x02ffffff, &budget)
        .unwrap()
        .decode(&AtomicBool::new(false))
        .unwrap();
    let rgba = decoded.pixels.main.unwrap();
    for y in 0..4 {
        for x in 0..7 {
            let expected = if x < 2 {
                [19, 53, 87, 255]
            } else if x < 4 {
                [36, 70, 104, 255]
            } else if y < 2 {
                [134, 142, 150, 255]
            } else {
                [150, 134, 117, 255]
            };
            assert_eq!(&rgba.as_slice()[(y * 7 + x) * 4..][..4], &expected);
        }
    }
    let prepared = prepare(&ktx, 0x00133557, &budget).unwrap();
    assert!(!prepared.is_compressed_upload());
    let keyed = prepared
        .decode(&AtomicBool::new(false))
        .unwrap()
        .pixels
        .main
        .unwrap();
    assert_eq!(keyed.as_slice()[3], 0);
    assert_eq!(keyed.as_slice()[2 * 4 + 3], 255);
}
#[test]
fn malformed_sizes_orientation_and_undefined_differential_blocks_are_rejected() {
    let size = Size {
        width: 4,
        height: 4,
    };
    let original = krkr_image::compressed::ktx(size, &[0; 8]).unwrap();
    let budget = Budget::new(4096);
    for n in [0, 12, 64, original.len() - 1] {
        assert!(prepare(&original[..n], 0x02ffffff, &budget).is_err());
        assert_eq!(budget.used(), 0);
    }
    for at in [16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 60] {
        let mut bytes = original.clone();
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(prepare(&bytes, 0x02ffffff, &budget).is_err(), "field={at}");
    }
    let mut bytes = original.clone();
    let at = bytes.windows(7).position(|s| s == b"S=r,T=d").unwrap();
    bytes[at + 6] = b'u';
    assert!(prepare(&bytes, 0x02ffffff, &budget).is_err());
    let mut bytes = original;
    let end = bytes.len();
    bytes[end - 8] = 7; // base 0, differential -1
    bytes[end - 5] = 2;
    assert!(prepare(&bytes, 0x02ffffff, &budget).is_err());
    assert_eq!(budget.used(), 0);
}

#[test]
fn tiled_textures_preserve_metadata_native_blocks_and_cpu_coordinates() {
    use krkr_protocol::texture::Format;
    let size = Size {
        width: 96,
        height: 32,
    };
    let tile = Size {
        width: 32,
        height: 16,
    };
    let tags = vec![
        ("offs_x".into(), "-17".into()),
        ("names".into(), "青子\0眼鏡".into()),
        ("names".into(), "duplicate".into()),
    ];
    for format in [
        Format::Etc1,
        Format::Pvrtc1Rgba4,
        Format::Bc1Rgb,
        Format::Bc3Rgba,
        Format::Bc1RgbVita,
        Format::Bc3RgbaVita,
    ] {
        let blocks = match format {
            Format::Etc1 => [0x12, 0x34, 0x56, 0, 0, 0, 0, 0].repeat(32),
            Format::Pvrtc1Rgba4 => include_bytes!("data/pvrtc-gradient.pvr")[52..].to_vec(),
            Format::Bc1Rgb | Format::Bc1RgbVita => {
                [0x1f, 0, 0xe0, 7, 0xe4, 0xe4, 0xe4, 0xe4].repeat(32)
            }
            Format::Bc3Rgba | Format::Bc3RgbaVita => [
                255, 0, 0x77, 0x88, 0x99, 0x44, 0x33, 0x11, 0x1f, 0, 0xe0, 7, 0xe4, 0xe4, 0xe4,
                0xe4,
            ]
            .repeat(32),
        };
        let single = krkr_image::compressed::ktx_format(tile, format, &blocks).unwrap();
        let ktx = krkr_image::compressed::assemble(size, &vec![single.clone(); 6], &tags).unwrap();
        // Native loading needs only the compressed allocation, even with tags.
        let budget = Budget::new(ktx.len() + 1024);
        let prepared = prepare(&ktx, 0x02ffffff, &budget).unwrap();
        assert_eq!(prepared.size, size);
        assert_eq!(prepared.compressed_upload_bytes(), Some(blocks.len() * 6));
        let held = budget.used();
        let (texture, actual_tags) = prepared.into_compressed_with_tags().unwrap();
        assert_eq!(actual_tags, tags);
        assert_eq!(budget.used(), held);
        for (i, (rect, data)) in texture.tiles().enumerate() {
            assert_eq!(
                (rect.left, rect.top),
                ((i % 3 * 32) as i32, (i / 3 * 16) as i32)
            );
            assert_eq!(data, blocks);
        }
        let cpu_budget = Budget::new(128 * 1024);
        let reference = prepare(&single, 0x02ffffff, &cpu_budget)
            .unwrap()
            .decode(&AtomicBool::new(false))
            .unwrap()
            .pixels
            .main
            .unwrap();
        let decoded = prepare(&ktx, 0x02ffffff, &cpu_budget)
            .unwrap()
            .decode(&AtomicBool::new(false))
            .unwrap();
        assert_eq!(decoded.tags, tags);
        let pixels = decoded.pixels.main.unwrap();
        for y in 0..32usize {
            for x in 0..96usize {
                assert_eq!(
                    &pixels.as_slice()[(y * 96 + x) * 4..][..4],
                    &reference.as_slice()[((y % 16) * 32 + x % 32) * 4..][..4]
                );
            }
        }
        let keyed = prepare(&ktx, 0x00133557, &cpu_budget).unwrap();
        assert!(!keyed.is_compressed_upload());
        assert_eq!(keyed.decode(&AtomicBool::new(false)).unwrap().tags, tags);
        assert!(
            krkr_image::compressed::decode(&texture, &cpu_budget, &AtomicBool::new(true)).is_err()
        );
        drop(texture);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn tiled_array_and_tag_corruption_is_rejected_before_upload() {
    use krkr_protocol::texture::Format;
    let size = Size {
        width: 16,
        height: 8,
    };
    let tile = Size {
        width: 8,
        height: 8,
    };
    let tags = vec![("offset".into(), "-2".into())];
    let original =
        krkr_image::compressed::ktx_tiles(size, tile, Format::Etc1, &[0; 64], &tags).unwrap();
    let canvas = original
        .windows(12)
        .position(|b| b == b"krkr.canvas\0")
        .unwrap()
        + 12;
    let tag = original
        .windows(10)
        .position(|b| b == b"krkr.tags\0")
        .unwrap()
        + 10;
    let budget = Budget::new(65536);
    for (at, value) in [
        (48, 0),
        (48, 3),
        (canvas, 17),
        (canvas + 4, 0),
        (tag, 2),
        (tag + 4, 1025),
        (tag + 8, u32::MAX),
    ] {
        let mut bytes = original.clone();
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
        assert!(prepare(&bytes, 0x02ffffff, &budget).is_err(), "field={at}");
        assert_eq!(budget.used(), 0);
    }
    let mut invalid_utf8 = original.clone();
    invalid_utf8[tag + 12] = 255;
    assert!(prepare(&invalid_utf8, 0x02ffffff, &budget).is_err());
    assert!(prepare(&original[..original.len() - 8], 0x02ffffff, &budget).is_err());
    assert!(krkr_image::compressed::validate_tags(&vec![("a".into(), "x".repeat(65536))]).is_err());
}

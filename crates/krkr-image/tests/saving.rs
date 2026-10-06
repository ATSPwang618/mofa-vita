use krkr_assets::Vfs;
use krkr_image::{
    resolve,
    save::{Format, Request},
};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::sync::atomic::AtomicBool;

fn units(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

#[test]
fn thumbnail_saving_falls_back_to_streaming_under_memory_pressure() {
    let directory = tempfile::tempdir().unwrap();
    let vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let size = Size {
        width: 301,
        height: 171,
    };
    let rgba = vec![0x7f; size.rgba_bytes().unwrap()];
    // Exactly the old row + 64 KiB writer allowance, with no room for a whole
    // encoded thumbnail buffer. Saving must still succeed and release permits.
    let budget = Budget::new(rgba.len() + size.width as usize * 4 + 64 * 1024);
    request(&vfs, "small-budget.bmp", "bmp", size, &rgba, &budget)
        .write(&AtomicBool::new(false))
        .unwrap();
    assert_eq!(budget.used(), 0);
    let bytes = std::fs::read(directory.path().join("small-budget.bmp")).unwrap();
    assert_eq!(&bytes[..2], b"BM");
    assert_eq!(bytes.len(), rgba.len() + 54);
    assert!(bytes[54..].iter().all(|&b| b == 0x7f));
}
fn request(vfs: &Vfs, name: &str, mode: &str, size: Size, rgba: &[u8], budget: &Budget) -> Request {
    let mut main = Bytes::zeroed(rgba.len(), budget).unwrap();
    main.as_mut_slice().copy_from_slice(rgba);
    Request {
        target: vfs.write_plan(&units(name)).unwrap(),
        format: Format::parse(mode).unwrap(),
        pixels: Pixels {
            size,
            main: Some(main),
            province: None,
        },
        tags: vec![
            ("mode".into(), "alpha".into()),
            ("注釈".into(), "漢字=あ,é".into()),
        ],
        budget: budget.clone(),
    }
}
#[test]
fn lossless_formats_preserve_channels_odd_block_edges_and_tlg_tags() {
    let directory = tempfile::tempdir().unwrap();
    let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let budget = Budget::new(32 * 1024 * 1024);
    let cancel = AtomicBool::new(false);
    for size in [
        Size {
            width: 1,
            height: 1,
        },
        Size {
            width: 17,
            height: 11,
        },
    ] {
        let mut seed = 19u32;
        let rgba: Vec<_> = (0..size.rgba_bytes().unwrap())
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        for (mode, ext, alpha) in [
            ("bmp", "bmp", true),
            ("bmp32", "bmp", true),
            ("bmp24", "bmp", false),
            ("png", "png", true),
            ("png24", "png", false),
            ("tlg", "tlg", true),
            ("tlg5", "tlg", true),
            ("tlg524", "tlg", false),
            ("tlg6", "tlg", true),
            ("tlg624", "tlg", false),
        ] {
            let name = format!("export.{ext}");
            request(&vfs, &name, mode, size, &rgba, &budget)
                .write(&cancel)
                .unwrap_or_else(|e| panic!("{mode}: {e}"));
            assert_eq!(budget.used(), 0, "{mode} retained encode memory");
            let decoded =
                resolve::request(&mut vfs, &units(&name), 0x1fffffff, None, budget.clone())
                    .unwrap()
                    .probe(&cancel)
                    .unwrap()
                    .decode(&cancel)
                    .unwrap_or_else(|e| panic!("{mode}: {e}"));
            let mut expected = rgba.clone();
            if !alpha {
                for p in expected.as_chunks_mut::<4>().0 {
                    p[3] = 255;
                }
            }
            assert_eq!(decoded.pixels.size, size);
            assert_eq!(decoded.pixels.main.unwrap().as_slice(), expected, "{mode}");
            if ext == "tlg" {
                assert!(decoded.tags.contains(&("mode".into(), "alpha".into())));
                assert!(decoded.tags.contains(&("注釈".into(), "漢字=あ,é".into())));
            } else {
                assert!(decoded.tags.is_empty());
            }
            assert_eq!(budget.used(), 0);
        }
    }
}
#[test]
fn formats_use_stock_defaults_and_jpeg_is_progressive_420() {
    assert_eq!(
        Format::parse(".tlg6").unwrap(),
        Format::Tlg {
            six: false,
            alpha: true
        }
    );
    assert_eq!(
        Format::parse("jpgabc").unwrap(),
        Format::Jpeg { quality: 10 }
    );
    assert_eq!(
        Format::parse("jpg9x5").unwrap(),
        Format::Jpeg { quality: 95 }
    );
    assert_eq!(
        Format::parse("jpg999").unwrap(),
        Format::Jpeg { quality: 100 }
    );
    assert!(Format::parse("PNG").is_err());
    assert!(Format::parse("jpeg").is_err());
    let directory = tempfile::tempdir().unwrap();
    let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let budget = Budget::new(4 * 1024 * 1024);
    let size = Size {
        width: 19,
        height: 9,
    };
    let rgba = [87, 121, 172, 13].repeat(19 * 9);
    request(&vfs, "photo.png", "jpg", size, &rgba, &budget)
        .write(&AtomicBool::new(false))
        .unwrap();
    let bytes = std::fs::read(directory.path().join("photo.png")).unwrap();
    assert_eq!(
        &bytes[..2],
        &[255, 216],
        "mode determines output, not extension"
    );
    let sof = bytes
        .windows(2)
        .position(|s| s == [255, 194])
        .expect("progressive SOF2");
    assert_eq!(
        &bytes[sof + 9..sof + 19],
        &[3, 0, 0x22, 0, 1, 0x11, 1, 2, 0x11, 1]
    );
    std::fs::rename(
        directory.path().join("photo.png"),
        directory.path().join("photo.jpg"),
    )
    .unwrap();
    let decoded = resolve::request(
        &mut vfs,
        &units("photo.jpg"),
        0x1fffffff,
        None,
        budget.clone(),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap()
    .decode(&AtomicBool::new(false))
    .unwrap();
    for p in decoded.pixels.main.unwrap().as_slice().as_chunks::<4>().0 {
        for (a, b) in p[..3].iter().zip([87u8, 121, 172]) {
            assert!(a.abs_diff(b) <= 3);
        }
        assert_eq!(p[3], 255);
    }
    assert_eq!(budget.used(), 0);
}
#[test]
fn failed_cancelled_and_over_budget_saves_preserve_existing_file_and_release_memory() {
    let directory = tempfile::tempdir().unwrap();
    let size = Size {
        width: 8,
        height: 8,
    };
    let rgba = [1, 2, 3, 4].repeat(64);
    let vfs = Vfs::new(
        directory.path(),
        krkr_assets::Limits {
            max_read_bytes: 70,
            ..Default::default()
        },
    )
    .unwrap();
    let path = directory.path().join("keep.bmp");
    std::fs::write(&path, "original").unwrap();
    let budget = Budget::new(4 * 1024 * 1024);
    for cancelled in [true, false] {
        assert!(
            request(&vfs, "keep.bmp", "bmp", size, &rgba, &budget)
                .write(&AtomicBool::new(cancelled))
                .is_err()
        );
        assert_eq!(budget.used(), 0);
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
    let mut req = request(&vfs, "keep.bmp", "png", size, &rgba, &budget);
    req.budget = Budget::new(0);
    assert!(req.write(&AtomicBool::new(false)).is_err());
    assert_eq!(budget.used(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
}

#[test]
fn bitmap8_uses_stock_palette_phase_and_bottom_up_scanlines() {
    let directory = tempfile::tempdir().unwrap();
    let mut vfs = Vfs::new(directory.path(), Default::default()).unwrap();
    let budget = Budget::new(4 * 1024 * 1024);
    let size = Size {
        width: 4,
        height: 4,
    };
    request(
        &vfs,
        "palette.bmp",
        "bmp8",
        size,
        &[89, 123, 177, 9].repeat(16),
        &budget,
    )
    .write(&AtomicBool::new(false))
    .unwrap();
    let bytes = std::fs::read(directory.path().join("palette.bmp")).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[10..14].try_into().unwrap()), 1078);
    assert_eq!(bytes[28], 8);
    assert_eq!(&bytes[54 + 42 * 4..54 + 43 * 4], &[51, 0, 0, 0]);
    assert_eq!(&bytes[54 + 4..54 + 8], &[0, 0, 51, 0]);
    assert_eq!(
        &bytes[1078..],
        &[
            146, 188, 146, 188, 188, 145, 182, 145, 146, 188, 146, 188, 188, 145, 182, 145
        ]
    );
    let decoded = resolve::request(
        &mut vfs,
        &units("palette.bmp"),
        0,
        Some(size),
        budget.clone(),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap()
    .decode(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(
        decoded.pixels.province.unwrap().as_slice(),
        &[
            188, 145, 182, 145, 146, 188, 146, 188, 188, 145, 182, 145, 146, 188, 146, 188
        ]
    );
    assert_eq!(budget.used(), 0);
}

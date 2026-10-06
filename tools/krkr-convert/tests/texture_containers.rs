use krkr_assets::{Vfs, name::units, xp3::Compression};
use krkr_convert::{
    helper::{Prepared, Target},
    media, psv,
};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::{
    fs,
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

fn icon(kind: u16) -> Vec<u8> {
    // Two independent DIB frames, including an AND mask and a CUR hotspot.
    let mut dib = Vec::new();
    for n in [40u32, 2, 2] {
        dib.extend(n.to_le_bytes());
    }
    dib.extend(1u16.to_le_bytes());
    dib.extend(24u16.to_le_bytes());
    dib.extend([0; 24]);
    dib.extend([3, 2, 1, 6, 5, 4, 0, 0, 0x40, 0, 0, 0]);
    let mut data = vec![0, 0];
    data.extend(kind.to_le_bytes());
    data.extend(2u16.to_le_bytes());
    for frame in 0..2 {
        data.extend([2, 1, 0, 0]);
        data.extend(1u16.to_le_bytes());
        data.extend(if kind == 2 { 0u16 } else { 24 }.to_le_bytes());
        data.extend((dib.len() as u32).to_le_bytes());
        data.extend((38 + frame * dib.len() as u32).to_le_bytes());
    }
    data.extend(&dib);
    dib[40] = 99;
    data.extend(dib);
    data
}

fn layered_psd() -> Vec<u8> {
    // A 2x2 RGB document with one named layer and uncompressed channels.
    let mut layer = 1u16.to_be_bytes().to_vec();
    for n in [0u32, 0, 2, 2] {
        layer.extend(n.to_be_bytes());
    }
    layer.extend(3u16.to_be_bytes());
    for channel in 0..3u16 {
        layer.extend(channel.to_be_bytes());
        layer.extend(6u32.to_be_bytes());
    }
    layer.extend(b"8BIMnorm");
    layer.extend([255, 0, 0, 0]);
    layer.extend(12u32.to_be_bytes());
    layer.extend([0; 8]); // Empty layer mask and blending ranges.
    layer.extend(b"\x03rgb");
    for value in [20, 70, 140] {
        layer.extend([0, 0]);
        layer.extend([value; 4]);
    }
    let mut data = b"8BPS\0\x01".to_vec();
    data.extend([0; 6]);
    data.extend(3u16.to_be_bytes());
    data.extend(2u32.to_be_bytes());
    data.extend(2u32.to_be_bytes());
    data.extend(8u16.to_be_bytes());
    data.extend(3u16.to_be_bytes());
    data.extend([0; 8]); // Empty color-mode data and image resources.
    data.extend((layer.len() as u32 + 8).to_be_bytes());
    data.extend((layer.len() as u32).to_be_bytes());
    data.extend(layer);
    data.extend([0; 4]); // Empty global mask.
    data.extend([0, 0]); // Raw merged image.
    for value in [20, 70, 140] {
        data.extend([value; 4]);
    }
    data
}

fn retained_images() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("scenario/mwheel.cur", icon(2)),
        ("badge.ico", icon(1)),
        ("scene.psd", layered_psd()),
        (
            "cached.ktx",
            krkr_image::compressed::ktx_format(
                Size {
                    width: 8,
                    height: 8,
                },
                krkr_protocol::texture::Format::Bc1Rgb,
                &[0; 32],
            )
            .unwrap(),
        ),
    ]
}

fn write_images(root: &Path, images: &[(&str, Vec<u8>)]) {
    for (name, data) in images {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }
}

#[test]
fn helper_auto_preserves_special_containers_in_loose_files_and_xp3() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let images = retained_images();
    write_images(&game, &images);
    let budget = Budget::new(8 * 1024 * 1024);
    let mut pixels = Bytes::zeroed(64 * 64 * 4, &budget).unwrap();
    for pixel in pixels.as_mut_slice().as_chunks_mut::<4>().0.iter_mut() {
        pixel.copy_from_slice(&[20, 70, 140, 255]);
    }
    krkr_image::save::Request {
        target: krkr_assets::WritePlan::local(game.join("background.png"), 1024 * 1024).unwrap(),
        format: krkr_image::save::Format::Png { alpha: true },
        pixels: Pixels {
            size: Size {
                width: 64,
                height: 64,
            },
            main: Some(pixels),
            province: None,
        },
        tags: Vec::new(),
        budget,
    }
    .write(&AtomicBool::new(false))
    .unwrap();
    let packed = temp.path().join("data.xp3");
    krkr_assets::xp3::offline::pack_directory(
        &game,
        &packed,
        Compression::None,
        Default::default(),
    )
    .unwrap();
    fs::rename(packed, game.join("data.xp3")).unwrap();
    let original_archive = fs::read(game.join("data.xp3")).unwrap();
    let output = temp.path().join("game-psv");
    let prepared = Prepared::new(&game, &output, None).unwrap();
    let inventory = prepared.inspect(&Default::default()).unwrap();
    let mut options = psv::Options::vita(Size {
        width: 1024,
        height: 576,
    });
    options.texture_auto = true;
    let report = prepared
        .convert(inventory, Target::Psv(options), &Default::default())
        .unwrap();
    assert_eq!(report.parts.len(), 2);
    for part in &report.parts {
        assert_eq!(part.compressed_textures, 1);
        assert_eq!(part.texture_skips.len(), images.len());
    }
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    for prefix in ["", "data.xp3>"] {
        for (name, bytes) in &images {
            let plan = vfs.plan(&units(&format!("{prefix}{name}"))).unwrap();
            assert_eq!(plan.read(0).unwrap(), *bytes, "{prefix}{name}");
            if name.ends_with(".cur") {
                let cursor =
                    krkr_image::cursor::read(plan, Budget::new(1024), &AtomicBool::new(false))
                        .unwrap();
                assert_eq!(cursor.hotspot, (1, 0));
            } else if name.ends_with(".psd") {
                let document =
                    krkr_image::psd::Document::load(Arc::new(plan), Default::default()).unwrap();
                assert_eq!(document.layers.len(), 1);
            }
        }
        assert!(
            vfs.plan(&units(&format!("{prefix}background.png")))
                .unwrap()
                .read(0)
                .unwrap()
                .starts_with(krkr_image::compressed::MAGIC)
        );
    }
    assert_eq!(fs::read(game.join("data.xp3")).unwrap(), original_archive);
    for (name, bytes) in images {
        assert_eq!(fs::read(game.join(name)).unwrap(), bytes);
    }
}

#[test]
fn explicit_selection_still_rejects_flattening_cursors_icons_and_layers() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    write_images(&source, &retained_images());
    for name in ["scenario/mwheel.cur", "badge.ico", "scene.psd"] {
        let mut options = psv::Options::vita(Size {
            width: 1024,
            height: 576,
        });
        options.texture_globs.push(name.into());
        let output = temp.path().join("rejected");
        let error = psv::build(
            &source,
            Some(&output),
            &options,
            &media::Tools::default(),
            |_| {},
        )
        .err()
        .expect("manual flattening must fail");
        assert!(
            error.contains("explicit format-specific conversion"),
            "{error}"
        );
        assert!(!output.exists());
    }
}

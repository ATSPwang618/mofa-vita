use krkr_assets::{
    Vfs,
    name::units,
    xp3::{Compression, Writer},
};
use krkr_convert::{
    archive,
    helper::{Prepared, Target},
    media::{self, Consistency},
    psv,
};
use krkr_protocol::{
    budget::Budget,
    graphics::Size,
    pixels::{Bytes, Pixels},
};
use std::{fs, io::Cursor, path::Path, process::Command, sync::atomic::AtomicBool};

fn cli(args: &[&str], cwd: &Path, success: bool) -> std::process::Output {
    let output = Command::new(env!("CARGO_BIN_EXE_krkr-convert"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
fn image(path: &Path, size: Size, rgba: &[u8]) {
    let budget = Budget::new(32 * 1024 * 1024);
    let mut pixels = Bytes::zeroed(rgba.len(), &budget).unwrap();
    pixels.as_mut_slice().copy_from_slice(rgba);
    krkr_image::save::Request {
        target: krkr_assets::WritePlan::local(path, 32 * 1024 * 1024).unwrap(),
        format: krkr_image::save::Format::Png { alpha: true },
        pixels: Pixels {
            size,
            main: Some(pixels),
            province: None,
        },
        tags: Vec::new(),
        budget,
    }
    .write(&AtomicBool::new(false))
    .unwrap();
}
fn ffmpeg(args: &[&str], output: &Path) {
    let out = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-xerror"])
        .args(args)
        .arg(output)
        .output()
        .expect("install ffmpeg to run media integration tests");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn normalized_game(source: &Path) -> std::path::PathBuf {
    let output = source.with_file_name(format!(
        "{}-normalized-fixture",
        source.file_name().unwrap().to_string_lossy()
    ));
    let prepared = Prepared::new(source, &output, None).unwrap();
    let inventories = prepared.inspect(&Default::default()).unwrap();
    prepared
        .convert(inventories, Target::Normalize, &Default::default())
        .unwrap();
    output
}

#[test]
fn cli_default_packed_bc_preserves_aliases_and_reports_storage_costs() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 128,
        height: 128,
    };
    let rgba: Vec<u8> = (0..128 * 128)
        .flat_map(|i| {
            if i % 128 < 64 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            }
        })
        .collect();
    image(&source.join("cg.png"), size, &rgba);
    let result = cli(
        &[
            "--no-progress",
            "psv",
            "source",
            "--canvas",
            "960x544",
            "--texture-auto",
            "--texture-quality",
            "fast",
            "--output",
            "out",
        ],
        dir.path(),
        true,
    );
    let output = dir.path().join("out");
    let bytes = fs::read(output.join("cg.kbct")).unwrap();
    let stats = krkr_image::packed_bc::storage_stats(&bytes).unwrap();
    assert!(stats.0 > 0);
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let storage = &report["entries"][0]["texture_storage"];
    assert_eq!(
        storage["encoded_bytes"].as_u64().unwrap(),
        bytes.len() as u64
    );
    assert_eq!(storage["gpu_bytes"].as_u64().unwrap(), stats.2 as u64);
    assert!(bytes.len() < stats.2);
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("cg.png"),
        0x02ffffff,
        None,
        Budget::new(4 << 20),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert!(prepared.is_compressed_upload());
    assert_eq!(prepared.into_compressed_with_tags().unwrap().0.size, size);
}

#[test]
fn at9_probe_uses_shared_header_and_detects_real_extension() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("sound.at9"),
        include_bytes!("../../../crates/krkr-audio/tests/data/timeline.at9"),
    )
    .unwrap();
    let report = media::inspect(temp.path(), &media::Tools::default()).unwrap();
    let entry = &report.entries[0];
    assert_eq!(entry.consistency, Consistency::Match);
    let media = entry.media.as_ref().unwrap();
    assert_eq!(media.container, "at9");
    assert_eq!(media.tracks[0].samples, Some(4003));
}

#[test]
fn small_uncompressed_strips_keep_pixels_while_large_images_still_scale() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    for (name, width, height) in [
        ("button_states.png", 465, 24),
        ("spark.png", 32, 32),
        ("large_strip.png", 1024, 128),
        ("background.png", 1024, 576),
    ] {
        let size = Size { width, height };
        let data: Vec<_> = (0..width * height)
            .flat_map(|i| {
                [
                    (i * 37) as u8,
                    (i * 13) as u8,
                    (i * 7) as u8,
                    (i * 11) as u8,
                ]
            })
            .collect();
        image(&source.join(name), size, &data);
    }
    let output = temp.path().join("converted");
    let report = psv::build(
        &source,
        Some(&output),
        &psv::Options::vita(Size {
            width: 1024,
            height: 576,
        }),
        &Default::default(),
        |_| {},
    )
    .unwrap();
    for name in ["button_states.png", "spark.png"] {
        assert_eq!(
            fs::read(source.join(name)).unwrap(),
            fs::read(output.join(name)).unwrap()
        );
        assert!(!output.join(format!("{name}.krkr-scale")).exists());
    }
    for (name, expected) in [
        ("large_strip.png", [960, 120]),
        ("background.png", [960, 540]),
    ] {
        let entry = report.entries.iter().find(|e| e.source == name).unwrap();
        assert_eq!(entry.stored_size, Some(expected));
    }
}

#[test]
#[ignore = "requires KRKR_AT9_TOOL pointing to an externally provided at9tool"]
fn at9_conversion_preserves_44100_timeline_aliases_loops_and_input() {
    let tool = std::env::var_os("KRKR_AT9_TOOL").expect("set KRKR_AT9_TOOL");
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let output = temp.path().join("psv");
    fs::create_dir(&source).unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=701:sample_rate=44100",
            "-af",
            "atrim=end_sample=4003",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
        ],
        &source.join("bgm.wav"),
    );
    let original = fs::read(source.join("bgm.wav")).unwrap();
    let sli = b"LoopStart=768\nLoopLength=3000\n";
    fs::write(source.join("bgm.wav.sli"), sli).unwrap();
    let report = psv::build(
        &source,
        Some(&output),
        &psv::Options {
            canvas: Size {
                width: 960,
                height: 544,
            },
            target: Size {
                width: 960,
                height: 544,
            },
            texture_globs: Vec::new(),
            texture_auto: false,
            texture_quality: Default::default(),
            texture_storage: Default::default(),
            at9: Some(krkr_convert::at9::Options {
                tool: tool.into(),
                globs: vec!["*.wav".into()],
            }),
        },
        &media::Tools::default(),
        |_| {},
    )
    .unwrap();
    let audio = report
        .entries
        .iter()
        .find(|e| e.source == "bgm.wav")
        .unwrap();
    assert_eq!(audio.action, "audio_at9");
    assert_eq!(fs::read(source.join("bgm.wav")).unwrap(), original);
    assert_eq!(fs::read(output.join("bgm.wav.sli")).unwrap(), sli);
    assert!(!output.join("bgm.wav").exists());
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let plan = vfs.plan(&units("bgm.wav")).unwrap();
    let h = krkr_audio::at9::inspect(&mut plan.open().unwrap(), plan.bytes)
        .unwrap()
        .unwrap();
    assert_eq!(
        (h.format.rate, h.codec_rate, h.format.frames),
        (44100, 48000, 4003)
    );
    krkr_audio::loops::Information::parse(sli)
        .unwrap()
        .validate_frames(h.format.frames)
        .unwrap();
    assert!(plan.bytes < original.len() as u64);
    let archive = temp.path().join("data.xp3");
    krkr_assets::xp3::offline::pack_directory(
        &output,
        &archive,
        Compression::None,
        Default::default(),
    )
    .unwrap();
    let mut vfs = Vfs::new(temp.path(), Default::default()).unwrap();
    let plan = vfs.plan(&units("data.xp3>bgm.wav")).unwrap();
    assert_eq!(
        krkr_audio::at9::inspect(&mut plan.open().unwrap(), plan.bytes)
            .unwrap()
            .unwrap()
            .format
            .rate,
        44100
    );
}

#[test]
fn helper_compresses_original_names_across_loose_and_xp3_parts() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let assets = temp.path().join("assets");
    let scripts = temp.path().join("scripts");
    for dir in [&game, &assets, &scripts] {
        fs::create_dir(dir).unwrap();
    }
    let size = Size {
        width: 16,
        height: 8,
    };
    image(
        &game.join("loose.png"),
        size,
        &[40, 80, 120, 255].repeat(128),
    );
    // PNG content, BMP script name, glob metacharacters, and a normalization
    // name collision: all must survive selection before names are repaired.
    image(
        &assets.join("bg[1].bmp"),
        size,
        &[40, 80, 120, 255].repeat(128),
    );
    image(
        &assets.join("bg[1].png"),
        size,
        &[20, 20, 20, 127].repeat(128),
    );
    fs::write(scripts.join("startup.tjs"), "var unchanged = 1;").unwrap();
    offline_pack(&assets, &game.join("data.xp3"));
    offline_pack(&scripts, &game.join("script.xp3"));
    let original = fs::read(game.join("data.xp3")).unwrap();
    let normalized = normalized_game(&game);
    let output = temp.path().join("game-psv");
    let prepared = Prepared::new(&normalized, &output, None).unwrap();
    let inventory = prepared.inspect(&Default::default()).unwrap();
    let mut options = psv::Options::vita(size);
    options.texture_globs = vec!["bg[[]1].bmp".into(), "loose.png".into()];
    let counts = krkr_convert::helper::hardware_counts(&inventory, &options).unwrap();
    assert_eq!((counts.at9, counts.textures), (0, 2));
    let report = prepared
        .convert(inventory, Target::Psv(options), &Default::default())
        .unwrap();
    assert_eq!(
        report
            .parts
            .iter()
            .map(|p| p.compressed_textures)
            .collect::<Vec<_>>(),
        [1, 1, 0]
    );
    assert_eq!(report.parts[1].converted_images, 1);
    assert_eq!(report.parts[1].adjusted, 0);
    assert_eq!(fs::read(game.join("data.xp3")).unwrap(), original);
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    for name in ["loose.png", "data.xp3>bg[1].bmp"] {
        let prepared = krkr_image::resolve::request(
            &mut vfs,
            &units(name),
            0x1fffffff,
            None,
            Budget::new(4096),
        )
        .unwrap()
        .probe(&AtomicBool::new(false))
        .unwrap();
        assert_eq!(prepared.size, size);
        assert_eq!(prepared.into_compressed().unwrap().data().len(), 64);
    }
    assert!(
        vfs.plan(&units("data.xp3>bg[1].png"))
            .unwrap()
            .read(0)
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    assert_eq!(
        vfs.plan(&units("script.xp3>startup.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"var unchanged = 1;"
    );
}

fn colorful(size: Size, alpha: bool) -> Vec<u8> {
    (0..size.height)
        .flat_map(|y| {
            (0..size.width).flat_map(move |x| {
                [
                    (x * 3) as u8,
                    (y * 3) as u8,
                    (50 + (x + y) / 2) as u8,
                    if alpha {
                        ((x * 255) / (size.width - 1)) as u8
                    } else {
                        255
                    },
                ]
            })
        })
        .collect()
}

#[test]
fn helper_auto_screens_pixels_scripts_and_cross_archive_companions() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let masks = temp.path().join("masks");
    fs::create_dir(&game).unwrap();
    fs::create_dir(&masks).unwrap();
    let size = Size {
        width: 64,
        height: 64,
    };
    for name in ["background.bmp", "paired.png", "special.png"] {
        image(&game.join(name), size, &colorful(size, false));
    }
    image(&game.join("actor.png"), size, &colorful(size, true));
    image(
        &game.join("grey.png"),
        size,
        &[70, 70, 70, 255].repeat(4096),
    );
    image(
        &masks.join("paired_m.png"),
        size,
        &[90, 90, 90, 255].repeat(4096),
    );
    fs::write(
        game.join("startup.tjs"),
        "layer.loadImages('special.png', key);",
    )
    .unwrap();
    offline_pack(&masks, &game.join("masks.xp3"));
    let normalized = normalized_game(&game);
    let output = temp.path().join("game-psv");
    let prepared = Prepared::new(&normalized, &output, None).unwrap();
    let inventory = prepared.inspect(&Default::default()).unwrap();
    let mut options = psv::Options::vita(size);
    options.texture_auto = true;
    let report = prepared
        .convert(inventory, Target::Psv(options), &Default::default())
        .unwrap();
    assert_eq!(report.parts[0].compressed_textures, 2);
    assert_eq!(report.parts[0].transparent_textures, 1);
    assert_eq!(report.parts[0].texture_skips.len(), 3);
    assert_eq!(report.parts[1].texture_skips.len(), 1);
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    assert!(
        vfs.plan(&units("background.bmp"))
            .unwrap()
            .read(0)
            .unwrap()
            .starts_with(krkr_image::compressed::MAGIC)
    );
    assert!(
        vfs.plan(&units("actor.png"))
            .unwrap()
            .read(0)
            .unwrap()
            .starts_with(krkr_image::compressed::MAGIC)
    );
    for name in ["paired.png", "special.png", "grey.png"] {
        assert!(
            vfs.plan(&units(name))
                .unwrap()
                .read(0)
                .unwrap()
                .starts_with(b"\x89PNG")
        );
    }
}

#[test]
#[ignore = "requires KRKR_AT9_TOOL"]
fn helper_auto_converts_transparency_and_audio_without_resource_patterns() {
    let at9 = std::env::var_os("KRKR_AT9_TOOL").expect("set KRKR_AT9_TOOL");
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let pictures = temp.path().join("pictures");
    fs::create_dir(&game).unwrap();
    fs::create_dir(&pictures).unwrap();
    let size = Size {
        width: 70,
        height: 66,
    };
    image(&pictures.join("actor.bmp"), size, &colorful(size, true));
    image(
        &pictures.join("background.png"),
        size,
        &colorful(size, false),
    );
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=701:sample_rate=44100",
            "-af",
            "atrim=end_sample=4003",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
        ],
        &game.join("bgm.wav"),
    );
    offline_pack(&pictures, &game.join("pictures.xp3"));
    let original = fs::read(game.join("pictures.xp3")).unwrap();
    let normalized = normalized_game(&game);
    let output = temp.path().join("game-psv");
    let prepared = Prepared::new(&normalized, &output, None).unwrap();
    let inventory = prepared.inspect(&Default::default()).unwrap();
    let mut options = psv::Options::vita(size);
    options.texture_auto = true;
    options.at9 = Some(krkr_convert::at9::Options {
        tool: at9.into(),
        globs: vec!["*".into()],
    });
    let report = prepared
        .convert(inventory, Target::Psv(options), &Default::default())
        .unwrap();
    assert_eq!(report.parts[0].hardware_audio, 1);
    assert_eq!(report.parts[1].compressed_textures, 2);
    assert_eq!(report.parts[1].transparent_textures, 1);
    assert!(report.parts[1].texture_skips.is_empty());
    assert_eq!(fs::read(game.join("pictures.xp3")).unwrap(), original);
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("pictures.xp3>actor.bmp"),
        0x02ffffff,
        None,
        Budget::new(128 * 1024),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(prepared.size, size);
    assert_eq!(
        prepared.upload_size(),
        Size {
            width: 128,
            height: 128
        }
    );
    let texture = prepared.into_compressed().unwrap();
    assert_eq!(texture.format, krkr_protocol::texture::Format::Bc3RgbaVita);
    assert_eq!(texture.data().len(), 8192);
    let decoded =
        krkr_image::compressed::decode(&texture, &Budget::new(128 * 1024), &AtomicBool::new(false))
            .unwrap()
            .main
            .unwrap();
    // Interior samples avoid the format's wrapped endpoint interpolation at the
    // outermost border; verify orientation and the full transparency gradient.
    let alpha = |x: usize| decoded.as_slice()[(64 * 128 + x) * 4 + 3];
    assert!(
        alpha(8) < 40 && alpha(120) > 210,
        "{} .. {}",
        alpha(8),
        alpha(120)
    );
}

#[test]
fn transparency_quality_fallback_preserves_pixels_tags_and_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 128,
        height: 128,
    };
    let mut data = [52, 119, 207, 255].repeat(128 * 128);
    for (i, p) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        p[3] = (((i % 128) * 53 + (i / 128) * 17) % 256) as u8;
    } // Rapid alpha changes cannot be represented accurately at BC3 alpha endpoints.
    let budget = Budget::new(4 << 20);
    let mut main = Bytes::zeroed(data.len(), &budget).unwrap();
    main.as_mut_slice().copy_from_slice(&data);
    let tags = vec![("origin_x".into(), "-17".into())];
    krkr_image::save::Request {
        target: krkr_assets::WritePlan::local(source.join("actor.tlg"), 1 << 20).unwrap(),
        format: krkr_image::save::Format::Tlg {
            six: false,
            alpha: true,
        },
        pixels: Pixels {
            size,
            main: Some(main),
            province: None,
        },
        tags: tags.clone(),
        budget,
    }
    .write(&AtomicBool::new(false))
    .unwrap();
    let output = temp.path().join("output");
    let mut options = psv::Options::vita(size);
    options.texture_auto = true;
    let report = psv::build(
        &source,
        Some(&output),
        &options,
        &Default::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.entries[0].action, "image_lossless_quality");
    assert!(
        report.entries[0]
            .texture_skip
            .as_ref()
            .unwrap()
            .contains("compression")
    );
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let decoded = krkr_image::resolve::request(
        &mut vfs,
        &units("actor.tlg"),
        0x02ffffff,
        None,
        Budget::new(1 << 20),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap()
    .decode(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(decoded.pixels.size, size);
    let pixels = decoded.pixels.main.unwrap();
    for (actual, expected) in pixels
        .as_slice()
        .as_chunks::<4>()
        .0
        .iter()
        .zip(data.as_chunks::<4>().0.iter())
    {
        assert_eq!(actual[3], expected[3]);
        if expected[3] != 0 {
            assert_eq!(actual, expected);
        }
    }
    assert_eq!(decoded.tags, tags);
}

#[test]
fn pvrtextool_keeps_nearly_opaque_image_compressed() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 128,
        height: 128,
    };
    let mut pixels = [52, 119, 207, 255].repeat(128 * 128);
    pixels[..4].fill(0);
    image(&source.join("background.png"), size, &pixels);
    let mut options = psv::Options::vita(size);
    options.texture_auto = true;
    let output = temp.path().join("output");
    let report = psv::build(
        &source,
        Some(&output),
        &options,
        &Default::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(
        report.entries[0].action, "image_bc3",
        "{:?}",
        report.entries[0].texture_skip
    );
    assert!(report.entries[0].texture_skip.is_none());
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("background.png"),
        0x02ffffff,
        None,
        Budget::new(1 << 20),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    let texture = prepared.into_compressed().unwrap();
    assert_eq!(texture.format, krkr_protocol::texture::Format::Bc3RgbaVita);
    assert_eq!(texture.data().len(), 128 * 128);
    let decoded =
        krkr_image::compressed::decode(&texture, &Budget::new(1 << 20), &AtomicBool::new(false))
            .unwrap()
            .main
            .unwrap();
    for y in 8..120 {
        for x in 8..120 {
            let pixel = &decoded.as_slice()[(y * 128 + x) * 4..][..4];
            assert_eq!(pixel[3], 255, "opaque interior became translucent");
            for c in 0..3 {
                assert!(pixel[c].abs_diff(pixels[4 + c]) <= 5);
            }
        }
    }
}

#[test]
fn helper_hardware_rejects_unmatched_patterns_and_unsafe_images_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let assets = temp.path().join("assets");
    fs::create_dir(&game).unwrap();
    fs::create_dir(&assets).unwrap();
    let size = Size {
        width: 8,
        height: 8,
    };
    image(&game.join("bg.png"), size, &[40, 80, 120, 255].repeat(64));
    image(
        &assets.join("mask_m.bmp"),
        size,
        &[20, 20, 20, 255].repeat(64),
    );
    image(
        &assets.join("alpha.bmp"),
        size,
        &[20, 20, 20, 127].repeat(64),
    );
    offline_pack(&assets, &game.join("data.xp3"));
    let original = fs::read(game.join("data.xp3")).unwrap();
    let normalized = normalized_game(&game);
    for pattern in ["missing*", "[", "mask_m.bmp"] {
        let output = temp.path().join("game-psv");
        let prepared = Prepared::new(&normalized, &output, None).unwrap();
        let inventory = prepared.inspect(&Default::default()).unwrap();
        let mut options = psv::Options::vita(size);
        options.texture_globs = vec![pattern.into()];
        if pattern.ends_with(".bmp") {
            // Convert the loose background first, then fail in an archive.
            options.texture_globs.push("bg.png".into());
        }
        let error = prepared
            .convert(inventory, Target::Psv(options), &Default::default())
            .err()
            .unwrap();
        assert!(
            error.contains("BC1") || error.contains("texture") || error.contains("lossy"),
            "{error}"
        );
        assert!(!output.exists());
        assert_eq!(fs::read(game.join("data.xp3")).unwrap(), original);
    }
    let output = temp.path().join("game-psv");
    let prepared = Prepared::new(&normalized, &output, None).unwrap();
    let inventory = prepared.inspect(&Default::default()).unwrap();
    let mut options = psv::Options::vita(size);
    options.at9 = Some(krkr_convert::at9::Options {
        tool: "missing-tool".into(),
        globs: vec!["*".into()],
    });
    assert!(
        prepared
            .convert(inventory, Target::Psv(options), &Default::default())
            .err()
            .unwrap()
            .contains("did not select")
    );
    assert!(!output.exists());
}

#[test]
#[ignore = "requires KRKR_AT9_TOOL"]
fn helper_at9_and_bc1_share_a_transaction_across_separate_archives() {
    let tool = std::env::var_os("KRKR_AT9_TOOL").expect("set KRKR_AT9_TOOL");
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let audio = temp.path().join("audio");
    let pictures = temp.path().join("pictures");
    for dir in [&game, &audio, &pictures] {
        fs::create_dir(dir).unwrap();
    }
    fs::write(game.join("startup.tjs"), "var unchanged = 1;").unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=701:sample_rate=44100",
            "-af",
            "atrim=end_sample=4003",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ],
        &audio.join("bgm[1].ogg"),
    );
    let sli = b"LoopStart=768\nLoopLength=3000\n";
    fs::write(audio.join("bgm[1].ogg.sli"), sli).unwrap();
    fs::write(
        audio.join("existing.at9"),
        include_bytes!("../../../crates/krkr-audio/tests/data/timeline.at9"),
    )
    .unwrap();
    let size = Size {
        width: 16,
        height: 8,
    };
    image(
        &pictures.join("bg.bmp"),
        size,
        &[40, 80, 120, 255].repeat(128),
    );
    offline_pack(&audio, &game.join("audio.xp3"));
    offline_pack(&pictures, &game.join("pictures.xp3"));
    let original = fs::read(game.join("audio.xp3")).unwrap();
    let normalized = normalized_game(&game);
    let output = temp.path().join("game-psv");
    let prepared = Prepared::new(&normalized, &output, None).unwrap();
    let inventory = prepared.inspect(&Default::default()).unwrap();
    let mut options = psv::Options::vita(size);
    options.at9 = Some(krkr_convert::at9::Options {
        tool: tool.into(),
        globs: vec!["*".into()],
    });
    options.texture_globs = vec!["*.bmp".into()];
    let counts = krkr_convert::helper::hardware_counts(&inventory, &options).unwrap();
    assert_eq!((counts.at9, counts.textures), (1, 1));
    let report = prepared
        .convert(inventory, Target::Psv(options), &Default::default())
        .unwrap();
    assert_eq!(
        report
            .parts
            .iter()
            .map(|p| (p.hardware_audio, p.compressed_textures))
            .collect::<Vec<_>>(),
        [(0, 0), (1, 0), (0, 1)]
    );
    assert_eq!(fs::read(game.join("audio.xp3")).unwrap(), original);
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let plan = vfs.plan(&units("audio.xp3>bgm[1].ogg")).unwrap();
    let header = krkr_audio::at9::inspect(&mut plan.open().unwrap(), plan.bytes)
        .unwrap()
        .unwrap();
    assert_eq!(
        (header.format.rate, header.codec_rate, header.format.frames),
        (44100, 48000, 4003)
    );
    assert_eq!(
        vfs.plan(&units("audio.xp3>bgm[1].ogg.sli"))
            .unwrap()
            .read(0)
            .unwrap(),
        sli
    );
    assert_eq!(
        vfs.plan(&units("audio.xp3>existing.at9"))
            .unwrap()
            .read(0)
            .unwrap(),
        include_bytes!("../../../crates/krkr-audio/tests/data/timeline.at9")
    );
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("pictures.xp3>bg.bmp"),
        0x1fffffff,
        None,
        Budget::new(4096),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(prepared.size, size);
    assert!(prepared.into_compressed().is_ok());
}

#[test]
fn helper_preserves_archive_paths_and_sources_without_reprobing_static_images() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let assets = temp.path().join("assets");
    fs::create_dir_all(&game).unwrap();
    fs::create_dir_all(assets.join("system")).unwrap();
    fs::write(
        assets.join("system/Config.tjs"),
        ";scWidth = 1280; ;scHeight = 720;",
    )
    .unwrap();
    fs::write(
        game.join("startup.tjs"),
        "Storages.addAutoPath('data.xp3>');",
    )
    .unwrap();
    image(
        &assets.join("sprite.bmp"),
        Size {
            width: 64,
            height: 32,
        },
        &[10, 20, 30, 200].repeat(64 * 32),
    );
    offline_pack(&assets, &game.join("data.xp3"));
    let original = fs::read(game.join("data.xp3")).unwrap();
    let normalized = normalized_game(&game);
    let output = temp.path().join("game-psv");
    let prepared = Prepared::new(&normalized, &output, None).unwrap();
    assert_eq!(
        prepared.canvases().unwrap(),
        [Size {
            width: 1280,
            height: 720
        }]
    );
    let inventory = prepared.inspect(&Default::default()).unwrap();
    // This image-only workflow must reuse the inventory and inspect animation
    // chunks directly; any additional external probe would fail this conversion.
    let tools = media::Tools {
        ffmpeg: temp.path().join("missing-ffmpeg"),
        ffprobe: temp.path().join("missing-ffprobe"),
    };
    let report = prepared
        .convert(
            inventory,
            Target::psv(Size {
                width: 1280,
                height: 720,
            }),
            &tools,
        )
        .unwrap();
    assert_eq!(report.parts[1].adjusted, 0);
    assert_eq!(report.parts[1].scaled_images, 0);
    assert_eq!(fs::read(game.join("data.xp3")).unwrap(), original);
    assert!(output.join("startup.tjs").is_file());
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let stored = vfs
        .plan(&units("data.xp3>sprite.bmp"))
        .unwrap()
        .read(0)
        .unwrap();
    assert!(stored.starts_with(b"\x89PNG\r\n\x1a\n"));
    // Resolve through the engine as well: logical sizes survive archive repacking.
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("data.xp3>sprite.bmp"),
        0x02ffffff,
        None,
        Budget::new(32 * 1024 * 1024),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(
        prepared.size,
        Size {
            width: 64,
            height: 32
        }
    );
    assert_eq!(
        prepared.upload_size(),
        Size {
            width: 64,
            height: 32
        }
    );
}

fn offline_pack(source: &Path, output: &Path) {
    krkr_assets::xp3::offline::pack_directory(
        source,
        output,
        Compression::Zlib,
        Default::default(),
    )
    .unwrap();
}

#[test]
fn normalize_cli_preserves_webp_aac_and_scripts_with_collisions_and_repeat_runs() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("game");
    fs::create_dir_all(source.join("art")).unwrap();
    image(
        &root.join("input.png"),
        Size {
            width: 16,
            height: 8,
        },
        &[20, 40, 60, 255].repeat(128),
    );
    ffmpeg(
        &[
            "-i",
            root.join("input.png").to_str().unwrap(),
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            "-f",
            "webp",
        ],
        &source.join("art/背景.png"),
    );
    let webp = fs::read(source.join("art/背景.png")).unwrap();
    // Keep an existing canonical name; normalization must choose another leaf.
    fs::write(source.join("art/背景.webp"), &webp).unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=0.1",
            "-c:a",
            "aac",
            "-f",
            "adts",
        ],
        &source.join("voice.wav"),
    );
    let aac = fs::read(source.join("voice.wav")).unwrap();
    let script = b"var picture='art/\xe8\x83\x8c\xe6\x99\xaf.png'; var voice='voice.wav';";
    fs::write(source.join("startup.tjs"), script).unwrap();
    fs::write(
        source.join("voice.wav.sli"),
        "#2.00\nLink {From=4000;To=1000;}",
    )
    .unwrap();
    let result = cli(&["normalize", "game"], root, true);
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["parts"][0]["links"].as_array().unwrap().len(), 2);
    let output = root.join("game-normalized");
    assert!(!output.join("art/背景.png").exists());
    assert!(!output.join("voice.wav").exists());
    assert_eq!(fs::read(output.join("art/背景.png.webp")).unwrap(), webp);
    assert_eq!(fs::read(output.join("art/背景.webp")).unwrap(), webp);
    assert_eq!(fs::read(output.join("voice.aac")).unwrap(), aac);
    assert_eq!(fs::read(output.join("startup.tjs")).unwrap(), script);
    assert_eq!(
        fs::read(output.join("voice.wav.sli")).unwrap(),
        fs::read(source.join("voice.wav.sli")).unwrap()
    );
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    assert_eq!(
        vfs.plan(&units("art/背景.png")).unwrap().read(0).unwrap(),
        webp
    );
    assert_eq!(vfs.plan(&units("voice.wav")).unwrap().read(0).unwrap(), aac);
    assert_eq!(fs::read(source.join("art/背景.png")).unwrap(), webp);
    assert_eq!(fs::read(source.join("voice.wav")).unwrap(), aac);
    let result = cli(&["normalize", "game-normalized", "-o", "again"], root, true);
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["parts"][0]["adjusted"], 0);
    let first = media::inspect(&output, &Default::default()).unwrap();
    let second = media::inspect(&root.join("again"), &Default::default()).unwrap();
    assert_eq!(
        first
            .entries
            .iter()
            .map(|e| (&e.path, &e.source_sha256))
            .collect::<Vec<_>>(),
        second
            .entries
            .iter()
            .map(|e| (&e.path, &e.source_sha256))
            .collect::<Vec<_>>()
    );
}

#[test]
fn normalized_links_keep_masks_provinces_and_logical_sizes_during_psv_scaling() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("game");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 64,
        height: 32,
    };
    for (name, pixel) in [
        ("sprite.bmp", [255, 0, 0, 255]),
        ("sprite.png", [0, 255, 0, 255]),
        ("sprite_m.bmp", [100, 100, 100, 255]),
        ("sprite_p.bmp", [10, 10, 10, 255]),
        ("sprite_p.png", [20, 20, 20, 255]),
    ] {
        image(&source.join(name), size, &pixel.repeat(64 * 32));
    }
    let province = fs::read(source.join("sprite_p.bmp")).unwrap();
    let normalized = temp.path().join("normalized");
    let prepared = Prepared::new(&source, &normalized, None).unwrap();
    let tools = Default::default();
    let inventories = prepared.inspect(&tools).unwrap();
    prepared
        .convert(inventories, Target::Normalize, &tools)
        .unwrap();
    let result = psv::build(
        &normalized,
        None,
        &psv::Options {
            at9: None,
            texture_globs: Vec::new(),
            texture_auto: false,
            texture_quality: Default::default(),
            texture_storage: Default::default(),
            canvas: size,
            target: Size {
                width: 32,
                height: 16,
            },
        },
        &tools,
        |_| {},
    )
    .unwrap();
    assert_eq!(
        fs::read(result.output.join("sprite_p.bmp.png")).unwrap(),
        province
    );
    assert!(!result.output.join("sprite_p.bmp.krkr-scale").exists());
    let mut vfs = Vfs::new(&result.output, Default::default()).unwrap();
    let mut request = krkr_image::resolve::request(
        &mut vfs,
        &units("sprite.bmp"),
        0x02ffffff,
        None,
        Budget::new(32 * 1024 * 1024),
    )
    .unwrap();
    assert!(
        String::from_utf16_lossy(&request.mask.as_ref().unwrap().name).ends_with("sprite_m.bmp")
    );
    assert_eq!(
        request.province.as_ref().unwrap().read(0).unwrap(),
        province
    );
    // The fixture uses RGBA as province data; verify the mask decode separately.
    request.province = None;
    let prepared = request.probe(&AtomicBool::new(false)).unwrap();
    assert_eq!(prepared.size, size);
    assert_eq!(
        prepared.upload_size(),
        Size {
            width: 32,
            height: 16
        }
    );
    let decoded = prepared.decode(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [255, 0, 0, 100].repeat(64 * 32)
    );
}

#[test]
fn selected_bc1_assets_keep_logical_names_scale_and_xp3_loading() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("game");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 64,
        height: 32,
    };
    image(
        &source.join("background.png"),
        size,
        &[40, 90, 150, 255].repeat(64 * 32),
    );
    image(
        &source.join("portrait.png"),
        size,
        &[70, 50, 100, 127].repeat(64 * 32),
    );
    let portrait = fs::read(source.join("portrait.png")).unwrap();
    let output = temp.path().join("converted");
    let mut options = psv::Options::vita(size);
    options.target = Size {
        width: 32,
        height: 16,
    };
    options.texture_globs = vec!["background.png".into()];
    psv::build(
        &source,
        Some(&output),
        &options,
        &Default::default(),
        |_| {},
    )
    .unwrap();
    assert!(output.join("background.ktx").exists());
    assert!(!output.join("portrait.ktx").exists());
    let report = media::inspect(&output, &Default::default()).unwrap();
    let entry = report
        .entries
        .iter()
        .find(|e| e.path == "background.ktx")
        .unwrap();
    assert_eq!(entry.consistency, Consistency::Match);
    assert_eq!(entry.media.as_ref().unwrap().container, "ktx");
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("background.png"),
        0x02ffffff,
        None,
        Budget::new(4096),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(prepared.size, size);
    assert_eq!(
        prepared.upload_size(),
        Size {
            width: 32,
            height: 16
        }
    );
    assert!(prepared.is_compressed_upload());
    let texture = prepared.into_compressed().unwrap();
    assert_eq!(texture.data().len(), 32 * 16 / 2);
    let decoded =
        krkr_image::compressed::decode(&texture, &Budget::new(4096), &AtomicBool::new(false))
            .unwrap();
    for pixel in decoded.main.unwrap().as_slice().as_chunks::<4>().0.iter() {
        for (a, b) in pixel.iter().zip([40u8, 90, 150, 255]) {
            assert!(a.abs_diff(b) <= 8);
        }
    }
    cli(
        &["--no-progress", "xp3", "pack", output.to_str().unwrap()],
        temp.path(),
        true,
    );
    let mut vfs = Vfs::new(temp.path(), Default::default()).unwrap();
    let request = krkr_image::resolve::request(
        &mut vfs,
        &units("converted.xp3>background.png"),
        0x02ffffff,
        None,
        Budget::new(4096),
    )
    .unwrap();
    assert!(
        request
            .probe(&AtomicBool::new(false))
            .unwrap()
            .is_compressed_upload()
    );
    assert_eq!(fs::read(source.join("portrait.png")).unwrap(), portrait);
}

#[test]
fn bc1_npot_assets_use_pot_storage_without_changing_logical_coordinates() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("game");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 70,
        height: 34,
    };
    image(
        &source.join("bg.png"),
        size,
        &[90, 40, 150, 255].repeat(70 * 34),
    );
    let output = temp.path().join("converted");
    let report = psv::build(
        &source,
        Some(&output),
        &psv::Options {
            canvas: size,
            target: Size {
                width: 35,
                height: 17,
            },
            at9: None,
            texture_globs: vec!["bg.png".into()],
            texture_auto: false,
            texture_quality: Default::default(),
            texture_storage: Default::default(),
        },
        &Default::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.entries[0].stored_size, Some([64, 32]));
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("bg.png"),
        0x1fffffff,
        None,
        Budget::new(4096),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(prepared.size, size);
    assert_eq!(
        prepared.upload_size(),
        Size {
            width: 64,
            height: 32
        }
    );
    assert_eq!(prepared.into_compressed().unwrap().data().len(), 1024);
}

#[test]
fn texture_selection_rejects_masks_and_typos_without_publishing_output() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("game");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 8,
        height: 8,
    };
    image(
        &source.join("sprite.png"),
        size,
        &[70, 50, 100, 127].repeat(64),
    );
    image(
        &source.join("sprite_m.png"),
        size,
        &[70, 70, 70, 255].repeat(64),
    );
    image(
        &source.join("mask-target.png"),
        size,
        &[70, 70, 70, 255].repeat(64),
    );
    fs::write(
        source.join("old_m.bmp.krkr-link"),
        krkr_assets::converted::encode_link("mask-target.png").unwrap(),
    )
    .unwrap();
    image(
        &source.join("paired.png"),
        size,
        &[70, 70, 70, 255].repeat(64),
    );
    fs::write(
        source.join("old.bmp.krkr-link"),
        krkr_assets::converted::encode_link("paired.png").unwrap(),
    )
    .unwrap();
    image(
        &source.join("translucent.png"),
        size,
        &[70, 70, 70, 254].repeat(64),
    );
    for pattern in [
        "sprite.png",
        "sprite_m.png",
        "mask-target.png",
        "paired.png",
        "missing.png",
    ] {
        let output = temp.path().join("converted");
        assert!(
            psv::build(
                &source,
                Some(&output),
                &psv::Options {
                    canvas: size,
                    target: size,
                    at9: None,
                    texture_globs: vec![pattern.into()],
                    texture_auto: false,
                    texture_quality: Default::default(),
                    texture_storage: Default::default(),
                },
                &Default::default(),
                |_| {}
            )
            .is_err()
        );
        assert!(!output.exists());
    }
}

#[test]
fn normalization_rejects_dangling_links_without_publishing_output() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join("game")).unwrap();
    let bytes = krkr_assets::converted::encode_link("absent.png").unwrap();
    fs::write(temp.path().join("game/a.bmp.krkr-link"), &bytes).unwrap();
    cli(&["normalize", "game"], temp.path(), false);
    assert!(!temp.path().join("game-normalized").exists());
    assert_eq!(
        fs::read(temp.path().join("game/a.bmp.krkr-link")).unwrap(),
        bytes
    );
}

fn decoded_audio_samples(path: &Path) -> usize {
    let output = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-xerror", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout.len() / 2 // Fixtures are mono; count decoded samples, not timestamps.
}

#[test]
fn psv_checks_matched_m4a_and_static_gif_and_flattens_normalization_links() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("game");
    fs::create_dir(&source).unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=44100:duration=0.5",
            "-c:a",
            "aac",
            "-f",
            "mp4",
        ],
        &source.join("speech.wav"),
    );
    let aac = fs::read(source.join("speech.wav")).unwrap();
    fs::write(source.join("music.m4a"), &aac).unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=0.5",
            "-c:a",
            "libvorbis",
        ],
        &source.join("music.ogg"),
    );
    let vorbis = fs::read(source.join("music.ogg")).unwrap();
    fs::write(
        source.join("speech.wav.sli"),
        "#2.00\nLink {From=20000;To=1000;Smooth=False;}\n",
    )
    .unwrap();
    let script = "var music='music.m4a'; var sound='speech.wav'; var picture='still.gif';";
    fs::write(source.join("startup.tjs"), script).unwrap();
    image(
        &root.join("input.png"),
        Size {
            width: 16,
            height: 8,
        },
        &[80, 40, 20, 255].repeat(128),
    );
    ffmpeg(
        &[
            "-i",
            root.join("input.png").to_str().unwrap(),
            "-frames:v",
            "1",
            "-f",
            "gif",
        ],
        &source.join("still.gif"),
    );
    ffmpeg(
        &[
            "-i",
            root.join("input.png").to_str().unwrap(),
            "-frames:v",
            "1",
            "-c:v",
            "libwebp",
            "-f",
            "webp",
        ],
        &source.join("keep.webp"),
    );
    let webp = fs::read(source.join("keep.webp")).unwrap();
    let samples = decoded_audio_samples(&source.join("speech.wav"));
    let tools = Default::default();
    let normalized = root.join("normalized");
    let prepared = Prepared::new(&source, &normalized, None).unwrap();
    let inventory = prepared.inspect(&tools).unwrap();
    prepared
        .convert(inventory, Target::Normalize, &tools)
        .unwrap();
    assert_eq!(fs::read(normalized.join("speech.m4a")).unwrap(), aac);
    assert_eq!(fs::read(normalized.join("music.m4a")).unwrap(), aac);
    let inventory = media::inspect(&normalized, &tools).unwrap();
    assert!(
        inventory
            .entries
            .iter()
            .all(|e| e.consistency != Consistency::Mismatch)
    );
    let report = psv::build(
        &normalized,
        None,
        &psv::Options {
            at9: None,
            texture_globs: Vec::new(),
            texture_auto: false,
            texture_quality: Default::default(),
            texture_storage: Default::default(),
            canvas: Size {
                width: 16,
                height: 8,
            },
            target: Size {
                width: 16,
                height: 8,
            },
        },
        &tools,
        |_| {},
    )
    .unwrap();
    assert_eq!(
        report
            .entries
            .iter()
            .filter(|e| e.action.starts_with("audio_"))
            .count(),
        2
    );
    assert!(
        report
            .entries
            .iter()
            .any(|e| e.source == "still.gif" && e.action == "image_converted")
    );
    let output = &report.output;
    assert_eq!(
        fs::read(output.join("startup.tjs")).unwrap(),
        script.as_bytes()
    );
    assert_eq!(
        fs::read(output.join("speech.wav.sli")).unwrap(),
        fs::read(source.join("speech.wav.sli")).unwrap()
    );
    assert_eq!(fs::read(output.join("keep.webp")).unwrap(), webp);
    assert_eq!(fs::read(output.join("music.ogg")).unwrap(), vorbis);
    for logical in ["speech.wav", "speech.m4a", "music.m4a"] {
        let link = report.links.iter().find(|l| l.source == logical).unwrap();
        assert!(
            output.join(&link.target).is_file(),
            "links must point straight to the final media"
        );
        assert_eq!(decoded_audio_samples(&output.join(&link.target)), samples);
        let bytes = fs::read(output.join(format!("{logical}.krkr-link"))).unwrap();
        assert_eq!(
            krkr_assets::converted::decode_link(&bytes).unwrap(),
            link.target
        );
    }
    offline_pack(output, &root.join("psv.xp3"));
    let mut vfs = Vfs::new(root, Default::default()).unwrap();
    vfs.add_path(&units("psv.xp3>")).unwrap();
    let format = krkr_audio::inspect_builtin(vfs.plan(&units("speech.wav")).unwrap()).unwrap();
    assert_eq!(format.rate, 44100);
    assert_eq!(format.channels, 1);
    assert!(
        vfs.plan(&units("still.gif"))
            .unwrap()
            .read(0)
            .unwrap()
            .starts_with(b"\x89PNG")
    );
}

#[test]
fn helper_normalizes_tlg_tags_without_a_redundant_pass_for_psv() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let assets = temp.path().join("assets");
    fs::create_dir_all(&game).unwrap();
    fs::create_dir_all(&assets).unwrap();
    let path = assets.join("sprite.tlg");
    let budget = Budget::new(4 * 1024 * 1024);
    let mut main = krkr_protocol::pixels::Bytes::zeroed(32 * 16 * 4, &budget).unwrap();
    main.as_mut_slice()
        .copy_from_slice(&[10, 20, 30, 200].repeat(32 * 16));
    krkr_image::save::Request {
        target: krkr_assets::WritePlan::local(&path, 1024 * 1024).unwrap(),
        format: krkr_image::save::Format::Tlg {
            six: false,
            alpha: true,
        },
        pixels: Pixels {
            size: Size {
                width: 32,
                height: 16,
            },
            main: Some(main),
            province: None,
        },
        tags: Vec::new(),
        budget,
    }
    .write(&AtomicBool::new(false))
    .unwrap();
    let raw = fs::read(&path).unwrap();
    assert!(raw.starts_with(b"TLG5.0\0raw\x1a"));
    let payload = b"5:names=6:\x8c\xf5\x8a\xe1\x8b\xbe,";
    let mut original = b"TLG0.0\0sds\x1a".to_vec();
    original.extend_from_slice(&(raw.len() as u32).to_le_bytes());
    original.extend_from_slice(&raw);
    original.extend_from_slice(b"tags");
    original.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    original.extend_from_slice(payload);
    fs::write(&path, &original).unwrap();
    offline_pack(&assets, &game.join("data.xp3"));
    let archive = fs::read(game.join("data.xp3")).unwrap();
    let tools = Default::default();
    for (name, target) in [
        ("normalized", Target::Normalize),
        (
            "psv",
            Target::psv(Size {
                width: 1280,
                height: 720,
            }),
        ),
        (
            "psv-copy",
            Target::psv(Size {
                width: 960,
                height: 544,
            }),
        ),
    ] {
        let output = temp.path().join(name);
        let prepared = Prepared::new(&game, &output, None).unwrap();
        let inventory = prepared.inspect(&tools).unwrap();
        assert_eq!(inventory[1].entries[0].consistency, Consistency::Match);
        let report = prepared.convert(inventory, target.clone(), &tools).unwrap();
        assert_eq!(
            report.parts[1].adjusted,
            usize::from(matches!(target, Target::Normalize))
        );
        let mut vfs = Vfs::new(&output, Default::default()).unwrap();
        let stored = vfs
            .plan(&units("data.xp3>sprite.tlg"))
            .unwrap()
            .read(0)
            .unwrap();
        if name != "normalized" {
            assert_eq!(stored, original);
        } else {
            assert!(
                krkr_image::normalize_tlg_metadata(&stored)
                    .unwrap()
                    .is_none()
            );
        }
        if matches!(target, Target::Normalize) {
            assert_eq!(&stored[..15 + raw.len()], &original[..15 + raw.len()]);
        }
        let decoded = krkr_image::resolve::request(
            &mut vfs,
            &units("data.xp3>sprite.tlg"),
            0x02ffffff,
            None,
            Budget::new(4 * 1024 * 1024),
        )
        .unwrap()
        .probe(&AtomicBool::new(false))
        .unwrap()
        .decode(&AtomicBool::new(false))
        .unwrap();
        assert!(decoded.tags.contains(&("names".into(), "光眼鏡".into())));
        assert_eq!(fs::read(game.join("data.xp3")).unwrap(), archive);
    }
}

#[test]
fn helper_repairs_truncated_bmp_pixels_and_records_the_repair() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    let assets = temp.path().join("assets");
    fs::create_dir_all(&game).unwrap();
    fs::create_dir_all(&assets).unwrap();
    let mut bitmap = vec![0; 54];
    bitmap[..2].copy_from_slice(b"BM");
    for (at, value) in [(2, 118u32), (10, 54), (14, 40), (18, 4), (22, 4), (34, 64)] {
        bitmap[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    bitmap[26..28].copy_from_slice(&1u16.to_le_bytes());
    bitmap[28..30].copy_from_slice(&32u16.to_le_bytes());
    bitmap.extend_from_slice(&[30, 20, 10, 255].repeat(16));
    bitmap.truncate(bitmap.len() - 6);
    fs::write(assets.join("disc.bmp"), &bitmap).unwrap();
    offline_pack(&assets, &game.join("image.xp3"));
    let original = fs::read(game.join("image.xp3")).unwrap();
    let tools = Default::default();
    for (name, target, stored_width) in [
        ("normalized", Target::Normalize, 4),
        (
            "psv-copy",
            Target::psv(Size {
                width: 960,
                height: 544,
            }),
            4,
        ),
        (
            "psv-scale",
            Target::psv(Size {
                width: 1920,
                height: 1088,
            }),
            4,
        ),
    ] {
        let output = temp.path().join(name);
        let prepared = Prepared::new(&game, &output, None).unwrap();
        let inventory = prepared.inspect(&tools).unwrap();
        assert_eq!(inventory[1].entries[0].consistency, Consistency::Match);
        let report = prepared.convert(inventory, target.clone(), &tools).unwrap();
        let repairs = &report.parts[1].repairs;
        assert_eq!(repairs.len(), 1);
        assert_eq!(repairs[0].path, "disc.bmp");
        assert!(repairs[0].detail.contains("6 bytes"));
        assert!(repairs[0].detail.contains("2 incomplete or missing pixels"));
        let mut vfs = Vfs::new(&output, Default::default()).unwrap();
        let decoded = krkr_image::resolve::request(
            &mut vfs,
            &units("image.xp3>disc.bmp"),
            0x02ffffff,
            None,
            Budget::new(4 * 1024 * 1024),
        )
        .unwrap()
        .probe(&AtomicBool::new(false))
        .unwrap()
        .decode_compact(&AtomicBool::new(false))
        .unwrap();
        assert_eq!(decoded.pixels.size.width, stored_width);
        if stored_width == 4 {
            let mut expected = [10, 20, 30, 255].repeat(16);
            expected[8..16].fill(0); // Top-right two pixels of bottom-up BMP.
            assert_eq!(decoded.pixels.main.unwrap().as_slice(), expected);
        }
        assert_eq!(fs::read(game.join("image.xp3")).unwrap(), original);
    }
}

#[test]
fn helper_filter_is_applied_once_and_failures_publish_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let game = temp.path().join("game");
    fs::create_dir(&game).unwrap();
    let source = b"System.exit(0);";
    let cipher: Vec<_> = source.iter().map(|b| b ^ 0xa5).collect();
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::Zlib,
        Default::default(),
    )
    .unwrap();
    writer
        .add(&units("startup.tjs"), &mut cipher.as_slice())
        .unwrap();
    let original = writer.finish().unwrap().into_inner();
    fs::write(game.join("data.xp3"), &original).unwrap();
    let script = game.join("xp3filter.tjs");
    fs::write(&script, "Storages.setXP3ArchiveExtractionFilter(function(hash,offset,buf,size){buf.xor(0,size,0xa5);});").unwrap();
    let filter = archive::FilterOptions {
        script: Some(script.clone()),
        root: Some(game.clone()),
        encoding: "utf-8".into(),
    };
    let output = temp.path().join("normalized");
    let prepared = Prepared::new(&game, &output, Some(&filter)).unwrap();
    let inventories = prepared.inspect(&Default::default()).unwrap();
    let report = prepared
        .convert(inventories, Target::Normalize, &Default::default())
        .unwrap();
    assert!(report.filter_removed);
    assert!(!output.join("xp3filter.tjs").exists());
    assert!(script.is_file());
    assert_eq!(fs::read(game.join("data.xp3")).unwrap(), original);
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    assert_eq!(
        vfs.plan(&units("data.xp3>startup.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        source
    );
    fs::write(game.join("broken.png"), b"not an image").unwrap();
    let failed = temp.path().join("failed");
    let prepared = Prepared::new(&game, &failed, Some(&filter)).unwrap();
    let inventories = prepared.inspect(&Default::default()).unwrap();
    assert!(
        prepared
            .convert(inventories, Target::Normalize, &Default::default())
            .is_err()
    );
    assert!(!failed.exists());
    assert_eq!(fs::read(game.join("data.xp3")).unwrap(), original);
    assert!(fs::read_dir(temp.path()).unwrap().all(|p| {
        !p.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".krkr-helper-")
    }));
}

#[test]
fn helper_requires_a_terminal_and_is_listed_in_help() {
    let temp = tempfile::tempdir().unwrap();
    let help = cli(&["helper", "--help"], temp.path(), true);
    assert!(String::from_utf8_lossy(&help.stdout).contains("[SOURCE]"));
    let result = cli(&["helper"], temp.path(), false);
    assert!(String::from_utf8_lossy(&result.stderr).contains("交互终端"));
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn amv_is_kept_native_and_animated_images_are_not_flattened() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("game");
    fs::create_dir(&source).unwrap();
    // One 16x16 neutral YUVA frame, with DC zero and EOB for every block.
    let mut amv = b"AJPM".to_vec();
    for n in [260u32, 1, 232, 0, 1, 0, 24] {
        amv.extend(n.to_le_bytes());
    }
    for n in [16u16, 16] {
        amv.extend(n.to_le_bytes());
    }
    amv.extend(1u32.to_le_bytes());
    amv.extend([8; 192]);
    amv.extend(b"FRAM");
    for n in [20u32, 0] {
        amv.extend(n.to_le_bytes());
    }
    for n in [16u16, 16, 16, 16] {
        amv.extend(n.to_le_bytes());
    }
    amv.extend([0x00, 0x28, 0xa2, 0x8a, 0x28, 0xa2, 0x8a, 0x00]);
    fs::write(source.join("alpha.amv"), &amv).unwrap();
    let report = psv::build(
        &source,
        None,
        &psv::Options {
            at9: None,
            texture_globs: Vec::new(),
            texture_auto: false,
            texture_quality: Default::default(),
            texture_storage: Default::default(),
            canvas: Size {
                width: 16,
                height: 16,
            },
            target: Size {
                width: 8,
                height: 8,
            },
        },
        &Default::default(),
        |_| {},
    )
    .unwrap();
    assert_eq!(report.entries[0].action, "amv_native");
    assert_eq!(fs::read(report.output.join("alpha.amv")).unwrap(), amv);
    let mut vfs = Vfs::new(&report.output, Default::default()).unwrap();
    let movie = krkr_image::amv::Movie::open(
        std::sync::Arc::new(vfs.plan(&units("alpha.amv")).unwrap()),
        &|| false,
    )
    .unwrap()
    .unwrap();
    let (_, pixels) = movie
        .decode(0, &Budget::new(1024 * 1024), &|| false)
        .unwrap();
    assert_eq!(pixels.as_slice(), [128, 128, 128, 128].repeat(16 * 16));

    let animated = temp.path().join("animated");
    fs::create_dir(&animated).unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=16x16:r=2:d=1",
            "-plays",
            "0",
            "-f",
            "apng",
        ],
        &animated.join("motion.bmp"),
    );
    let before = fs::read(animated.join("motion.bmp")).unwrap();
    let report = media::inspect(&animated, &Default::default()).unwrap();
    let result = krkr_convert::adjust::apply(&report, &Default::default(), |_| {}).unwrap();
    assert!(result.failed());
    assert_eq!(fs::read(animated.join("motion.bmp")).unwrap(), before);
    fs::rename(animated.join("motion.bmp"), animated.join("motion.png")).unwrap();
    assert!(
        psv::build(
            &animated,
            None,
            &psv::Options {
                at9: None,
                texture_globs: Vec::new(),
                texture_auto: false,
                texture_quality: Default::default(),
                texture_storage: Default::default(),
                canvas: Size {
                    width: 16,
                    height: 16
                },
                target: Size {
                    width: 8,
                    height: 8
                },
            },
            &Default::default(),
            |_| {}
        )
        .is_err()
    );
    assert!(!temp.path().join("animated-psv").exists());
}

#[test]
fn batch_xp3_names_patterns_duplicates_and_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir_all(root.join("parts/data.v1/sub")).unwrap();
    fs::create_dir_all(root.join("parts/voice")).unwrap();
    fs::write(root.join("parts/data.v1/sub/scene.ks"), b"hello").unwrap();
    fs::write(root.join("parts/voice/line.ogg"), b"voice").unwrap();
    cli(&["xp3", "pack", "parts", "--each"], root, true);
    assert!(root.join("parts/data.v1.xp3").is_file());
    let expanded =
        archive::expand(&[root.join("parts/*.xp3"), root.join("parts/voice.xp3")]).unwrap();
    assert_eq!(expanded.len(), 2);
    // Every output is preflighted before any extraction is published.
    cli(&["xp3", "unpack", "parts/*.xp3"], root, false);
    fs::rename(root.join("parts/data.v1"), root.join("original-data")).unwrap();
    fs::rename(root.join("parts/voice"), root.join("original-voice")).unwrap();
    cli(
        &["xp3", "unpack", "parts/*.xp3", "parts/voice.xp3"],
        root,
        true,
    );
    assert_eq!(
        fs::read(root.join("parts/data.v1/sub/scene.ks")).unwrap(),
        b"hello"
    );
    assert_eq!(
        fs::read(root.join("parts/voice/line.ogg")).unwrap(),
        b"voice"
    );
    cli(&["xp3", "unpack", "missing*.xp3"], root, false);
    cli(&["xp3", "pack", "parts/voice"], root, false);
}

#[test]
fn in_place_script_decryption_and_failure_keep_originals() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let original = b"payload from encrypted game";
    for compression in [Compression::None, Compression::Zlib] {
        let mut writer =
            Writer::new(Cursor::new(Vec::new()), compression, Default::default()).unwrap();
        let cipher: Vec<_> = original.iter().map(|b| b ^ 0xa5).collect();
        writer
            .add(&units("startup.tjs"), &mut cipher.as_slice())
            .unwrap();
        let bytes = writer.finish().unwrap().into_inner();
        fs::write(root.join("encrypted.xp3"), &bytes).unwrap();
        fs::write(root.join("xp3filter.tjs"),"Storages.setXP3ArchiveExtractionFilter(function(hash,offset,buf,size){buf.xor(0,size,0xa5);});").unwrap();
        cli(&["xp3", "decrypt", "encrypted.xp3"], root, true);
        let mut vfs = Vfs::new(root, Default::default()).unwrap();
        assert_eq!(
            vfs.plan(&units("encrypted.xp3>startup.tjs"))
                .unwrap()
                .read(0)
                .unwrap(),
            original
        );
        fs::write(root.join("encrypted.xp3"), &bytes).unwrap();
        fs::write(
            root.join("bad.tjs"),
            "Storages.setXP3ArchiveExtractionFilter(function(){throw 'bad key';});",
        )
        .unwrap();
        cli(
            &["xp3", "decrypt", "*.xp3", "--xp3-filter", "bad.tjs"],
            root,
            false,
        );
        assert_eq!(fs::read(root.join("encrypted.xp3")).unwrap(), bytes);
    }
}

#[test]
fn parallel_unpack_filter_and_batch_failure_preserve_sources() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let plaintext = b"payload from encrypted game";
    let cipher: Vec<_> = plaintext.iter().map(|b| b ^ 0xa5).collect();
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::Zlib,
        Default::default(),
    )
    .unwrap();
    for n in 0..24 {
        writer
            .add(&units(&format!("dir/{n:02}.bin")), &mut cipher.as_slice())
            .unwrap();
    }
    let bytes = writer.finish().unwrap().into_inner();
    fs::write(root.join("encrypted.xp3"), &bytes).unwrap();
    fs::write(root.join("invalid.xp3"), b"broken index").unwrap();
    fs::write(root.join("filter.tjs"), "Storages.setXP3ArchiveExtractionFilter(function(hash,offset,buf,size){buf.xor(0,size,0xa5);});").unwrap();
    // Successful archives are published even when another parallel job fails.
    let output = cli(
        &[
            "xp3",
            "unpack",
            "*.xp3",
            "--xp3-filter",
            "filter.tjs",
            "--filter-root",
            ".",
            "--jobs",
            "4",
            "--no-progress",
        ],
        root,
        false,
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid.xp3"));
    assert!(!output.stderr.contains(&0x1b));
    assert!(output.stdout.is_empty());
    for n in 0..24 {
        assert_eq!(
            fs::read(root.join(format!("encrypted/dir/{n:02}.bin"))).unwrap(),
            plaintext
        );
    }
    assert_eq!(fs::read(root.join("encrypted.xp3")).unwrap(), bytes);
    assert!(!root.join("invalid").exists());
    fs::write(root.join("failure.xp3"), &bytes).unwrap();
    fs::write(
        root.join("filter.tjs"),
        "Storages.setXP3ArchiveExtractionFilter(function(){throw 'bad key';});",
    )
    .unwrap();
    cli(
        &[
            "xp3",
            "unpack",
            "failure.xp3",
            "--xp3-filter",
            "filter.tjs",
            "--jobs",
            "4",
        ],
        root,
        false,
    );
    assert!(!root.join("failure").exists());
    assert_eq!(fs::read(root.join("failure.xp3")).unwrap(), bytes);
    assert!(fs::read_dir(root).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".krkr-")
    }));
    cli(
        &["--jobs", "0", "xp3", "unpack", "failure.xp3"],
        root,
        false,
    );
    cli(
        &["--jobs", "65", "xp3", "unpack", "failure.xp3"],
        root,
        false,
    );
}

#[test]
fn parallel_probe_keeps_json_order_and_progress_out_of_stdout() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("game")).unwrap();
    for n in 0..8 {
        fs::write(root.join(format!("game/{n:02}.tjs")), b"var x = 1;").unwrap();
    }
    let serial = cli(&["probe", "game", "--jobs", "1"], root, true);
    let parallel = cli(
        &["probe", "game", "--jobs", "4", "--no-progress"],
        root,
        true,
    );
    assert_eq!(serial.stdout, parallel.stdout);
    assert!(serial.stderr.is_empty());
    assert!(parallel.stderr.is_empty());
    let report: media::Report = serde_json::from_slice(&parallel.stdout).unwrap();
    assert_eq!(report.entries.len(), 8);
    assert!(
        report
            .entries
            .windows(2)
            .all(|pair| pair[0].path < pair[1].path)
    );
}

#[test]
fn adjust_accepts_powershell_utf16_reports_and_rejects_invalid_unicode() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let assets = root.join("assets");
    fs::create_dir(&assets).unwrap();
    let source = assets.join("背景😀.bmp");
    let size = Size {
        width: 16,
        height: 16,
    };
    let rgba = [10, 20, 30, 255].repeat(16 * 16);
    image(&source, size, &rgba);
    let report = media::inspect(&assets, &Default::default()).unwrap();
    let json = serde_json::to_string(&report).unwrap();
    let mut utf8 = vec![0xef, 0xbb, 0xbf];
    utf8.extend(json.as_bytes());
    let mut le = vec![0xff, 0xfe];
    le.extend(json.encode_utf16().flat_map(u16::to_le_bytes));
    let mut be = vec![0xfe, 0xff];
    be.extend(json.encode_utf16().flat_map(u16::to_be_bytes));
    for bytes in [utf8, le, be] {
        image(&source, size, &rgba);
        fs::write(root.join("report.json"), bytes).unwrap();
        let output = cli(&["adjust", "report.json", "--no-progress"], root, true);
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["entries"][0]["status"], "adjusted");
        assert!(fs::read(&source).unwrap().starts_with(b"BM"));
    }
    let unchanged = fs::read(&source).unwrap();
    for bytes in [vec![0xff, 0xfe, b'{'], vec![0xff, 0xfe, 0, 0xd8]] {
        fs::write(root.join("invalid.json"), bytes).unwrap();
        let output = cli(
            &["adjust", "invalid.json", "-o", "invalid-result.json"],
            root,
            false,
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid.json"));
        assert!(!root.join("invalid-result.json").exists());
        assert_eq!(fs::read(&source).unwrap(), unchanged);
    }
}

#[test]
fn helper_links_opus_in_ogg_and_preserves_encoded_bytes_loops_and_archive_paths() {
    let temp = tempfile::tempdir().unwrap();
    let assets = temp.path().join("assets");
    let game = temp.path().join("game");
    fs::create_dir_all(&assets).unwrap();
    fs::create_dir_all(game.join("arc")).unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=5.17",
            "-c:a",
            "libopus",
            "-f",
            "ogg",
        ],
        &assets.join("voice.ogg"),
    );
    let original = fs::read(assets.join("voice.ogg")).unwrap();
    let loops =
        "#2.00\nLink { From=20000; To=1000; Smooth=False; Condition=no; RefValue=0; CondVar=0; }\n";
    fs::write(assets.join("voice.ogg.sli"), loops).unwrap();
    // The explicit .opus suffix keeps its own contract; Ogg/Vorbis is already valid.
    fs::write(temp.path().join("explicit.opus"), &original).unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=0.5",
            "-c:a",
            "libvorbis",
            "-f",
            "ogg",
        ],
        &assets.join("correct.ogg"),
    );
    let correct = fs::read(assets.join("correct.ogg")).unwrap();
    let tools = Default::default();
    let probe = media::inspect(temp.path(), &tools).unwrap();
    for (path, expected) in [
        ("assets/voice.ogg", Consistency::Mismatch),
        ("assets/correct.ogg", Consistency::Match),
        ("explicit.opus", Consistency::Match),
    ] {
        assert_eq!(
            probe
                .entries
                .iter()
                .find(|e| e.path == path)
                .unwrap()
                .consistency,
            expected
        );
    }
    offline_pack(&assets, &game.join("arc/sound.xp3"));
    let archive = fs::read(game.join("arc/sound.xp3")).unwrap();
    let normalized = normalized_game(&game);
    for (name, target) in [
        ("normalized", Target::Normalize),
        (
            "psv",
            Target::psv(Size {
                width: 960,
                height: 544,
            }),
        ),
    ] {
        let output = temp.path().join(name);
        let prepared = Prepared::new(
            if matches!(target, Target::Normalize) {
                &game
            } else {
                &normalized
            },
            &output,
            None,
        )
        .unwrap();
        let inventory = prepared.inspect(&tools).unwrap();
        let report = prepared.convert(inventory, target.clone(), &tools).unwrap();
        assert_eq!(
            report.parts.iter().map(|p| p.adjusted).sum::<usize>(),
            usize::from(matches!(target, Target::Normalize))
        );
        let mut vfs = Vfs::new(&output, Default::default()).unwrap();
        let plan = vfs.plan(&units("arc/sound.xp3>voice.ogg")).unwrap();
        assert!(String::from_utf16_lossy(&plan.name).ends_with(">voice.ogg"));
        if matches!(target, Target::Normalize) {
            assert_eq!(plan.read(0).unwrap(), original);
            assert_eq!(
                vfs.plan(&units("arc/sound.xp3>voice.opus"))
                    .unwrap()
                    .read(0)
                    .unwrap(),
                original
            );
        } else {
            let format = krkr_audio::inspect_builtin(plan).unwrap();
            assert_eq!(format.rate, 48000);
            assert_eq!(format.channels, 1);
            assert_eq!(
                report
                    .parts
                    .iter()
                    .map(|p| p.converted_audio)
                    .sum::<usize>(),
                1
            );
        }
        assert_eq!(
            vfs.plan(&units("arc/sound.xp3>voice.ogg.sli"))
                .unwrap()
                .read(0)
                .unwrap(),
            loops.as_bytes()
        );
        assert_eq!(
            vfs.plan(&units("arc/sound.xp3>correct.ogg"))
                .unwrap()
                .read(0)
                .unwrap(),
            correct
        );
        assert_eq!(fs::read(game.join("arc/sound.xp3")).unwrap(), archive);
    }
}

#[test]
fn audio_adjustment_preserves_samples_when_timestamps_round_down() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("loop.ogg");
    // At 44.1 kHz, the final PCM input frame starts at sample 61440.
    // N/SR/TB evaluates just below 61440 and used to trim one output sample.
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=44100:duration=2",
            "-af",
            "atrim=end_sample=62017",
            "-ac",
            "2",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ],
        &source,
    );
    let sidecar = temp.path().join("loop.ogg.sli");
    let loops =
        "#2.00\nLink { From=62017; To=1000; Smooth=False; Condition=no; RefValue=0; CondVar=0; }\n";
    fs::write(&sidecar, loops).unwrap();
    let tools = Default::default();
    let probe = media::inspect(temp.path(), &tools).unwrap();
    let result = krkr_convert::adjust::apply(&probe, &tools, |_| {}).unwrap();
    let entry = &result.entries[0];
    assert_eq!(entry.status, "adjusted", "{:?}", entry.detail);
    assert_eq!(fs::read(&sidecar).unwrap(), loops.as_bytes());
    assert_eq!(
        media::inspect(&source, &tools).unwrap().entries[0]
            .media
            .as_ref()
            .unwrap()
            .tracks[0]
            .codec,
        "vorbis"
    );
    // Count actual PCM independently of the converter's ffprobe frame sum.
    let decoded = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-xerror", "-i"])
        .arg(&source)
        .args([
            "-map",
            "0:a:0",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .output()
        .unwrap();
    assert!(decoded.status.success());
    assert_eq!(decoded.stdout.len(), 62017 * 2 * 2);
}

#[test]
fn audio_adjustment_refuses_changed_loop_sample_counts() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("short.ogg");
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=sample_rate=48000:duration=0.5",
            "-c:a",
            "libopus",
            "-f",
            "ogg",
        ],
        &source,
    );
    let original = fs::read(&source).unwrap();
    fs::write(temp.path().join("short.ogg.sli"), "#2.00\n").unwrap();
    let tools = Default::default();
    let probe = media::inspect(temp.path(), &tools).unwrap();
    let reference = temp.path().join("reference.ogg");
    ffmpeg(
        &[
            "-i",
            source.to_str().unwrap(),
            "-af",
            "asettb=1/sr,asetpts=N",
            "-c:a",
            "libvorbis",
            "-q:a",
            "5",
        ],
        &reference,
    );
    let pcm_bytes = |path: &Path| {
        let output = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-xerror", "-i"])
            .arg(path)
            .args([
                "-map",
                "0:a:0",
                "-c:a",
                "pcm_s16le",
                "-f",
                "s16le",
                "pipe:1",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        output.stdout.len()
    };
    let original_samples = pcm_bytes(&source);
    let reference_samples = pcm_bytes(&reference);
    let result = krkr_convert::adjust::apply(&probe, &tools, |_| {}).unwrap();
    // Some Vorbis packet layouts trim the first block in FFmpeg. Preserve the
    // original whenever a codec/FFmpeg version cannot keep the decoded length.
    let entry = &result.entries[0];
    if reference_samples != original_samples {
        assert_eq!(entry.status, "error");
        assert!(
            entry
                .detail
                .as_ref()
                .unwrap()
                .contains("decoded sample counts")
        );
        assert!(entry.detail.as_ref().unwrap().contains(&format!(
            "[{}] -> [{}]",
            original_samples / 2,
            reference_samples / 2
        )));
        assert_eq!(fs::read(&source).unwrap(), original);
    } else {
        assert_eq!(entry.status, "adjusted");
        assert_eq!(pcm_bytes(&source), original_samples);
        let plan = Vfs::new(temp.path(), Default::default())
            .unwrap()
            .plan(&units("short.ogg"))
            .unwrap();
        assert_eq!(krkr_audio::inspect_builtin(plan).unwrap().rate, 48000);
    }
}

#[test]
fn media_json_adjusts_image_audio_video_without_renaming_and_rejects_stale_paths() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let rgba = [12, 34, 56, 78].repeat(16 * 16);
    image(
        &root.join("sprite.tlg6"),
        Size {
            width: 16,
            height: 16,
        },
        &rgba,
    );
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100:duration=0.2",
            "-c:a",
            "pcm_s16le",
            "-f",
            "wav",
        ],
        &root.join("voice.ogg"),
    );
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=red:s=32x16:r=25:d=0.2",
            "-c:v",
            "mpeg4",
            "-pix_fmt",
            "yuv420p",
            "-f",
            "mp4",
        ],
        &root.join("opening.wmv"),
    );
    fs::write(root.join("startup.tjs"), "var x=1;").unwrap();
    let report = media::inspect(root, &Default::default()).unwrap();
    assert_eq!(
        report
            .entries
            .iter()
            .filter(|e| e.consistency == Consistency::Mismatch)
            .count(),
        3
    );
    assert_eq!(
        report
            .entries
            .iter()
            .find(|e| e.path == "startup.tjs")
            .unwrap()
            .consistency,
        Consistency::Unknown
    );
    let json = serde_json::to_vec(&report).unwrap();
    fs::write(root.join("probe.json"), json).unwrap();
    let output = cli(&["adjust", "probe.json"], root, true);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["entries"].as_array().unwrap().len(), 3);
    assert!(
        result["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["status"] == "adjusted")
    );
    let after = media::inspect(root, &Default::default()).unwrap();
    assert!(
        !after
            .entries
            .iter()
            .any(|e| e.consistency == Consistency::Mismatch)
    );
    let mut vfs = Vfs::new(root, Default::default()).unwrap();
    let decoded = krkr_image::resolve::request(
        &mut vfs,
        &units("sprite.tlg6"),
        0x02ffffff,
        None,
        Budget::new(32 * 1024 * 1024),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap()
    .decode(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(decoded.pixels.main.unwrap().as_slice(), rgba);
    cli(&["adjust", "probe.json"], root, true); // idempotent replay
    let stale = root.join("stale");
    fs::create_dir(&stale).unwrap();
    image(
        &stale.join("a.bmp"),
        Size {
            width: 16,
            height: 16,
        },
        &rgba,
    );
    let mut report = media::inspect(&stale, &Default::default()).unwrap();
    image(
        &stale.join("a.bmp"),
        Size {
            width: 16,
            height: 16,
        },
        &[8, 7, 6, 255].repeat(16 * 16),
    );
    let bytes = fs::read(stale.join("a.bmp")).unwrap();
    let result = krkr_convert::adjust::apply(&report, &Default::default(), |_| {}).unwrap();
    assert!(result.failed());
    assert_eq!(fs::read(stale.join("a.bmp")).unwrap(), bytes);
    report.entries[0].path = "../opening.wmv".into();
    assert!(krkr_convert::adjust::apply(&report, &Default::default(), |_| {}).is_err());
}

#[test]
fn psv_scale_keeps_logical_images_and_legacy_video_names_in_xp3() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("game");
    fs::create_dir(&source).unwrap();
    let rgba = [255, 0, 0, 255].repeat(64 * 32);
    image(
        &source.join("sprite.png"),
        Size {
            width: 64,
            height: 32,
        },
        &rgba,
    );
    image(
        &source.join("sprite_m.png"),
        Size {
            width: 64,
            height: 32,
        },
        &[100, 100, 100, 255].repeat(64 * 32),
    );
    fs::write(source.join("startup.tjs"), "var unchanged=1920;").unwrap();
    ffmpeg(
        &[
            "-f",
            "lavfi",
            "-i",
            "color=blue:s=64x32:r=25:d=0.2",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:duration=0.2",
            "-c:v",
            "mpeg4",
            "-c:a",
            "pcm_s16le",
            "-shortest",
            "-f",
            "avi",
        ],
        &source.join("opening.avi"),
    );
    // Normalize misleading suffixes separately. PSV must reuse those links and
    // encode each video only once.
    let helper_source = root.join("helper-video");
    fs::create_dir(&helper_source).unwrap();
    fs::copy(
        source.join("opening.avi"),
        helper_source.join("opening.wmv"),
    )
    .unwrap();
    fs::copy(
        source.join("opening.avi"),
        helper_source.join("soundtrack.wav"),
    )
    .unwrap();
    let normalized = normalized_game(&helper_source);
    let prepared = Prepared::new(&normalized, &root.join("helper-video-psv"), None).unwrap();
    let inventories = prepared.inspect(&Default::default()).unwrap();
    assert!(
        inventories[0]
            .entries
            .iter()
            .all(|e| e.consistency != Consistency::Mismatch)
    );
    let result = prepared
        .convert(
            inventories,
            Target::psv(Size {
                width: 1280,
                height: 720,
            }),
            &Default::default(),
        )
        .unwrap();
    assert_eq!(result.parts[0].adjusted, 0);
    assert_eq!(result.parts[0].converted_videos, 2);
    assert_eq!(result.parts[0].converted_audio, 0);
    let tracks = media::inspect(
        &result.output.join("soundtrack.avi.mp4"),
        &Default::default(),
    )
    .unwrap();
    let tracks = &tracks.entries[0].media.as_ref().unwrap().tracks;
    assert!(tracks.iter().any(|t| t.kind == "video"));
    assert!(tracks.iter().any(|t| t.kind == "audio"));
    assert!(result.output.join("opening.avi.mp4").is_file());
    let mut linked_vfs = Vfs::new(&result.output, Default::default()).unwrap();
    assert!(
        linked_vfs
            .plan(&units("opening.wmv"))
            .unwrap()
            .read(0)
            .unwrap()
            .get(4..8)
            == Some(b"ftyp")
    );
    assert!(!result.output.join("opening.wmv").exists());
    let report = psv::build(
        &source,
        None,
        &psv::Options {
            at9: None,
            texture_globs: Vec::new(),
            texture_auto: false,
            texture_quality: Default::default(),
            texture_storage: Default::default(),
            canvas: Size {
                width: 64,
                height: 32,
            },
            target: Size {
                width: 32,
                height: 16,
            },
        },
        &Default::default(),
        |_| {},
    )
    .unwrap();
    let output = report.output;
    assert!(!output.join("krkr-psv.json").exists());
    assert_eq!(
        fs::read(output.join("startup.tjs")).unwrap(),
        b"var unchanged=1920;"
    );
    assert!(!output.join("opening.avi").exists());
    assert!(output.join("opening.avi.mp4").exists());
    let color = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=color_space,color_range",
            "-of",
            "json",
        ])
        .arg(output.join("opening.avi.mp4"))
        .output()
        .unwrap();
    assert!(color.status.success());
    let color: serde_json::Value = serde_json::from_slice(&color.stdout).unwrap();
    assert_eq!(color["streams"][0]["color_space"], "smpte170m");
    assert_eq!(color["streams"][0]["color_range"], "tv");
    let probe = media::inspect(&output, &Default::default()).unwrap();
    let video = probe
        .entries
        .iter()
        .find(|e| e.path == "opening.avi.mp4")
        .unwrap()
        .media
        .as_ref()
        .unwrap();
    assert_eq!(video.container, "mp4");
    assert_eq!(video.width, Some(32));
    assert!(video.tracks.iter().any(|t| t.codec == "h264"));
    assert!(video.tracks.iter().any(|t| t.codec == "aac"));
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    let request = krkr_image::resolve::request(
        &mut vfs,
        &units("sprite.png"),
        0x02ffffff,
        None,
        Budget::new(32 * 1024 * 1024),
    )
    .unwrap();
    let prepared = request.probe(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        prepared.size,
        Size {
            width: 64,
            height: 32
        }
    );
    assert_eq!(
        prepared.upload_size(),
        Size {
            width: 32,
            height: 16
        }
    );
    let decoded = prepared.decode(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        decoded.pixels.size,
        Size {
            width: 64,
            height: 32
        }
    );
    assert_eq!(
        decoded.pixels.main.unwrap().as_slice(),
        [255, 0, 0, 100].repeat(64 * 32)
    );
    let alias = vfs.plan(&units("opening.avi")).unwrap();
    assert!(String::from_utf16_lossy(&alias.name).ends_with("opening.avi.mp4"));
    // Indexed auto paths and packed archives resolve exactly the same alias.
    let archive = root.join("game-psv.xp3");
    krkr_assets::xp3::offline::pack_directory(
        &output,
        &archive,
        Compression::Zlib,
        Default::default(),
    )
    .unwrap();
    let packed = krkr_assets::xp3::Archive::load_strict(&archive, Default::default()).unwrap();
    assert!(packed.entries.get(&units("krkr-psv.json")).is_none());
    let mut vfs = Vfs::new(root, Default::default()).unwrap();
    vfs.add_path(&units("game-psv.xp3>")).unwrap();
    let alias = vfs.plan(&units("opening.avi")).unwrap();
    assert_eq!(
        alias.read(0).unwrap(),
        fs::read(output.join("opening.avi.mp4")).unwrap()
    );
    let prepared = krkr_image::resolve::request(
        &mut vfs,
        &units("sprite.png"),
        0x02ffffff,
        None,
        Budget::new(32 * 1024 * 1024),
    )
    .unwrap()
    .probe(&AtomicBool::new(false))
    .unwrap();
    assert_eq!(
        prepared.size,
        Size {
            width: 64,
            height: 32
        }
    );
}

#[test]
fn helper_psv_rejects_unnormalized_input_before_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let output = temp.path().join("psv");
    fs::create_dir(&source).unwrap();
    let size = Size {
        width: 8,
        height: 8,
    };
    image(
        &source.join("wrong.bmp"),
        size,
        &[30, 60, 90, 255].repeat(64),
    );
    let original = fs::read(source.join("wrong.bmp")).unwrap();
    let prepared = Prepared::new(&source, &output, None).unwrap();
    let inventories = prepared.inspect(&Default::default()).unwrap();
    let error = prepared
        .convert(inventories, Target::psv(size), &Default::default())
        .err()
        .unwrap();
    assert!(error.contains("请先归一化"), "{error}");
    assert!(!output.exists());
    assert_eq!(fs::read(source.join("wrong.bmp")).unwrap(), original);
    let normalized = normalized_game(&source);
    let prepared = Prepared::new(&normalized, &output, None).unwrap();
    let inventories = prepared.inspect(&Default::default()).unwrap();
    let report = prepared
        .convert(inventories, Target::psv(size), &Default::default())
        .unwrap();
    assert!(report.parts.iter().all(|part| part.adjusted == 0));
    let mut vfs = Vfs::new(&output, Default::default()).unwrap();
    assert!(vfs.plan(&units("wrong.bmp")).is_ok());
}

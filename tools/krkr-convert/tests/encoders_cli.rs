use std::{fs, process::Command};

#[test]
fn psv_finds_encoders_beside_executable_instead_of_working_directory() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("程序 with spaces");
    let cwd = temp.path().join("game");
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&cwd).unwrap();
    let executable = bin.join(format!("krkr-convert{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_krkr-convert"), &executable).unwrap();
    let budget = krkr_protocol::budget::Budget::new(4 * 1024 * 1024);
    let mut pixels = krkr_protocol::pixels::Bytes::zeroed(128 * 128 * 4, &budget).unwrap();
    for pixel in pixels.as_mut_slice().as_chunks_mut::<4>().0.iter_mut() {
        pixel.copy_from_slice(&[52, 119, 207, 119]);
    }
    krkr_image::save::Request {
        target: krkr_assets::WritePlan::local(cwd.join("actor.png"), 4 * 1024 * 1024).unwrap(),
        format: krkr_image::save::Format::Png { alpha: true },
        pixels: krkr_protocol::pixels::Pixels {
            size: krkr_protocol::graphics::Size {
                width: 128,
                height: 128,
            },
            main: Some(pixels),
            province: None,
        },
        tags: Vec::new(),
        budget,
    }
    .write(&std::sync::atomic::AtomicBool::new(false))
    .unwrap();
    {
        let (flag, name, args) = ("--at9-glob", "at9tool", vec!["--at9-glob", "*"]);
        let filename = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        fs::write(cwd.join(&filename), b"not an encoder").unwrap();
        let result = Command::new(&executable)
            .current_dir(&cwd)
            .args(["psv", ".", "--canvas", "960x544"])
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success(), "{flag}");
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(
            error.contains(&bin.join(filename).display().to_string()),
            "{error}"
        );
        assert!(error.contains("缺少"), "{error}");
    }
    let help = Command::new(&executable)
        .args(["psv", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(!help.contains("--pvrtc-tool"));
    assert!(!help.contains("--at9-tool"));
}

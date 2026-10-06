use krkr_assets::{
    Vfs, converted,
    name::units,
    xp3::{Compression, Writer},
};
use std::{fs, io::Cursor};

fn link(target: &str) -> Vec<u8> {
    converted::encode_link(target).unwrap()
}

#[test]
fn links_preserve_logical_names_streams_and_auto_paths_in_loose_files_and_xp3() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("media")).unwrap();
    let entries = [
        ("景色.bmp.krkr-link", link("景色.png")),
        ("景色.png.krkr-link", link("景色.webp")),
        ("景色.webp", b"compressed image".to_vec()),
        ("景色.bmp.sli", b"unchanged metadata".to_vec()),
    ];
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::Zlib,
        Default::default(),
    )
    .unwrap();
    for (name, bytes) in &entries {
        fs::write(dir.path().join("media").join(name), bytes).unwrap();
        writer.add(&units(name), &mut bytes.as_slice()).unwrap();
    }
    fs::write(
        dir.path().join("data.xp3"),
        writer.finish().unwrap().into_inner(),
    )
    .unwrap();
    for prefix in ["media/", "data.xp3>"] {
        let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
        vfs.add_path(&units(prefix)).unwrap();
        let name = units(&format!("{prefix}景色.bmp"));
        let plan = vfs.plan(&units("景色.bmp")).unwrap();
        assert_eq!(plan.name, vfs.full_path(&name).unwrap());
        assert!(String::from_utf16_lossy(plan.physical_name()).ends_with("景色.webp"));
        assert_eq!(plan.read(0).unwrap(), b"compressed image");
        let physical = vfs.plan(&units("景色.webp")).unwrap();
        assert!(plan.same_file_version(&physical));
        assert!(physical.same_file_version(&plan));
        assert_eq!(plan.read(11).unwrap(), b"image");
        assert!(vfs.exists_no_search_no_normalize(&plan.name).unwrap());
        assert_eq!(
            vfs.plan(&units("景色.bmp.sli")).unwrap().read(0).unwrap(),
            b"unchanged metadata"
        );
        let full = vfs.full_path(&units(prefix)).unwrap();
        let visible = vfs
            .visible_names(&full, entries.iter().map(|(n, _)| units(n)).collect())
            .unwrap();
        assert!(visible.contains(&units("景色.bmp")));
        assert!(!visible.contains(&units("景色.webp")));
        assert!(
            !visible
                .iter()
                .any(|n| String::from_utf16_lossy(n).ends_with(converted::LINK_SUFFIX))
        );
    }
}

#[test]
fn decoded_file_reuse_requires_a_fresh_matching_version() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("font.tft");
    fs::write(&file, b"old").unwrap();
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    let old = vfs.plan(&units("font.tft")).unwrap();
    assert!(old.same_file_version(&vfs.plan(&units("font.tft")).unwrap()));
    fs::write(&file, b"replacement").unwrap();
    assert!(!old.same_file_version(&vfs.plan(&units("font.tft")).unwrap()));
}

#[test]
fn malformed_dangling_and_cyclic_links_fail_and_real_files_take_priority() {
    let dir = tempfile::tempdir().unwrap();
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    let marker = dir.path().join("item.bmp.krkr-link");
    for bytes in [
        b"garbage".to_vec(),
        b"KRKR-LINK-1\n../outside".to_vec(),
        b"KRKR-LINK-1\ndata.xp3>secret".to_vec(),
        link("missing.png"),
        link("item.bmp"),
        vec![0; converted::MAX_LINK_BYTES + 1],
    ] {
        fs::write(&marker, bytes).unwrap();
        assert!(vfs.plan(&units("item.bmp")).is_err());
    }
    fs::write(&marker, link("next.png")).unwrap();
    fs::write(dir.path().join("next.png.krkr-link"), link("item.bmp")).unwrap();
    assert!(
        vfs.plan(&units("item.bmp"))
            .err()
            .unwrap()
            .to_string()
            .contains("cyclic")
    );
    fs::write(dir.path().join("item.bmp"), b"override").unwrap();
    assert_eq!(
        vfs.plan(&units("item.bmp")).unwrap().read(0).unwrap(),
        b"override"
    );
}

#[test]
fn a_linked_read_plan_still_detects_changes_to_its_physical_file() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("item.bmp.krkr-link"), link("item.png")).unwrap();
    fs::write(dir.path().join("item.png"), b"first").unwrap();
    let mut vfs = Vfs::new(dir.path(), Default::default()).unwrap();
    let plan = vfs.plan(&units("item.bmp")).unwrap();
    fs::write(dir.path().join("item.png"), b"changed length").unwrap();
    assert!(matches!(plan.read(0), Err(krkr_assets::Error::Changed)));
}

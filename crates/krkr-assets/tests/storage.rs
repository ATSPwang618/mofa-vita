mod support;
use krkr_assets::{
    Error, Limits, Vfs,
    name::{self, units},
    text, xp3,
};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    sync::{Arc, Mutex},
};

#[test]
fn names_roots_archives_case_and_autopath_precedence() {
    let current = units("file://./c/game/");
    for (input, expected) in [
        ("..\\Data//STARTUP.TJS", "file://./c/data/startup.tjs"),
        (
            "D:\\Games\\Data.xp3>ScEnArIo//Start.KS",
            "file://./d/games/data.xp3>scenario/start.ks",
        ),
        ("FILE://Server/Share/a/../b", "file://server/share/b"),
        ("file:///c/game/./", "file://./c/game/"),
        ("a/b/.../c", "file://./c/game/c"),
        ("", ""),
    ] {
        assert_eq!(
            name::normalize(&units(input), &current).unwrap(),
            units(expected),
            "{input}"
        );
    }
    assert!(name::normalize(&units("data.xp3>../bad"), &current).is_err());
    assert_eq!(
        name::normalize(
            &units("../next.ks"),
            &units("file://./c/data.xp3>scenario/")
        )
        .unwrap(),
        units("file://./c/data.xp3>next.ks")
    );
    assert_eq!(
        name::split_ext(&units("a.dir/data.xp3>.profile")).1,
        units(".profile")
    );
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("Old")).unwrap();
    fs::create_dir(dir.path().join("Patch")).unwrap();
    fs::write(dir.path().join("Old/Start.TJS"), b"old").unwrap();
    fs::write(dir.path().join("Patch/Start.TJS"), b"patch").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    vfs.add_path(&units("old/")).unwrap();
    vfs.add_path(&units("patch/")).unwrap();
    vfs.add_path(&units("old/")).unwrap();
    assert_eq!(
        vfs.plan(&units("missing/start.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"patch"
    );
    fs::write(dir.path().join("Start.TJS"), b"local").unwrap();
    assert_eq!(
        vfs.plan(&units("start.tjs")).unwrap().read(0).unwrap(),
        b"local"
    );
    vfs.remove_path(&units("patch/")).unwrap();
    assert_eq!(
        vfs.plan(&units("missing/start.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"old"
    );
    assert!(vfs.placed_path(&units("missing.tjs")).unwrap().is_empty());
    assert!(vfs.add_path(&units("bad")).is_err());
    assert!(vfs.placed_path(&units("old/")).unwrap().is_empty());
    vfs.write(&units("missing/start.tjs"), Some(1), b"X")
        .unwrap();
    assert_eq!(fs::read(dir.path().join("Old/Start.TJS")).unwrap(), b"oXd");
}

#[test]
fn text_formats_offsets_surrogates_and_decompression_limits() {
    let mut sample = units("中文\r\nabc");
    sample.push(0xd800);
    for mode in ["", "c", "c1", "z0", "z9"] {
        let bytes = text::encode(&sample, &units(mode), 1024).unwrap();
        assert_eq!(
            text::decode(&bytes, &units("utf-8"), 1024).unwrap(),
            sample,
            "{mode}"
        );
    }
    // Mode 0 is an old read-only format; apply its known wire transform.
    let raw = [0xfe, 0xfe, 0, 0xff, 0xfe]
        .into_iter()
        .chain(
            units("abc")
                .iter()
                .flat_map(|&u| (u ^ (((u & 0xfe) << 8) ^ 1)).to_le_bytes()),
        )
        .collect::<Vec<_>>();
    assert_eq!(
        text::decode(&raw, &units("utf-8"), 1024).unwrap(),
        units("abc")
    );
    assert_eq!(
        text::decode(&[0x93, 0xfa, 0x96, 0x7b], &units("shift-jis"), 1024).unwrap(),
        units("日本")
    );
    let zipped = text::encode(&vec![65; 1000], &units("z"), 10000).unwrap();
    assert!(matches!(
        text::decode(&zipped, &units("utf-8"), 256),
        Err(Error::Limit(_))
    ));
    assert!(text::decode(&zipped[..zipped.len() - 1], &units("utf-8"), 10000).is_err());
    assert!(text::encode(&sample, &units("utf-8"), 1024).is_err());
    assert_eq!(text::offset(&units("z9o123")).unwrap(), Some(123));
}

#[test]
fn xp3_segments_seeks_index_chain_embedded_exe_and_retained_plans() {
    let dir = tempfile::tempdir().unwrap();
    let data = b"0123456789abcdefghij";
    for compressed in [false, true] {
        for embedded in [false, true] {
            fs::write(
                dir.path().join("data.xp3"),
                support::archive("Scenario/Start.TJS", data, compressed, embedded),
            )
            .unwrap();
            let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
            vfs.add_path(&units("data.xp3>scenario/")).unwrap();
            let plan = vfs.plan(&units("start.tjs")).unwrap();
            vfs.clear_archive_cache(); // plans independently retain metadata
            assert_eq!(plan.read(0).unwrap(), data);
            assert_eq!(plan.read(8).unwrap(), &data[8..]);
            let mut a = plan.open().unwrap();
            let mut b = plan.open().unwrap();
            a.seek(SeekFrom::Start(12)).unwrap();
            let mut out = [0; 3];
            a.read_exact(&mut out).unwrap();
            assert_eq!(&out, b"cde");
            b.read_exact(&mut out).unwrap();
            assert_eq!(&out, b"012");
            a.seek(SeekFrom::Current(-2)).unwrap();
            a.read_exact(&mut out).unwrap();
            assert_eq!(&out, b"def");
            a.seek(SeekFrom::End(-2)).unwrap();
            assert_eq!(a.read(&mut out).unwrap(), 2);
            assert_eq!(&out[..2], b"ij");
            a.seek(SeekFrom::Start(100)).unwrap();
            assert_eq!(a.read(&mut out).unwrap(), 0);
            drop(a);
            drop(b);
            fs::write(dir.path().join("data.xp3"), b"changed").unwrap();
            assert!(plan.read(0).is_err());
        }
    }
}

#[test]
fn filters_use_logical_offsets_independent_state_and_validated_boundaries() {
    struct Factory(Arc<Mutex<Vec<u64>>>);
    struct Filter(Arc<Mutex<Vec<u64>>>);
    impl xp3::FilterFactory for Factory {
        fn create(
            &self,
            name: &[u16],
            entry: &xp3::Entry,
        ) -> krkr_assets::Result<Box<dyn xp3::Filter>> {
            assert!(name.ends_with(&units(">test")));
            assert_eq!(entry.hash, 0x12345678);
            assert!(entry.protected);
            Ok(Box::new(Filter(self.0.clone())))
        }
    }
    impl xp3::Filter for Filter {
        fn apply(&mut self, offset: u64, bytes: &mut [u8]) -> std::io::Result<()> {
            self.0.lock().unwrap().push(offset);
            for b in bytes {
                *b ^= 1;
            }
            Ok(())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let data = support::archive("test", b"0123456789abcdefghij", true, false);
    fs::write(dir.path().join("data.xp3"), &data).unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let offsets = Arc::new(Mutex::new(Vec::new()));
    vfs.set_filter(Some(Arc::new(Factory(offsets.clone()))));
    let plan = vfs.plan(&units("data.xp3>test")).unwrap();
    assert!(!plan.same_file_version(&vfs.plan(&units("data.xp3>test")).unwrap()));
    let value = plan.read(8).unwrap();
    assert_eq!(
        value,
        b"89abcdefghij".iter().map(|b| b ^ 1).collect::<Vec<_>>()
    );
    assert_eq!(*offsets.lock().unwrap(), [8, 10]);
    assert!(
        vfs.write(&units("data.xp3>test"), None, b"overwrite")
            .is_err()
    );
    let mut tiny = Vfs::new(
        dir.path(),
        Limits {
            max_read_bytes: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        tiny.plan(&units("data.xp3>test")).unwrap().read(0),
        Err(Error::Limit(_))
    ));
    let mut index_tiny = Vfs::new(
        dir.path(),
        Limits {
            max_index_bytes: 8,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(index_tiny.plan(&units("data.xp3>test")).is_err());
    let mut cyclic = data.clone();
    let index = u64::from_le_bytes(cyclic[11..19].try_into().unwrap()) as usize;
    cyclic[index + 9..index + 17].copy_from_slice(&(index as u64).to_le_bytes());
    fs::write(dir.path().join("cycle.xp3"), cyclic).unwrap();
    assert!(vfs.plan(&units("cycle.xp3>test")).is_err());
    fs::write(dir.path().join("bad.xp3"), &data[..data.len() - 1]).unwrap();
    assert!(vfs.plan(&units("bad.xp3>test")).is_err());
}

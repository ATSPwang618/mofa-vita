mod support;
use krkr_assets::{
    Limits, name,
    xp3::{self, Archive, Compression, Filter, FilterFactory, Writer, offline},
};
use std::{
    fs,
    io::{self, Cursor, Read},
    sync::Arc,
};

#[test]
fn pack_roundtrip_is_deterministic_and_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir_all(source.join("Images")).unwrap();
    fs::write(source.join("Images/背景.bin"), vec![0xa5; 150_000]).unwrap();
    fs::write(source.join("empty"), []).unwrap();
    fs::write(source.join("Startup.tjs"), b"Wikipedia").unwrap();
    for (i, compression) in [Compression::None, Compression::Zlib, Compression::Auto]
        .into_iter()
        .enumerate()
    {
        let packed = root.path().join(format!("{i}.xp3"));
        let summary =
            offline::pack_directory(&source, &packed, compression, Limits::default()).unwrap();
        assert_eq!((summary.files, summary.bytes), (3, 150_009));
        let second = root.path().join(format!("{i}-second.xp3"));
        offline::pack_directory(&source, &second, compression, Limits::default()).unwrap();
        let original = fs::read(&packed).unwrap();
        assert_eq!(original, fs::read(second).unwrap());
        assert!(offline::pack_directory(&source, &packed, compression, Limits::default()).is_err());
        assert_eq!(original, fs::read(&packed).unwrap());
        let archive = Archive::load_strict(&packed, Limits::default()).unwrap();
        assert_eq!(
            archive
                .entries
                .get(&name::units("startup.tjs"))
                .unwrap()
                .hash,
            0x11e60398
        );
        assert!(archive.entries.values().all(|entry| !entry.protected));
        let output = root.path().join(format!("out-{i}"));
        offline::unpack_archive(&packed, &output, None, Limits::default()).unwrap();
        assert_eq!(
            fs::read(output.join("images/背景.bin")).unwrap(),
            vec![0xa5; 150_000]
        );
        assert!(fs::read(output.join("empty")).unwrap().is_empty());
        assert_eq!(fs::read(output.join("startup.tjs")).unwrap(), b"Wikipedia");
        assert!(offline::unpack_archive(&packed, &output, None, Limits::default()).is_err());
    }
}

#[test]
fn protected_plaintext_uses_checksum_instead_of_rejecting_the_flag() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("protected.xp3");
    let plaintext = vec![0xa5; 180_000];
    for (i, data) in [plaintext.as_slice(), b""].into_iter().enumerate() {
        let hash = adler2::adler32_slice(data);
        fs::write(
            &source,
            support::archive_with_hash("Dir/背景.bin", data, true, true, hash),
        )
        .unwrap();
        let events = std::sync::Mutex::new(Vec::new());
        let output = root.path().join(format!("out-{i}"));
        let summary = offline::unpack_archive_with_progress(
            &source,
            &output,
            None,
            Limits::default(),
            &|event| {
                let value = match event {
                    offline::Progress::Started { files } => (files, 0),
                    offline::Progress::File { name, bytes } => {
                        assert_eq!(String::from_utf16(name).unwrap(), "dir/背景.bin");
                        (1, bytes)
                    }
                };
                events.lock().unwrap().push(value);
            },
        )
        .unwrap();
        assert_eq!((summary.files, summary.bytes), (1, data.len() as u64));
        assert_eq!(fs::read(output.join("dir/背景.bin")).unwrap(), data);
        assert_eq!(*events.lock().unwrap(), [(1, 0), (1, data.len() as u64)]);

        // A corrupt checksum must still fail, even when the data looks plausible.
        let rejected = root.path().join(format!("rejected-{i}"));
        fs::write(
            &source,
            support::archive_with_hash("Dir/背景.bin", data, true, true, hash ^ 1),
        )
        .unwrap();
        let error = offline::unpack_archive(&source, &rejected, None, Limits::default())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("Adler-32") && error.contains("--xp3-filter"),
            "{error}"
        );
        assert!(!rejected.exists());
    }
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 3);
}

struct XorFactory;
struct XorFilter(u64);
impl FilterFactory for XorFactory {
    fn create(&self, storage: &[u16], entry: &xp3::Entry) -> krkr_assets::Result<Box<dyn Filter>> {
        assert!(
            String::from_utf16(storage)
                .unwrap()
                .ends_with("encrypted.xp3>dir/file.bin")
        );
        assert!(entry.protected);
        assert_eq!(entry.hash, 0x12345678);
        Ok(Box::new(XorFilter(0)))
    }
}
impl Filter for XorFilter {
    fn apply(&mut self, offset: u64, bytes: &mut [u8]) -> io::Result<()> {
        assert_eq!(offset, self.0);
        self.0 += bytes.len() as u64;
        for byte in bytes {
            *byte ^= 0xa5;
        }
        Ok(())
    }
}

#[test]
fn decrypt_embedded_chained_mixed_segments_and_extract_plaintext() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("encrypted.xp3");
    let plaintext: Vec<u8> = (0..180_000).map(|n| (n % 251) as u8).collect();
    let encrypted: Vec<u8> = plaintext.iter().map(|b| b ^ 0xa5).collect();
    fs::write(
        &source,
        support::archive("Dir/File.bin", &encrypted, true, true),
    )
    .unwrap();
    let rejected = root.path().join("no-filter");
    assert!(offline::unpack_archive(&source, &rejected, None, Limits::default()).is_err());
    assert!(!rejected.exists());
    let packed = root.path().join("plain.xp3");
    offline::decrypt_archive(
        &source,
        &packed,
        &XorFactory,
        Compression::Zlib,
        Limits::default(),
    )
    .unwrap();
    let archive = Arc::new(Archive::load_strict(&packed, Limits::default()).unwrap());
    let entry = archive.entries.get(&name::units("dir/file.bin")).unwrap();
    assert!(!entry.protected);
    let mut actual = Vec::new();
    xp3::Reader::new(archive, entry, None)
        .read_to_end(&mut actual)
        .unwrap();
    assert_eq!(actual, plaintext);
    for (input, filter, output) in [
        (&source, Some(&XorFactory as &dyn FilterFactory), "direct"),
        (&packed, None, "plain"),
    ] {
        let output = root.path().join(output);
        offline::unpack_archive(input, &output, filter, Limits::default()).unwrap();
        assert_eq!(fs::read(output.join("dir/file.bin")).unwrap(), plaintext);
    }
}

#[test]
fn unsafe_names_and_duplicate_entries_are_rejected_before_export() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("invalid.xp3");
    let output = root.path().join("output");
    for name in [
        "../escape",
        "/absolute",
        "a/../b",
        "C:/drive",
        "a:stream",
        "a\0ignored",
        "a//b",
        "NUL.txt",
        "dir./file",
        "a/...",
    ] {
        fs::write(&source, support::archive(name, b"data", false, false)).unwrap();
        assert!(
            Archive::load_strict(&source, Limits::default()).is_err(),
            "{name}"
        );
        assert!(offline::unpack_archive(&source, &output, None, Limits::default()).is_err());
        assert!(!output.exists());
    }
    let mut bytes = support::archive("same", b"data", false, false);
    let first = u64::from_le_bytes(bytes[11..19].try_into().unwrap()) as usize;
    let second = first + 17;
    let len = u64::from_le_bytes(bytes[second + 1..second + 9].try_into().unwrap());
    let index = bytes[second + 9..].to_vec();
    bytes[second + 1..second + 9].copy_from_slice(&(len * 2).to_le_bytes());
    bytes.extend(index);
    fs::write(&source, bytes).unwrap();
    assert!(Archive::load(&source, Limits::default()).is_ok()); // Runtime first-entry semantics remain.
    assert!(Archive::load_strict(&source, Limits::default()).is_err());
}

#[test]
fn writer_failure_and_filter_failure_never_publish_partial_archives() {
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::None,
        Limits::default(),
    )
    .unwrap();
    writer.add(&name::units("FILE"), &mut &b"data"[..]).unwrap();
    assert!(
        writer
            .add(&name::units("file"), &mut &b"other"[..])
            .is_err()
    );
    assert!(writer.finish().is_err());
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::None,
        Limits {
            max_index_bytes: 10,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(writer.add(&name::units("file"), &mut &b"data"[..]).is_err());
    assert!(writer.finish().is_err());

    struct Broken;
    impl FilterFactory for Broken {
        fn create(&self, _: &[u16], _: &xp3::Entry) -> krkr_assets::Result<Box<dyn Filter>> {
            Ok(Box::new(Self))
        }
    }
    impl Filter for Broken {
        fn apply(&mut self, _: u64, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::other("bad key"))
        }
    }
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("encrypted.xp3");
    fs::write(
        &source,
        support::archive("file.bin", b"encrypted", false, false),
    )
    .unwrap();
    let packed = root.path().join("partial.xp3");
    assert!(
        offline::decrypt_archive(
            &source,
            &packed,
            &Broken,
            Compression::None,
            Limits::default()
        )
        .is_err()
    );
    assert!(!packed.exists());
    let output = root.path().join("partial");
    assert!(offline::unpack_archive(&source, &output, Some(&Broken), Limits::default()).is_err());
    assert!(!output.exists());
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}

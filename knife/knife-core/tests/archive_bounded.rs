use std::{
    borrow::Cow,
    fs::{self, File},
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use knife_core::format::{
    archive::{Archive, ArchiveLimits, SECTOR},
    nx,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new(records: &[(u32, u16, &str)], bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "knife-archive-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let mut hed = Vec::new();
        let mut nam = Vec::new();
        for &(offset, sectors, name) in records {
            hed.extend_from_slice(&offset.to_le_bytes());
            hed.extend_from_slice(&sectors.to_le_bytes());
            hed.extend_from_slice(&0u16.to_le_bytes());
            let mut record = [0u8; 32];
            record[..name.len()].copy_from_slice(name.as_bytes());
            nam.extend_from_slice(&record);
        }
        hed.extend_from_slice(&[0xff; 8]);
        fs::write(path.join("test.hed"), hed).unwrap();
        fs::write(path.join("test.nam"), nam).unwrap();
        fs::write(path.join("test.mrg"), bytes).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for name in ["test.hed", "test.nam", "test.mrg"] {
            let _ = fs::remove_file(self.0.join(name));
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn wrapped(payload: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(payload).unwrap();
    let body = encoder.finish().unwrap();
    let mut bytes = b"NXCX".to_vec();
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&body);
    bytes
}

#[test]
fn truncated_sector_payload_is_an_error_instead_of_partial_success() {
    let f = Fixture::new(&[(0, 1, "partial.bin")], b"bad");
    let a = Archive::open(f.path(), "test").unwrap();
    assert!(a.read(0).is_err());
}

#[test]
fn file_backend_matches_mapped_bytes_and_preserves_lookup_indices() {
    let bytes = [0x5au8; SECTOR as usize];
    let f = Fixture::new(
        &[
            (0, 1, "first.bin"),
            (0, 1, "FIRST.BIN"),
            (0, 0, "NOFILE.EXISTS"),
        ],
        &bytes,
    );
    let file = Archive::open_file(f.path(), "test").unwrap();
    let mapped = Archive::open(f.path(), "test").unwrap();
    assert_eq!(file.find("FIRST.BIN"), Some(0));
    assert_eq!(file.find_stem("first"), Some(0));
    assert_eq!(file.find("nofile.exists"), None);
    assert_eq!(file.read(0).unwrap(), mapped.read(0).unwrap());
    assert!(matches!(file.raw(0).unwrap(), Some(Cow::Owned(_))));
    #[cfg(not(target_os = "vita"))]
    assert!(matches!(mapped.raw(0).unwrap(), Some(Cow::Borrowed(_))));
    assert!(file.raw(2).unwrap().is_none());
    assert!(file.raw(99).unwrap().is_none());
}

#[test]
fn nx_header_controls_extent_and_bounds_output_before_reading() {
    let payload: Vec<_> = (0..10000)
        .map(|i| ((i * 113 + i / 256) % 256) as u8)
        .collect();
    let wrapped = wrapped(&payload);
    let f = Fixture::new(&[(0, 0, "image.bin")], &wrapped);
    let a = Archive::open_file(f.path(), "test").unwrap();
    assert_eq!(a.raw(0).unwrap().unwrap().len(), wrapped.len());
    assert_eq!(a.read(0).unwrap(), payload);
    let limited = Archive::open_file_with_limits(
        f.path(),
        "test",
        ArchiveLimits {
            max_stored_bytes: 20000,
            max_unpacked_bytes: 9999,
        },
    )
    .unwrap();
    assert!(limited.raw(0).is_err());
}

#[test]
fn invalid_wrapper_magic_and_truncated_headers_are_errors() {
    for magic in [b"NXZZ", b"NXCX"] {
        let mut data = vec![0; SECTOR as usize];
        data[..4].copy_from_slice(magic);
        let f = Fixture::new(&[(0, 1, "invalid.bin")], &data);
        let a = Archive::open_file(f.path(), "test").unwrap();
        assert!(a.read(0).is_err());
    }
    let f = Fixture::new(&[(0, 1, "short.bin")], b"NXCX");
    assert!(
        Archive::open_file(f.path(), "test")
            .unwrap()
            .read(0)
            .is_err()
    );
}

#[test]
fn extent_and_budget_fail_before_allocating_large_entries() {
    let f = Fixture::new(
        &[(u32::MAX - 1, 1, "outside.bin"), (0, 1, "over.bin")],
        &[0; SECTOR as usize],
    );
    let a = Archive::open_file_with_limits(
        f.path(),
        "test",
        ArchiveLimits {
            max_stored_bytes: 1024,
            max_unpacked_bytes: 1024,
        },
    )
    .unwrap();
    assert!(a.read(0).is_err());
    assert!(a.read(1).is_err());
    let mut data = wrapped(b"small");
    data[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    let f = Fixture::new(&[(0, 1, "overflow.bin")], &data);
    let a = Archive::open_file(f.path(), "test").unwrap();
    assert!(a.read(0).is_err());
    data[8..12].copy_from_slice(&8u32.to_le_bytes());
    data[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(nx::decompress_bounded(&data, 1024).is_err());
}

#[test]
fn uncompressed_entry_also_obeys_the_decoded_budget() {
    let f = Fixture::new(&[(0, 1, "plain.bin")], &[0; SECTOR as usize]);
    let a = Archive::open_file_with_limits(
        f.path(),
        "test",
        ArchiveLimits {
            max_stored_bytes: SECTOR as usize,
            max_unpacked_bytes: 1024,
        },
    )
    .unwrap();
    assert!(a.read(0).is_err());
}

#[test]
fn file_changed_after_open_reports_short_read() {
    let f = Fixture::new(&[(0, 1, "short.bin")], &[0; SECTOR as usize]);
    let a = Archive::open_file(f.path(), "test").unwrap();
    File::options()
        .write(true)
        .open(f.0.join("test.mrg"))
        .unwrap()
        .set_len(3)
        .unwrap();
    assert!(a.read(0).is_err());
}

#[test]
fn file_backend_seeks_beyond_four_gib_without_mapping_the_archive() {
    let offset = 5 * 1024 * 1024 * 1024u64;
    let f = Fixture::new(&[((offset / SECTOR) as u32, 1, "tail.bin")], &[]);
    let mut file = File::options()
        .write(true)
        .open(f.0.join("test.mrg"))
        .unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&[0x67; SECTOR as usize]).unwrap();
    drop(file);
    let a = Archive::open_file(f.path(), "test").unwrap();
    assert_eq!(a.read(0).unwrap(), vec![0x67; SECTOR as usize]);
}

#[test]
fn concurrent_file_reads_do_not_share_an_unlocked_seek_position() {
    let mut bytes = vec![0x31; SECTOR as usize];
    bytes.extend_from_slice(&[0xa6; SECTOR as usize]);
    let f = Fixture::new(&[(0, 1, "a.bin"), (1, 1, "b.bin")], &bytes);
    let a = Archive::open_file(f.path(), "test").unwrap();
    std::thread::scope(|scope| {
        for index in 0..8 {
            let a = &a;
            scope.spawn(move || {
                let entry = index % 2;
                let expected = if entry == 0 { 0x31 } else { 0xa6 };
                for _ in 0..100 {
                    assert!(a.read(entry).unwrap().iter().all(|&byte| byte == expected));
                }
            });
        }
    });
}

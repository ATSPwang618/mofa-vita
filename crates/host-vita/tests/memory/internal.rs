use super::*;
use krkr_protocol::budget::Budget;

#[cfg(not(target_os = "vita"))]
#[test]
fn startup_archive_searches_reuse_indexes_within_the_byte_budget() {
    use krkr_assets::{
        ReadSource, Stream, Vfs,
        archive::{ArchiveEntry, ArchiveFormat, ArchiveIndex},
        name::units,
        xp3::Version,
    };
    use std::{
        collections::BTreeMap,
        fs,
        io::Cursor,
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct Source;
    impl ReadSource for Source {
        fn open(&self) -> krkr_assets::Result<Box<dyn Stream>> {
            Ok(Box::new(Cursor::new(b"asset")))
        }
    }
    struct Archive {
        opens: Arc<AtomicUsize>,
        bytes: usize,
    }
    impl ArchiveFormat for Archive {
        fn open(
            &self,
            path: &Path,
            _: krkr_assets::Limits,
        ) -> krkr_assets::Result<Option<ArchiveIndex>> {
            self.opens.fetch_add(1, Ordering::Relaxed);
            Ok(Some(ArchiveIndex {
                version: Version::of(&fs::File::open(path)?)?,
                entries: BTreeMap::from([(
                    units("asset"),
                    ArchiveEntry {
                        bytes: 5,
                        source: Arc::new(Source),
                    },
                )]),
                index_bytes: self.bytes,
            }))
        }
    }

    // Ten small indexes must survive repeated image/music/script searches.
    // Larger indexes still evict each other when their total exceeds 16 MiB.
    for (archives, bytes, expected_opens) in [(10, 128 * 1024, 10), (3, 6 * MIB, 6)] {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..archives {
            fs::write(dir.path().join(format!("{i}.pkg")), b"archive").unwrap();
        }
        let mut vfs = Vfs::new(dir.path(), storage_limits()).unwrap();
        let opens = Arc::new(AtomicUsize::new(0));
        vfs.register_archive_format(Arc::new(Archive {
            opens: opens.clone(),
            bytes,
        }));
        for _ in 0..2 {
            for i in 0..archives {
                let plan = vfs.plan(&units(&format!("{i}.pkg>asset"))).unwrap();
                assert_eq!(plan.read(0).unwrap(), b"asset");
            }
        }
        assert_eq!(opens.load(Ordering::Relaxed), expected_opens);
    }
}

#[test]
fn scratch_borrows_spare_capacity_without_exceeding_the_shared_limit() {
    let config = graphics_config(Budget::new(MIB));
    let resident = config.resident.reserve(GRAPHICS_BYTES - 50 * MIB).unwrap();
    let scratch = config.scratch.reserve(40 * MIB).unwrap();
    assert_eq!(config.resident.available(), 10 * MIB);
    assert_eq!(config.scratch.available(), 8 * MIB);
    assert!(config.resident.reserve(11 * MIB).is_err());
    assert_eq!(config.resident.used(), GRAPHICS_BYTES - 50 * MIB);
    drop((scratch, resident));
    assert_eq!(config.resident.used(), 0);
    assert_eq!(config.scratch.used(), 0);
}

#[test]
fn scene_upload_borrows_idle_scratch_capacity_without_expanding_total_budget() {
    let config = graphics_config(Budget::new(MIB));
    // Captured scene failure: a 6.2 MiB sky failed at the old 80 MiB sublimit.
    let resident = config.resident.reserve(82_626_992).unwrap();
    let scratch = config.scratch.reserve(6_267_904).unwrap();
    let sky = config.resident.reserve(6_458_976).unwrap();
    let remaining = GRAPHICS_BYTES - 82_626_992 - 6_267_904 - 6_458_976;
    assert_eq!(config.resident.available(), remaining);
    assert_eq!(
        config.scratch.available(),
        remaining.min(SCRATCH_BYTES - 6_267_904)
    );
    assert!(config.resident.reserve(remaining + 1).is_err());
    assert!(config.scratch.reserve(remaining + 1).is_err());
    drop((sky, resident, scratch));
    assert_eq!(config.resident.available(), GRAPHICS_BYTES);
}

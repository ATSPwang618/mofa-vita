use krkr_assets::{
    Limits, ReadPlan, ReadSource, Search, StorageMedium, Stream, Vfs,
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

#[test]
fn archive_relative_overlay_uses_exact_entry_and_preserves_explicit_addresses() {
    use krkr_assets::xp3::{Compression, offline};
    let root = tempfile::tempdir().unwrap();
    for (directory, content) in [
        ("original", b"original".as_slice()),
        ("other", b"other"),
        ("patch", b"patched"),
    ] {
        let source = root.path().join(directory);
        fs::create_dir_all(source.join("sub")).unwrap();
        fs::write(source.join("sub/layer.override.tjs"), content).unwrap();
        fs::write(source.join("sub/unmodified.tjs"), b"untouched").unwrap();
        offline::pack_directory(
            &source,
            &root.path().join(format!("{directory}.xp3")),
            Compression::None,
            Limits::default(),
        )
        .unwrap();
    }
    let mut vfs = Vfs::new(root.path(), Limits::default()).unwrap();
    vfs.add_path(&units("patch.xp3>")).unwrap();
    vfs.set_directory(&units("original.xp3>sub/")).unwrap();
    assert_eq!(
        vfs.plan(&units("layer.override.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"patched"
    );
    assert_eq!(
        vfs.plan(&units("../sub/layer.override.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"patched"
    );
    assert_eq!(
        vfs.plan(&units(&format!(
            "{}/original.xp3>sub/layer.override.tjs",
            root.path().display()
        )))
        .unwrap()
        .read(0)
        .unwrap(),
        b"original"
    );
    assert_eq!(
        vfs.plan(&units(&format!(
            "{}/other.xp3>sub/layer.override.tjs",
            root.path().display()
        )))
        .unwrap()
        .read(0)
        .unwrap(),
        b"other"
    );
    fs::create_dir(root.path().join("loose")).unwrap();
    fs::write(
        root.path().join("loose/layer.override.tjs"),
        b"wrong basename",
    )
    .unwrap();
    let loose = root
        .path()
        .join("loose")
        .to_string_lossy()
        .replace('\\', "/")
        + "/";
    vfs.add_path(&units(&loose)).unwrap();
    assert_eq!(
        vfs.plan(&units("layer.override.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"patched"
    );
    fs::create_dir(root.path().join("loose/sub")).unwrap();
    fs::write(
        root.path().join("loose/sub/layer.override.tjs"),
        b"loose override",
    )
    .unwrap();
    assert_eq!(
        vfs.plan(&units("layer.override.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"loose override"
    );
    let root_directory = root.path().to_string_lossy().replace('\\', "/") + "/";
    vfs.set_directory(&units(&root_directory)).unwrap();
    vfs.add_path(&units("original.xp3>sub/")).unwrap();
    vfs.set_directory(&units(&format!("{}/original.xp3>", root.path().display())))
        .unwrap();
    assert_eq!(
        vfs.plan(&units("layer.override.tjs"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"loose override"
    );

    // Real KAG startup keeps the archive root as cwd and registers several
    // namespaces with duplicate basenames. Missing later directories must not
    // make another archive's dependency replace the first existing candidate.
    let data = root.path().join("data");
    fs::create_dir_all(data.join("others")).unwrap();
    fs::write(data.join("others/unmodified.tjs"), b"different namespace").unwrap();
    offline::pack_directory(
        &data,
        &root.path().join("data.xp3"),
        Compression::None,
        Limits::default(),
    )
    .unwrap();
    vfs.add_path(&units(&format!(
        "{}/data.xp3>others/",
        root.path().display()
    )))
    .unwrap();
    vfs.remove_path(&units(&format!(
        "{}/original.xp3>sub/",
        root.path().display()
    )))
    .unwrap();
    vfs.add_path(&units(&format!(
        "{}/original.xp3>sub/",
        root.path().display()
    )))
    .unwrap();
    vfs.add_path(&units(&format!(
        "{}/data.xp3>absent/",
        root.path().display()
    )))
    .unwrap();
    assert_eq!(
        vfs.plan(&units("unmodified.tjs")).unwrap().read(0).unwrap(),
        b"untouched"
    );
}

#[test]
fn sibling_lookup_keeps_case_links_and_live_file_versions() {
    use krkr_assets::Lookup;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("Actor.PNG"), b"sprite").unwrap();
    fs::write(
        dir.path().join("old.tlg.krkr-link"),
        krkr_assets::converted::encode_link("actor.png").unwrap(),
    )
    .unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let mut lookup = Lookup::default();
    let mut find = |name: &str, lookup: &mut Lookup| {
        let path = vfs.full_path(&units(name)).unwrap();
        vfs.direct_plan_in(&path, lookup).unwrap()
    };
    assert!(find("absent_m.bmp", &mut lookup).is_none());
    assert!(find("absent_m.png", &mut lookup).is_none());
    let plan = find("old.tlg", &mut lookup).unwrap();
    assert_eq!(plan.read(0).unwrap(), b"sprite");
    // A new batch must see a file written after an earlier miss.
    fs::write(dir.path().join("absent_m.png"), b"mask").unwrap();
    assert_eq!(
        find("absent_m.png", &mut Lookup::default())
            .unwrap()
            .read(0)
            .unwrap(),
        b"mask"
    );
    // Positive plans still detect replacement; they are never content snapshots.
    fs::write(dir.path().join("Actor.PNG"), b"changed sprite").unwrap();
    assert!(matches!(plan.read(0), Err(krkr_assets::Error::Changed)));
}

#[test]
fn oversized_directory_does_not_turn_partial_listing_into_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..1100 {
        fs::write(dir.path().join(format!("image{i:04}")), b"image").unwrap();
    }
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let mut lookup = krkr_assets::Lookup::default();
    let missing = vfs.full_path(&units("absent")).unwrap();
    assert!(vfs.direct_plan_in(&missing, &mut lookup).unwrap().is_none());
    for i in [0, 550, 1099] {
        let path = vfs.full_path(&units(&format!("image{i:04}"))).unwrap();
        assert_eq!(
            vfs.direct_plan_in(&path, &mut lookup)
                .unwrap()
                .unwrap()
                .read(0)
                .unwrap(),
            b"image"
        );
    }
}

#[test]
fn missing_auto_paths_are_pruned_and_reindexed_when_written() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("images")).unwrap();
    fs::write(dir.path().join("images/button.png"), b"image").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    vfs.add_path(&units("images/")).unwrap();
    for path in ["temp/", "temp2/", "#patch/"] {
        vfs.add_path(&units(path)).unwrap();
    }
    assert!(
        vfs.search_candidates(&units("button_m.png"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        vfs.search_candidates(&units("button.png")).unwrap(),
        [vfs.full_path(&units("images/button.png")).unwrap()]
    );

    // Engine writes invalidate the directory index, including an absent path.
    fs::create_dir(dir.path().join("#patch")).unwrap();
    vfs.write(&units("#patch/button.png"), None, b"patch")
        .unwrap();
    assert_eq!(
        vfs.plan(&units("button.png")).unwrap().read(0).unwrap(),
        b"patch"
    );
}

#[test]
fn a_fresh_lookup_sees_a_directory_created_after_a_sibling_miss() {
    let dir = tempfile::tempdir().unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let mut lookup = krkr_assets::Lookup::default();
    for name in ["temp/button.bmp", "temp/button.png", "temp/button_m.png"] {
        let path = vfs.full_path(&units(name)).unwrap();
        assert!(vfs.direct_plan_in(&path, &mut lookup).unwrap().is_none());
    }
    fs::create_dir(dir.path().join("temp")).unwrap();
    fs::write(dir.path().join("temp/button.png"), b"new image").unwrap();
    let path = vfs.full_path(&units("temp/button.png")).unwrap();
    assert_eq!(
        vfs.direct_plan_in(&path, &mut krkr_assets::Lookup::default())
            .unwrap()
            .unwrap()
            .read(0)
            .unwrap(),
        b"new image"
    );
}

#[test]
fn missing_siblings_keep_case_folded_parent_and_live_positive_metadata() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("Sprites")).unwrap();
    let sprite = dir.path().join("Sprites/Actor.PNG");
    fs::write(&sprite, b"sprite").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let mut lookup = krkr_assets::Lookup::default();
    for name in ["sprites/absent.bmp", "sprites/absent.png"] {
        let path = vfs.full_path(&units(name)).unwrap();
        assert!(vfs.direct_plan_in(&path, &mut lookup).unwrap().is_none());
    }
    let path = vfs.full_path(&units("sprites/actor.png")).unwrap();
    let first = vfs.direct_plan_in(&path, &mut lookup).unwrap().unwrap();
    assert_eq!(first.read(0).unwrap(), b"sprite");
    fs::write(&sprite, b"replaced sprite").unwrap();
    let second = vfs.direct_plan_in(&path, &mut lookup).unwrap().unwrap();
    assert_eq!(second.read(0).unwrap(), b"replaced sprite");
    assert!(matches!(first.read(0), Err(krkr_assets::Error::Changed)));
}

#[cfg(not(windows))]
#[test]
fn sibling_snapshot_does_not_hide_ambiguous_case_folded_names() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("ACTOR.png"), b"first").unwrap();
    fs::write(dir.path().join("Actor.png"), b"second").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let mut lookup = krkr_assets::Lookup::default();
    let missing = vfs.full_path(&units("absent.png")).unwrap();
    assert!(vfs.direct_plan_in(&missing, &mut lookup).unwrap().is_none());
    let path = vfs.full_path(&units("actor.png")).unwrap();
    assert!(matches!(
        vfs.direct_plan_in(&path, &mut lookup),
        Err(krkr_assets::Error::Name("ambiguous case-folded host path"))
    ));
}

struct Bytes;
impl ReadSource for Bytes {
    fn open(&self) -> krkr_assets::Result<Box<dyn Stream>> {
        Ok(Box::new(Cursor::new(b"contents".to_vec())))
    }
}
struct CountedArchive(Arc<AtomicUsize>);
impl ArchiveFormat for CountedArchive {
    fn open(&self, path: &Path, _: Limits) -> krkr_assets::Result<Option<ArchiveIndex>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Some(ArchiveIndex {
            version: Version::of(&fs::File::open(path)?)?,
            entries: BTreeMap::from([(
                units("item"),
                ArchiveEntry {
                    bytes: 8,
                    source: Arc::new(Bytes),
                },
            )]),
            index_bytes: 128,
        }))
    }
}

#[test]
fn sibling_archive_probes_do_not_pin_evicted_indices() {
    for retained in [0, 1, 2] {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.pkg", "b.pkg"] {
            fs::write(dir.path().join(name), b"archive").unwrap();
        }
        let mut vfs = Vfs::new(
            dir.path(),
            Limits {
                max_cached_archives: retained,
                ..Limits::default()
            },
        )
        .unwrap();
        let opens = Arc::new(AtomicUsize::new(0));
        vfs.register_archive_format(Arc::new(CountedArchive(opens.clone())));
        let mut lookup = krkr_assets::Lookup::default();
        for name in ["a.pkg>absent", "b.pkg>absent", "a.pkg>item"] {
            let path = vfs.full_path(&units(name)).unwrap();
            let plan = vfs.direct_plan_in(&path, &mut lookup).unwrap();
            if name.ends_with("item") {
                assert_eq!(plan.unwrap().read(0).unwrap(), b"contents");
            } else {
                assert!(plan.is_none());
            }
        }
        assert_eq!(
            opens.load(Ordering::Relaxed),
            if retained == 2 { 2 } else { 3 }
        );
    }
}

#[test]
fn an_explicit_archive_hit_is_resolved_once_without_retained_index_cache() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("data.pkg"), b"archive").unwrap();
    let mut vfs = Vfs::new(
        dir.path(),
        Limits {
            max_cached_archives: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    vfs.register_archive_format(Arc::new(CountedArchive(opens.clone())));
    vfs.add_path(&units("fallback/")).unwrap();
    let plan = vfs.plan(&units("data.pkg>item")).unwrap();
    assert_eq!(opens.load(Ordering::Relaxed), 1);
    assert_eq!(plan.read(0).unwrap(), b"contents");
    // This is reuse within one search, not a stale cache across later searches.
    vfs.plan(&units("data.pkg>item")).unwrap();
    assert_eq!(opens.load(Ordering::Relaxed), 2);
}

#[test]
fn local_saves_retain_unrelated_archives_but_refresh_written_archives_and_search_paths() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("data.pkg"), b"archive").unwrap();
    fs::create_dir(dir.path().join("patch")).unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let opens = Arc::new(AtomicUsize::new(0));
    vfs.register_archive_format(Arc::new(CountedArchive(opens.clone())));
    vfs.add_path(&units("data.pkg>")).unwrap();
    vfs.add_path(&units("patch/")).unwrap();
    assert_eq!(
        vfs.plan(&units("item")).unwrap().read(0).unwrap(),
        b"contents"
    );
    for _ in 0..3 {
        vfs.write(&units("save.dat"), None, b"save").unwrap();
        assert_eq!(
            vfs.plan(&units("item")).unwrap().read(0).unwrap(),
            b"contents"
        );
    }
    assert_eq!(
        opens.load(Ordering::Relaxed),
        1,
        "saving must retain the resource index"
    );

    // A background write captures its absolute path before cwd can change.
    let plan = vfs.write_plan(&units("patch/item")).unwrap();
    let path = plan.path().to_owned();
    let mut output = plan.create().unwrap();
    output.write_all(b"patch").unwrap();
    output.finish().unwrap();
    vfs.invalidate_file(&path);
    assert_eq!(vfs.plan(&units("item")).unwrap().read(0).unwrap(), b"patch");
    assert_eq!(opens.load(Ordering::Relaxed), 1);

    // Explicit writes invalidate even if size and modification time match.
    let modified = fs::metadata(dir.path().join("data.pkg"))
        .unwrap()
        .modified()
        .unwrap();
    vfs.write(&units("data.pkg"), None, b"changed").unwrap();
    fs::File::options()
        .write(true)
        .open(dir.path().join("data.pkg"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    vfs.plan(&units("data.pkg>item")).unwrap();
    assert_eq!(opens.load(Ordering::Relaxed), 2);
}

#[test]
fn resolved_local_plans_still_validate_changes_and_preserve_explicit_priority() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("fallback")).unwrap();
    fs::write(dir.path().join("item"), b"explicit").unwrap();
    fs::write(dir.path().join("fallback/item"), b"fallback").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    vfs.add_path(&units("fallback/")).unwrap();
    let Search::Found { candidate, plan } = vfs.search(&units("item")).unwrap() else {
        panic!("ordinary hit should retain its plan");
    };
    assert_eq!(candidate, vfs.full_path(&units("item")).unwrap());
    assert_eq!(plan.read(0).unwrap(), b"explicit");
    fs::write(dir.path().join("item"), b"changed extent").unwrap();
    assert!(matches!(plan.read(0), Err(krkr_assets::Error::Changed)));
    fs::remove_file(dir.path().join("item")).unwrap();
    assert_eq!(
        vfs.plan(&units("item")).unwrap().read(0).unwrap(),
        b"fallback"
    );
}

#[test]
fn converted_video_keeps_the_requested_candidate_and_physical_read_plan() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("intro.avi.krkr-mp4"),
        krkr_assets::converted::VIDEO_MARKER,
    )
    .unwrap();
    fs::write(dir.path().join("intro.avi.mp4"), b"video").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    vfs.add_path(&units("fallback/")).unwrap();
    let Search::Found { candidate, plan } = vfs.search(&units("intro.avi")).unwrap() else {
        panic!("converted video should retain its plan");
    };
    assert_eq!(candidate, vfs.full_path(&units("intro.avi")).unwrap());
    assert_eq!(plan.name, vfs.full_path(&units("intro.avi.mp4")).unwrap());
    assert_eq!(plan.read(0).unwrap(), b"video");
    assert_eq!(
        vfs.search_candidates(&units("intro.avi")).unwrap(),
        vec![candidate]
    );
}

struct CountedMedium(Arc<AtomicUsize>);
impl StorageMedium for CountedMedium {
    fn plan(&self, _: &mut Vfs, path: &[u16]) -> krkr_assets::Result<Option<ReadPlan>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(Some(ReadPlan::custom(
            path.to_vec(),
            8,
            1024,
            Arc::new(Bytes),
        )))
    }
    fn list(&self, _: &mut Vfs, _: &[u16]) -> krkr_assets::Result<Vec<Vec<u16>>> {
        panic!("dynamic media must not be indexed speculatively");
    }
}

#[test]
fn dynamic_media_keep_ordered_candidates_and_defer_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("local"), b"local").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    vfs.register_medium("memory", Arc::new(CountedMedium(calls.clone())))
        .unwrap();
    vfs.add_path(&units("memory://./data/")).unwrap();
    for name in ["local", "remote"] {
        let Search::Candidates(candidates) = vfs.search(&units(name)).unwrap() else {
            panic!("dynamic searches must preserve their callback sequence");
        };
        assert_eq!(
            candidates,
            vec![
                vfs.full_path(&units(name)).unwrap(),
                units(&format!("memory://./data/{name}"))
            ]
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        vfs.plan(&units("local")).unwrap().read(0).unwrap(),
        b"local"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        vfs.plan(&units("remote")).unwrap().read(0).unwrap(),
        b"contents"
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn dynamic_media_defer_archive_overlay_resolution() {
    use krkr_assets::xp3::{Compression, offline};
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    fs::create_dir_all(source.join("sub")).unwrap();
    fs::write(source.join("sub/unit.tjs"), b"original").unwrap();
    offline::pack_directory(
        &source,
        &dir.path().join("original.xp3"),
        Compression::None,
        Limits::default(),
    )
    .unwrap();
    let patch = dir.path().join("patch");
    fs::create_dir_all(patch.join("sub")).unwrap();
    fs::write(patch.join("sub/unit.tjs"), b"overlay").unwrap();
    let mut vfs = Vfs::new(dir.path(), Limits::default()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    vfs.register_medium("memory", Arc::new(CountedMedium(calls.clone())))
        .unwrap();
    vfs.add_path(&units("patch/")).unwrap();
    vfs.add_path(&units("original.xp3>sub/")).unwrap();
    vfs.add_path(&units("memory://./data/")).unwrap();
    for directory in ["original.xp3>sub/", "original.xp3>"] {
        vfs.set_directory(&units(&format!("{}/{directory}", dir.path().display())))
            .unwrap();
        let Search::Candidates(candidates) = vfs.search(&units("unit.tjs")).unwrap() else {
            panic!("archive overlays must not bypass live media");
        };
        assert_eq!(candidates[0], vfs.full_path(&units("unit.tjs")).unwrap());
        assert_eq!(candidates[1], units("memory://./data/unit.tjs"));
    }
    assert_eq!(calls.load(Ordering::Relaxed), 0);
}

#[test]
fn packed_auto_paths_fit_a_small_budget_and_keep_aliases_and_patch_priority() {
    use krkr_assets::xp3::{Compression, Writer};
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::None,
        Limits::default(),
    )
    .unwrap();
    for i in 0..512 {
        writer
            .add(
                &units(&format!("voice/v{i:04}.ogg")),
                &mut Cursor::new(b"voice"),
            )
            .unwrap();
    }
    writer
        .add(&units("sprite/actor.ktx"), &mut Cursor::new(b"sprite"))
        .unwrap();
    writer
        .add(
            &units("sprite/actor.png.krkr-link"),
            &mut Cursor::new(b"KRKR-LINK-1\nactor.ktx"),
        )
        .unwrap();
    fs::write(
        dir.path().join("data.xp3"),
        writer.finish().unwrap().into_inner(),
    )
    .unwrap();
    fs::create_dir(dir.path().join("patch")).unwrap();
    fs::write(dir.path().join("patch/actor.png"), b"patch").unwrap();
    let mut vfs = Vfs::new(
        dir.path(),
        Limits {
            // 32 KiB for the auto-path table. A per-name BTree/Vec table cannot fit.
            max_cached_index_bytes: 256 * 1024,
            ..Limits::default()
        },
    )
    .unwrap();
    vfs.add_path(&units("data.xp3>voice/")).unwrap();
    vfs.add_path(&units("data.xp3>sprite/")).unwrap();
    vfs.add_path(&units("patch/")).unwrap();
    let Search::Candidates(candidates) = vfs.search(&units("nonexistent.png")).unwrap() else {
        panic!("absent resource");
    };
    assert!(
        candidates.is_empty(),
        "large directories must still prune absent names"
    );
    assert_eq!(
        vfs.plan(&units("v0511.ogg")).unwrap().read(0).unwrap(),
        b"voice"
    );
    assert_eq!(
        vfs.plan(&units("actor.png")).unwrap().read(0).unwrap(),
        b"patch"
    );
    vfs.remove_path(&units("patch/")).unwrap();
    assert_eq!(
        vfs.plan(&units("actor.png")).unwrap().read(0).unwrap(),
        b"sprite"
    );

    // The explicit path remains live even after an indexed miss.
    fs::write(dir.path().join("nonexistent.png"), b"new local file").unwrap();
    assert_eq!(
        vfs.plan(&units("nonexistent.png"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"new local file"
    );
}

#[test]
fn an_oversized_auto_path_falls_back_without_starving_later_small_paths() {
    use krkr_assets::xp3::{Compression, Writer};
    let dir = tempfile::tempdir().unwrap();
    let mut writer = Writer::new(
        Cursor::new(Vec::new()),
        Compression::None,
        Limits::default(),
    )
    .unwrap();
    for i in 0..128 {
        writer
            .add(
                &units(&format!("large/v{i:04}")),
                &mut Cursor::new(b"large"),
            )
            .unwrap();
    }
    writer
        .add(&units("small/button"), &mut Cursor::new(b"button"))
        .unwrap();
    fs::write(
        dir.path().join("data.xp3"),
        writer.finish().unwrap().into_inner(),
    )
    .unwrap();
    let mut vfs = Vfs::new(
        dir.path(),
        Limits {
            max_cached_index_bytes: 4096,
            ..Limits::default()
        },
    )
    .unwrap();
    vfs.add_path(&units("data.xp3>large/")).unwrap();
    vfs.add_path(&units("data.xp3>small/")).unwrap();
    assert_eq!(
        vfs.search_candidates(&units("absent")).unwrap(),
        vec![vfs.full_path(&units("data.xp3>large/absent")).unwrap()]
    );
    assert_eq!(
        vfs.plan(&units("v0127")).unwrap().read(0).unwrap(),
        b"large"
    );
    assert_eq!(
        vfs.plan(&units("button")).unwrap().read(0).unwrap(),
        b"button"
    );
}

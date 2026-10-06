use super::*;

#[test]
fn indexed_and_fallback_candidates_keep_priority_without_duplicates() {
    let name = name::units("button.png");
    for fallback in 0..256 {
        let mut index = Index::new(8);
        for path in 0..8 {
            index.fallback[path] = fallback & (1 << path) != 0;
            if path % 2 == 0 {
                assert!(index.insert(&name, path, 4096));
                assert!(index.insert(&name, path, 4096));
            }
        }
        index.finish();
        let expected: Vec<_> = (0..8)
            .rev()
            .filter(|&path| path % 2 == 0 || fallback & (1 << path) != 0)
            .collect();
        assert_eq!(index.candidates(&name).collect::<Vec<_>>(), expected);
    }
}

#[test]
fn long_names_fit_without_exceeding_the_candidate_budget() {
    let mut index = Index::new(2);
    let limit = 32 * 1024;
    for i in 0..1024 {
        assert!(index.insert(
            &name::units(&format!("scene_{i:05}_character_voice_take_003.ogg")),
            0,
            limit
        ));
        assert!(index.bytes <= limit);
    }
    assert!(index.insert(&name::units("button.png"), 1, limit));
    index.fallback.fill(false);
    index.finish();
    assert_eq!(
        index
            .candidates(&name::units("button.png"))
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        index
            .candidates(&name::units("scene_01023_character_voice_take_003.ogg"))
            .collect::<Vec<_>>(),
        [0]
    );
    assert_eq!(index.candidates(&name::units("absent.png")).count(), 0);
}

#[test]
fn hash_collision_only_adds_a_candidate_and_preserves_path_priority() {
    let dir = tempfile::tempdir().unwrap();
    for path in ["lower", "upper"] {
        std::fs::create_dir(dir.path().join(path)).unwrap();
    }
    std::fs::write(dir.path().join("lower/button.png"), b"button").unwrap();
    std::fs::write(dir.path().join("upper/other.png"), b"other").unwrap();
    let mut vfs = Vfs::new(dir.path(), crate::Limits::default()).unwrap();
    vfs.add_path(&name::units("lower/")).unwrap();
    vfs.add_path(&name::units("upper/")).unwrap();
    vfs.ensure_auto_paths();
    let index = vfs.auto_paths.as_mut().unwrap();
    // Force a collision without depending on the hash algorithm or seed.
    for entry in &mut index.entries {
        entry.hash = name_hash(&name::units("button.png"));
    }
    index.finish();
    assert_eq!(
        index
            .candidates(&name::units("button.png"))
            .collect::<Vec<_>>(),
        [1, 0]
    );
    assert_eq!(
        vfs.plan(&name::units("button.png"))
            .unwrap()
            .read(0)
            .unwrap(),
        b"button"
    );
}

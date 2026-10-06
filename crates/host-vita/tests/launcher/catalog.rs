use super::*;
#[test]
fn direct_children_only_and_selection_and_settings_persist() {
    let root = tempfile::tempdir().unwrap();
    for (name, file) in [
        ("Game A", "Startup.TJS"),
        ("另一个游戏", "Data.XP3"),
        ("games/nested", "startup.tjs"),
    ] {
        fs::create_dir_all(root.path().join(name)).unwrap();
        fs::write(root.path().join(name).join(file), []).unwrap();
    }
    let mut catalog = Catalog::open(root.path()).unwrap();
    assert_eq!(catalog.games.len(), 2);
    catalog.select(1);
    catalog.language = Language::Japanese;
    catalog.games[1].cursor = CursorSpeed::Slow;
    catalog.games[1].script_logs = true;
    catalog.games[0].engine_logs = true;
    catalog.games[1].show_stats = true;
    catalog.games[1].startup = "Data.XP3>scripts/boot.tjs".into();
    catalog.games[1].render_quality = RenderQuality::Balanced;
    catalog.save().unwrap();
    let mut catalog = Catalog::open(root.path()).unwrap();
    catalog.refresh().unwrap();
    assert_eq!(catalog.selected_index(), 1);
    assert_eq!(catalog.language, Language::Japanese);
    assert_eq!(catalog.games[1].cursor, CursorSpeed::Slow);
    assert!(catalog.games[1].script_logs);
    assert!(!catalog.games[1].engine_logs);
    assert!(catalog.games[0].engine_logs);
    assert!(!catalog.games[0].script_logs);
    assert!(catalog.games[1].show_stats);
    assert_eq!(catalog.games[1].startup, "Data.XP3>scripts/boot.tjs");
    assert_eq!(catalog.games[1].render_quality, RenderQuality::Balanced);
}

#[test]
fn old_settings_load_with_defaults_and_invalid_entries_cannot_escape_game() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join(CONFIG),
        "game\told\tfast\tOld game\ngame\tbad\tnormal\tBad entry\t1\t../outside.tjs\n",
    )
    .unwrap();
    let catalog = Catalog::open(root.path()).unwrap();
    let old = catalog.games.iter().find(|g| g.id == "old").unwrap();
    assert_eq!(old.cursor, CursorSpeed::Fast);
    assert!(!old.script_logs);
    assert!(catalog.games.iter().all(|g| !g.engine_logs));
    assert!(catalog.games.iter().all(|g| !g.show_stats));
    assert!(catalog.games.iter().all(|g| g.startup == "startup.tjs"));
    for invalid in [
        "",
        "/boot.tjs",
        "../boot.tjs",
        "ux0:/boot.tjs",
        "data.xp3>../boot.tjs",
    ] {
        assert!(!valid_startup(invalid), "{invalid}");
    }
}

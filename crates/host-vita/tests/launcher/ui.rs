use super::super::{CursorSpeed, Game};
use super::*;
use nivora_platform::{Point, TextStyle};

struct Metrics;
impl TextMeasurer for Metrics {
    fn measure_text(&self, text: &str, style: TextStyle, max_width: Option<f32>) -> Size {
        let natural = text.chars().count() as f32 * style.font_size * 0.7;
        let width = max_width.unwrap_or(960.0).max(style.font_size);
        Size {
            width: natural.min(width),
            height: (natural / width).ceil().max(1.0) * style.font_size * 1.3,
        }
    }
}
fn fixture(count: usize) -> (tempfile::TempDir, Catalog) {
    let root = tempfile::tempdir().unwrap();
    let mut c = Catalog::empty(root.path());
    c.animations = false;
    for i in 0..count {
        let id = format!("Game-{i:03}");
        let directory = root.path().join(&id);
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("startup.tjs"), "").unwrap();
        c.games.push(Game {
            id,
            name: format!("游戏 {i:03}"),
            directory,
            cursor: CursorSpeed::Normal,
            script_logs: false,
            engine_logs: false,
            show_stats: false,
            startup: "startup.tjs".into(),
            render_quality: Default::default(),
        });
    }
    (root, c)
}
fn layout(app: &mut LauncherUi) {
    app.layout(&Metrics).unwrap();
}
fn key(app: &mut LauncherUi, key: Key) -> Option<Effect> {
    let result = app.handle(InputEvent::KeyDown(key), &Metrics).unwrap();
    layout(app);
    result
}
fn finish_job(app: &mut LauncherUi) -> Result<(), String> {
    let start = std::time::Instant::now();
    while app.busy() {
        app.poll()?;
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "worker did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    layout(app);
    Ok(())
}

#[test]
fn virtual_library_navigates_hundreds_of_games_and_restores_selection() {
    let (_root, mut c) = fixture(300);
    c.select(130);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    for _ in 0..30 {
        key(&mut app, Key::Down);
    }
    assert_eq!(app.selected, 160);
    assert!(app.library.as_ref().unwrap().slot_count() < 12);
    app.shortcut(Action::OpenSelected).unwrap();
    layout(&mut app);
    assert_eq!(app.screen(), Screen::Game);
    key(&mut app, Key::Back);
    assert_eq!(app.screen(), Screen::Library);
    assert_eq!(
        app.navigator.active().focused_action(),
        Some(Action::OpenGame(160))
    );
    app.shortcut(Action::PageDown).unwrap();
    layout(&mut app);
    assert_eq!(
        app.navigator.active().focused_action(),
        Some(Action::OpenGame(164))
    );
}
#[test]
fn preferences_keep_selection_and_reload_from_disk() {
    let (root, mut c) = fixture(3);
    c.select(2);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.shortcut(Action::Settings).unwrap();
    layout(&mut app);
    app.apply(Action::SetLanguage(Language::Japanese)).unwrap();
    layout(&mut app);
    app.apply(Action::SetTheme(true)).unwrap();
    layout(&mut app);
    assert_eq!(app.screen(), Screen::Settings);
    key(&mut app, Key::Back);
    assert_eq!(
        app.navigator.active().focused_action(),
        Some(Action::OpenGame(2))
    );
    let c = Catalog::open(root.path()).unwrap();
    assert_eq!(c.language, Language::Japanese);
    assert!(c.light_theme);
    assert_eq!(c.selected_index(), 2);
}
#[test]
fn launch_uses_the_selected_games_settings_and_preserves_history() {
    let (root, c) = fixture(2);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    key(&mut app, Key::Down);
    app.shortcut(Action::OpenSelected).unwrap();
    layout(&mut app);
    app.game_tab = GameTab::Diagnostics;
    app.rebuild(None).unwrap();
    for action in [
        Action::Stats,
        Action::Scripts,
        Action::Diagnostics,
        Action::SetCursor(CursorSpeed::Fast),
        Action::SetQuality(RenderQuality::Performance),
    ] {
        app.apply(action).unwrap();
        layout(&mut app);
    }
    let Some(Effect::Launch(s)) = app.shortcut(Action::Play).unwrap() else {
        panic!("launch expected")
    };
    assert_eq!(s.directory, root.path().join("Game-001"));
    assert!(s.show_stats && s.script_logs && s.engine_logs);
    assert_eq!(s.cursor_speed, 800.0);
    assert_eq!(s.render_quality, RenderQuality::Performance);
    let loaded = Catalog::open(root.path()).unwrap();
    assert_eq!(loaded.selected_index(), 1);
    assert!(!loaded.games[0].show_stats);
    assert!(loaded.games[1].show_stats);
    assert_eq!(loaded.games[1].render_quality, RenderQuality::Performance);
    assert_eq!(loaded.games[0].render_quality, RenderQuality::Native);
}
#[test]
fn modal_dialogs_block_shortcuts_and_background_selection() {
    let (_root, c) = fixture(10);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.show_error("Example error").unwrap();
    layout(&mut app);
    for action in [
        Action::Play,
        Action::Refresh,
        Action::Settings,
        Action::PageDown,
    ] {
        assert!(app.shortcut(action).unwrap().is_none());
    }
    key(&mut app, Key::Down);
    assert_eq!(app.selected, 0);
    assert!(!app.busy());
    key(&mut app, Key::Accept);
    assert!(!app.navigator.is_modal_active());
    assert_eq!(app.screen(), Screen::Library);
}
#[test]
fn dropdown_selection_uses_navigation_and_restores_its_anchor() {
    let (_root, c) = fixture(1);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.shortcut(Action::Settings).unwrap();
    layout(&mut app);
    key(&mut app, Key::Accept);
    assert!(app.navigator.is_modal_active());
    key(&mut app, Key::Down);
    key(&mut app, Key::Accept);
    assert_eq!(app.catalog.language, Language::English);
    assert_eq!(
        app.navigator.active().focused_action(),
        Some(Action::Language)
    );
}
#[test]
fn quality_dropdown_restores_focus_and_saves_the_game_choice() {
    let (root, c) = fixture(1);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.shortcut(Action::OpenSelected).unwrap();
    layout(&mut app);
    app.apply(Action::Quality).unwrap();
    layout(&mut app);
    assert!(app.navigator.is_modal_active());
    key(&mut app, Key::Down);
    key(&mut app, Key::Accept);
    assert_eq!(
        app.navigator.active().focused_action(),
        Some(Action::Quality)
    );
    assert_eq!(
        Catalog::open(root.path()).unwrap().games[0].render_quality,
        RenderQuality::Balanced
    );
}
#[test]
fn exit_requires_confirmation_and_can_be_cancelled() {
    let (_root, c) = fixture(1);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    assert!(key(&mut app, Key::Back).is_none());
    assert!(app.navigator.is_modal_active());
    key(&mut app, Key::Accept);
    assert!(!app.navigator.is_modal_active());
    key(&mut app, Key::Back);
    key(&mut app, Key::Right);
    assert!(matches!(key(&mut app, Key::Accept), Some(Effect::Exit)));
}
#[test]
fn empty_library_is_usable_and_scanning_is_modal() {
    let (root, c) = fixture(0);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    assert!(app.shortcut(Action::Play).unwrap().is_none());
    app.shortcut(Action::Refresh).unwrap();
    assert!(app.busy());
    assert!(app.shortcut(Action::Play).unwrap().is_none());
    finish_job(&mut app).unwrap();
    assert!(Catalog::open(root.path()).unwrap().games.is_empty());
    app.shortcut(Action::Settings).unwrap();
    assert_eq!(app.screen(), Screen::Settings);
}
#[test]
fn browser_back_walks_directories_and_startup_selection_is_persisted() {
    let (root, c) = fixture(1);
    let game = c.games[0].directory.clone();
    std::fs::create_dir(game.join("scripts")).unwrap();
    std::fs::write(game.join("scripts/boot.tjs"), "").unwrap();
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.shortcut(Action::OpenSelected).unwrap();
    layout(&mut app);
    app.apply(Action::Browse).unwrap();
    finish_job(&mut app).unwrap();
    let i = app
        .browser
        .as_ref()
        .unwrap()
        .entries
        .iter()
        .position(|e| e.name == "scripts")
        .unwrap();
    app.apply(Action::File(i)).unwrap();
    finish_job(&mut app).unwrap();
    assert_eq!(app.browser.as_ref().unwrap().directory, "scripts/");
    key(&mut app, Key::Back);
    finish_job(&mut app).unwrap();
    assert_eq!(app.screen(), Screen::Files);
    assert_eq!(app.browser.as_ref().unwrap().directory, "");
    app.apply(Action::File(i)).unwrap();
    finish_job(&mut app).unwrap();
    app.apply(Action::File(0)).unwrap();
    finish_job(&mut app).unwrap();
    assert_eq!(app.screen(), Screen::Game);
    assert!(app.browser.is_none());
    assert_eq!(
        Catalog::open(root.path()).unwrap().games[0].startup,
        "scripts/boot.tjs"
    );
}
#[test]
fn write_failure_restores_toggle_value_and_configuration() {
    let (root, c) = fixture(1);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.shortcut(Action::OpenSelected).unwrap();
    layout(&mut app);
    app.game_tab = GameTab::Diagnostics;
    app.rebuild(None).unwrap();
    std::fs::create_dir(root.path().join("launcher.tsv")).unwrap();
    assert!(app.apply(Action::Stats).is_err());
    layout(&mut app);
    assert!(!app.catalog.games[0].show_stats);
    let id = app.navigator.active().focused().unwrap();
    assert_eq!(app.navigator.active().checked(id), Some(false));
}
#[test]
fn refresh_error_retains_previous_library_and_releases_busy_screen() {
    let (_root, c) = fixture(1);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.start_job(|| Err("read failed".into())).unwrap();
    assert_eq!(finish_job(&mut app).unwrap_err(), "read failed");
    assert!(!app.busy());
    assert_eq!(app.catalog.games.len(), 1);
    assert_eq!(app.navigator.depth(), 1);
}
#[test]
fn touch_tap_opens_game_but_drag_does_not_activate_it() {
    let (_root, c) = fixture(20);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    let id = app.library.as_ref().unwrap().widget_for(1).unwrap();
    let r = app.navigator.active().rect(id).unwrap();
    let p = Point {
        x: r.x + 50.0,
        y: r.y + r.height / 2.0,
    };
    app.handle(InputEvent::PointerDown(p), &Metrics).unwrap();
    app.handle(
        InputEvent::PointerMove(Point {
            x: p.x,
            y: p.y - 70.0,
        }),
        &Metrics,
    )
    .unwrap();
    app.handle(
        InputEvent::PointerUp(Point {
            x: p.x,
            y: p.y - 70.0,
        }),
        &Metrics,
    )
    .unwrap();
    layout(&mut app);
    assert_eq!(app.screen(), Screen::Library);
    let range = app.library.as_ref().unwrap().mounted_range();
    let id = app
        .library
        .as_ref()
        .unwrap()
        .widget_for(range.start + 2)
        .unwrap();
    let r = app.navigator.active().rect(id).unwrap();
    let p = Point {
        x: r.x + 50.0,
        y: r.y + r.height / 2.0,
    };
    app.handle(InputEvent::PointerDown(p), &Metrics).unwrap();
    assert!(matches!(
        app.handle(InputEvent::PointerUp(p), &Metrics).unwrap(),
        Some(Effect::Launch(_))
    ));
}

#[test]
fn sidebar_focus_switches_tabs_without_growing_the_page_stack() {
    let (_root, c) = fixture(2);
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.shortcut(Action::Settings).unwrap();
    layout(&mut app);
    key(&mut app, Key::Left);
    key(&mut app, Key::Down);
    assert_eq!(app.settings_tab, SettingsTab::About);
    assert_eq!(app.navigator.depth(), 2);
    key(&mut app, Key::Up);
    assert_eq!(app.settings_tab, SettingsTab::Interface);
    key(&mut app, Key::Back);
    app.shortcut(Action::OpenSelected).unwrap();
    layout(&mut app);
    key(&mut app, Key::Left);
    // Focus on the matching row may land on either sidebar tab; explicitly move to diagnostics.
    app.apply(Action::GameTab(GameTab::Diagnostics)).unwrap();
    layout(&mut app);
    key(&mut app, Key::Right);
    key(&mut app, Key::Accept);
    let game = &app.catalog.games[0];
    assert_eq!(
        usize::from(game.show_stats)
            + usize::from(game.script_logs)
            + usize::from(game.engine_logs),
        1
    );
    assert_eq!(app.navigator.depth(), 2);
}
#[test]
fn modal_exit_animation_does_not_leak_shortcuts() {
    let (_root, mut c) = fixture(1);
    c.animations = true;
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.show_error("Error").unwrap();
    layout(&mut app);
    app.advance(Duration::from_secs(1));
    key(&mut app, Key::Accept);
    assert!(!app.navigator.accepts_input());
    assert!(app.shortcut(Action::Play).unwrap().is_none());
    assert!(app.shortcut(Action::Refresh).unwrap().is_none());
    app.advance(Duration::from_secs(1));
    assert!(matches!(
        app.shortcut(Action::Play).unwrap(),
        Some(Effect::Launch(_))
    ));
}

#[test]
fn idle_library_stops_animating_and_loading_pages_have_no_images() {
    let (_root, mut c) = fixture(3);
    c.animations = true;
    let mut app = LauncherUi::new(c).unwrap();
    layout(&mut app);
    app.advance(Duration::from_secs(1));
    assert!(!app.advance(Duration::from_millis(16)));
    for launching in [false, true] {
        let mut page = pages::busy(&app.catalog, launching).unwrap();
        page.layout(VIEWPORT, &Metrics).unwrap();
        assert!(
            !page
                .frame()
                .commands
                .iter()
                .any(|c| matches!(c, nivora_platform::DrawCommand::Image { .. }))
        );
    }
}

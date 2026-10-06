//! Native-size rendering on a strict ES2 pbuffer, using the shipped font.
use super::super::{Game, render::LauncherRenderer};
use super::*;
use crate::gles_test_support as support;
use glow::HasContext;
use nivora_render_gles2::Gles2Target;

#[test]
fn launcher_pages_render_at_native_size_and_release_context_resources() {
    let context = support::Context::sized(960, 544);
    let gl = context.gl();
    let target = unsafe { Gles2Target::new(&gl) };
    let mut renderer = LauncherRenderer::new(&target, &gl).unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut catalog = Catalog::empty(root.path());
    catalog.animations = false;
    for name in [
        "G弦上的魔王",
        "魔法使之夜",
        "Yosuga no Sora HD",
        "星空下的约定",
        "长夜物语 / A very long game title that must fit in one row",
    ] {
        let directory = root.path().join(format!("game-{}", catalog.games.len()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("startup.tjs"), "").unwrap();
        catalog.games.push(Game {
            id: name.into(),
            name: name.into(),
            directory: std::path::PathBuf::from(format!(
                "ux0:/data/KRKR/game-{}",
                catalog.games.len()
            )),
            cursor: CursorSpeed::Normal,
            script_logs: false,
            engine_logs: false,
            show_stats: false,
            startup: "data.xp3>scripts/boot.tjs".into(),
            render_quality: Default::default(),
        });
    }
    let draw = |renderer: &mut LauncherRenderer<'_>, name: &str, frame: &Frame| {
        renderer.present(frame).unwrap();
        assert_eq!(frame.size, VIEWPORT);
        assert_eq!(unsafe { gl.get_error() }, glow::NO_ERROR);
        let mut rgba = vec![0; 960 * 544 * 4];
        unsafe {
            gl.read_pixels(
                0,
                0,
                960,
                544,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut rgba)),
            );
        }
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] > 180 && p[1] > 180 && p[2] > 180)
                .count()
                > 100,
            "{name}: missing text"
        );
        if let Some(output) = std::env::var_os("KRKR_LAUNCHER_PREVIEW") {
            let output = std::path::Path::new(&output);
            std::fs::create_dir_all(output).unwrap();
            let pixels: Vec<u8> = rgba
                .as_chunks::<{ 960 * 4 }>()
                .0
                .iter()
                .rev()
                .flatten()
                .copied()
                .collect();
            std::fs::write(output.join(format!("{name}.rgba")), pixels).unwrap();
        }
    };
    for (lang, language) in [
        ("zh", Language::Chinese),
        ("en", Language::English),
        ("ja", Language::Japanese),
    ] {
        for light in [false, true] {
            catalog.language = language;
            catalog.light_theme = light;
            let suffix = format!("{lang}-{}", if light { "light" } else { "dark" });
            let mut app = LauncherUi::new(catalog.clone()).unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("library-{suffix}"), &app.frame());
            app.apply(Action::OpenSelected).unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("game-{suffix}"), &app.frame());
            app.apply(Action::GameTab(GameTab::Diagnostics)).unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(
                &mut renderer,
                &format!("diagnostics-{suffix}"),
                &app.frame(),
            );
            app.apply(Action::Back).unwrap();
            app.apply(Action::Settings).unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("settings-{suffix}"), &app.frame());
            app.apply(Action::Language).unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("language-{suffix}"), &app.frame());
            app.navigator.pop();
            app.apply(Action::SettingsTab(SettingsTab::About)).unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("about-{suffix}"), &app.frame());
            app.show_error("无法打开 startup.tjs。请检查游戏资源是否已完整复制。\nCannot open the startup file.").unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("error-{suffix}"), &app.frame());
            let mut page = app.loading_page().unwrap();
            page.layout(VIEWPORT, renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("loading-{suffix}"), &page.frame());
            let mut page = pages::busy(&catalog, false).unwrap();
            page.layout(VIEWPORT, renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("working-{suffix}"), &page.frame());
            let b = Browser::new(&root.path().join("game-0")).unwrap();
            let (mut page, list) = pages::files(&catalog, &b).unwrap();
            page.layout(VIEWPORT, renderer.measurer()).unwrap();
            if let Some(mut list) = list {
                list.update(&mut page, VIEWPORT, renderer.measurer(), |i| {
                    pages::file_row(&catalog, &b, i)
                })
                .unwrap();
            }
            draw(&mut renderer, &format!("files-{suffix}"), &page.frame());
            let mut empty = catalog.clone();
            empty.games.clear();
            let mut app = LauncherUi::new(empty).unwrap();
            app.layout(renderer.measurer()).unwrap();
            draw(&mut renderer, &format!("empty-{suffix}"), &app.frame());
        }
    }
    drop(renderer);
    // A subsequent game renderer must be able to use the same current context.
    let gpu = unsafe { krkr_render_gles2::Gpu::new(context.gl(), Default::default()).unwrap() };
    let image = gpu
        .create_image(crate::window::DISPLAY, 0xff336699)
        .unwrap();
    gpu.present(
        &image,
        crate::window::DISPLAY,
        crate::window::DISPLAY.rect(),
    )
    .unwrap();
}

use super::super::{
    Catalog, Game,
    browser::{Browser, Kind},
};
use super::{Action, Screen, text::Text};
use nivora_ui::{
    AlignItems, ControllerButton as Button, ControllerLayout, Hint, HintSide,
    LayoutDimension as Dim, LayoutLengthPercentage as Length, LayoutLengthPercentageAuto as Auto,
    ScreenFrame, SpinnerSize, TextFlow, Theme, Ui, UiError, VirtualList, WidgetId, WidgetSpec,
};

pub(super) const ROW_HEIGHT: f32 = 90.0;
pub(super) fn theme(c: &Catalog) -> Theme {
    (if c.light_theme {
        Theme::light()
    } else {
        Theme::dark()
    })
    .with_animated_focus(false)
}

fn label(
    ui: &mut Ui<Action>,
    parent: WidgetId,
    t: Theme,
    text: impl Into<String>,
    size: f32,
    muted: bool,
) -> Result<WidgetId, UiError> {
    ui.insert(
        parent,
        t.label(text, size)
            .update_appearance(|a| {
                a.foreground = if muted {
                    t.colors.muted_text
                } else {
                    t.colors.text
                };
                a.text_flow = TextFlow::Wrap;
            })
            .update_layout(|s| {
                s.flex_shrink = 0.0;
                s.min_size.width = Auto::length(0.0);
            }),
    )
}
fn hint(
    frame: &mut ScreenFrame<Action>,
    button: Button,
    text: &str,
    action: Option<Action>,
    left: bool,
) -> Result<(), UiError> {
    let mut hint = Hint::controller(
        if matches!(button, Button::LeftShoulder | Button::RightShoulder) {
            ControllerLayout::Switch
        } else {
            ControllerLayout::PlayStation
        },
        button,
        text,
    )
    .side(if left {
        HintSide::Left
    } else {
        HintSide::Right
    });
    if let Some(action) = action {
        hint = hint.on_activate(action);
    }
    let id = frame.add_hint(hint)?;
    let mut a = frame.ui().appearance(id).expect("inserted hint");
    a.font_size = 18.0;
    frame.ui_mut().set_appearance(id, a)
}
fn content_style() -> nivora_ui::LayoutStyle {
    let mut s = nivora_ui::LayoutStyle {
        flex_direction: nivora_ui::FlexDirection::Column,
        flex_grow: 1.0,
        ..Default::default()
    };
    s.min_size.width = Auto::length(0.0);
    s.min_size.height = Auto::length(0.0);
    s.padding.left = Length::length(64.0);
    s.padding.right = Length::length(64.0);
    s.padding.top = Length::length(12.0);
    s.padding.bottom = Length::length(16.0);
    s.gap.width = Length::length(28.0);
    s.gap.height = Length::length(12.0);
    s
}
fn frame(c: &Catalog, title: &str, root: bool) -> Result<ScreenFrame<Action>, UiError> {
    let t = theme(c);
    let l = Text(c.language);
    let mut frame = ScreenFrame::new(t, title)?;
    frame.set_separator_inset(48.0)?;
    let status = frame.header_status();
    label(
        frame.ui_mut(),
        status,
        t,
        concat!("v", env!("CARGO_PKG_VERSION")),
        17.0,
        true,
    )?;
    if root {
        hint(
            &mut frame,
            Button::Select,
            l.settings(),
            Some(Action::Settings),
            true,
        )?;
        if !c.games.is_empty() {
            hint(
                &mut frame,
                Button::LeftShoulder,
                "",
                Some(Action::PageUp),
                true,
            )?;
            hint(
                &mut frame,
                Button::RightShoulder,
                l.pick("翻页", "Page", "ページ"),
                Some(Action::PageDown),
                true,
            )?;
        }
        hint(
            &mut frame,
            Button::Start,
            l.refresh(),
            Some(Action::Refresh),
            true,
        )?;
        if !c.games.is_empty() {
            hint(
                &mut frame,
                Button::FaceNorth,
                l.details(),
                Some(Action::OpenSelected),
                false,
            )?;
            hint(
                &mut frame,
                Button::FaceEast,
                l.play(),
                Some(Action::Play),
                false,
            )?;
        }
    } else {
        hint(
            &mut frame,
            Button::FaceSouth,
            l.back(),
            Some(Action::Back),
            false,
        )?;
        hint(&mut frame, Button::FaceEast, l.select(), None, false)?;
    }
    let content = frame.content();
    frame.ui_mut().set_style(content, content_style())?;
    Ok(frame)
}
fn column(ui: &mut Ui<Action>, parent: WidgetId, width: Option<f32>) -> Result<WidgetId, UiError> {
    ui.insert(
        parent,
        WidgetSpec::column().update_layout(|s| {
            s.gap.height = Length::length(14.0);
            s.min_size.width = Auto::length(0.0);
            s.min_size.height = Auto::length(0.0);
            if let Some(width) = width {
                s.size.width = Dim::length(width);
                s.flex_shrink = 0.0;
            } else {
                s.flex_grow = 1.0;
            }
        }),
    )
}
fn horizontal(frame: &mut ScreenFrame<Action>) -> Result<WidgetId, UiError> {
    let content = frame.content();
    let mut s = content_style();
    s.flex_direction = nivora_ui::FlexDirection::Row;
    frame.ui_mut().set_style(content, s)?;
    Ok(content)
}

fn control(
    ui: &mut Ui<Action>,
    parent: WidgetId,
    spec: WidgetSpec<Action>,
    action: &Action,
    focus: Option<&Action>,
) -> Result<WidgetId, UiError> {
    let id = ui.insert(
        parent,
        spec.update_layout(|s| {
            s.size.height = Dim::length(56.0);
            s.flex_shrink = 0.0;
        })
        .update_appearance(|a| a.font_size = 22.0),
    )?;
    if focus == Some(action) {
        ui.focus(id)?;
    }
    Ok(id)
}

pub(super) fn library(c: &Catalog) -> Result<(Ui<Action>, Option<VirtualList>), UiError> {
    let t = theme(c);
    let l = Text(c.language);
    let mut f = frame(c, "KRKR", true)?;
    let trailing = f.title_trailing();
    label(
        f.ui_mut(),
        trailing,
        t,
        format!("{} {}", c.games.len(), l.pick("部游戏", "games", "作品")),
        17.0,
        true,
    )?;
    let content = f.content();
    let ui = f.ui_mut();
    let list = if c.games.is_empty() {
        let center = ui.insert(
            content,
            WidgetSpec::column().update_layout(|s| {
                s.flex_grow = 1.0;
                s.align_items = Some(AlignItems::CENTER);
                s.justify_content = Some(nivora_ui::JustifyContent::CENTER);
                s.gap.height = Length::length(20.0);
            }),
        )?;
        label(
            ui,
            center,
            t,
            l.pick("未找到游戏", "No games found", "ゲームが見つかりません"),
            28.0,
            false,
        )?;
        label(ui, center, t, super::super::ROOT, 20.0, true)?;
        let id = control(
            ui,
            center,
            t.primary_button(l.refresh(), Action::Refresh)
                .update_layout(|s| s.size.width = Dim::length(240.0)),
            &Action::Refresh,
            None,
        )?;
        ui.focus(id)?;
        None
    } else {
        Some(VirtualList::new(ui, content, c.games.len(), ROW_HEIGHT)?.with_overscan(1))
    };
    Ok((f.into_ui(), list))
}

pub(super) fn game_row(c: &Catalog, index: usize) -> WidgetSpec<Action> {
    theme(c)
        .list_item(&c.games[index].name, None, Action::OpenGame(index))
        .update_appearance(|a| {
            a.font_size = 28.0;
            a.text_flow = TextFlow::SingleLine;
        })
}
fn sidebar(
    ui: &mut Ui<Action>,
    parent: WidgetId,
    t: Theme,
    tabs: &[(&str, Action, bool)],
    focus: Option<&Action>,
) -> Result<WidgetId, UiError> {
    let column = column(ui, parent, Some(190.0))?;
    for (text, action, selected) in tabs {
        let id = ui.insert(column, t.sidebar_item(*text, *selected, action.clone()))?;
        if focus == Some(action) {
            ui.focus(id)?;
        }
    }
    ui.insert(
        parent,
        WidgetSpec::column()
            .update_layout(|s| {
                s.size.width = Dim::length(1.0);
                s.flex_shrink = 0.0;
            })
            .update_appearance(|a| a.background = Some(t.colors.sidebar_separator)),
    )?;
    Ok(column)
}
fn body(ui: &mut Ui<Action>, parent: WidgetId) -> Result<WidgetId, UiError> {
    let scroll = ui.insert(
        parent,
        WidgetSpec::scroll().update_layout(|s| {
            s.flex_grow = 1.0;
            s.flex_basis = Dim::length(0.0);
            s.min_size.width = Auto::length(0.0);
        }),
    )?;
    column(ui, scroll, None)
}
pub(super) fn game(
    c: &Catalog,
    g: &Game,
    tab: super::GameTab,
    focus: Option<&Action>,
) -> Result<Ui<Action>, UiError> {
    use super::GameTab;
    let t = theme(c);
    let l = Text(c.language);
    let mut f = frame(c, l.details(), false)?;
    let content = horizontal(&mut f)?;
    let ui = f.ui_mut();
    let left = sidebar(
        ui,
        content,
        t,
        &[
            (
                l.pick("常规", "General", "一般"),
                Action::GameTab(GameTab::Launch),
                tab == GameTab::Launch,
            ),
            (
                l.pick("诊断", "Diagnostics", "診断"),
                Action::GameTab(GameTab::Diagnostics),
                tab == GameTab::Diagnostics,
            ),
        ],
        focus,
    )?;
    control(
        ui,
        left,
        t.primary_button(l.play(), Action::Play),
        &Action::Play,
        focus,
    )?;
    let body = body(ui, content)?;
    label(ui, body, t, &g.name, 28.0, false)?;
    match tab {
        GameTab::Launch => {
            label(ui, body, t, g.directory.to_string_lossy(), 18.0, true)?;
            let first = control(
                ui,
                body,
                t.selector_cell(l.startup(), "…", Action::Browse),
                &Action::Browse,
                focus,
            )?;
            label(ui, body, t, &g.startup, 18.0, true)?;
            control(
                ui,
                body,
                t.selector_cell(l.cursor(), l.speed(g.cursor), Action::Cursor),
                &Action::Cursor,
                focus,
            )?;
            control(
                ui,
                body,
                t.selector_cell(
                    l.pick("特效画质", "Effect quality", "エフェクト画質"),
                    l.quality(g.render_quality),
                    Action::Quality,
                ),
                &Action::Quality,
                focus,
            )?;
            if focus.is_none() {
                ui.focus(first)?;
            }
        }
        GameTab::Diagnostics => {
            control(
                ui,
                body,
                t.toggle(l.stats(), g.show_stats, Action::Stats),
                &Action::Stats,
                focus,
            )?;
            control(
                ui,
                body,
                t.toggle(l.scripts(), g.script_logs, Action::Scripts),
                &Action::Scripts,
                focus,
            )?;
            control(
                ui,
                body,
                t.toggle(l.diagnostics(), g.engine_logs, Action::Diagnostics),
                &Action::Diagnostics,
                focus,
            )?;
        }
    }
    Ok(f.into_ui())
}
pub(super) fn settings(
    c: &Catalog,
    tab: super::SettingsTab,
    focus: Option<&Action>,
) -> Result<Ui<Action>, UiError> {
    use super::SettingsTab;
    let t = theme(c);
    let l = Text(c.language);
    let mut f = frame(c, l.settings(), false)?;
    let content = horizontal(&mut f)?;
    let ui = f.ui_mut();
    sidebar(
        ui,
        content,
        t,
        &[
            (
                l.pick("界面", "Interface", "表示"),
                Action::SettingsTab(SettingsTab::Interface),
                tab == SettingsTab::Interface,
            ),
            (
                l.pick("关于", "About", "情報"),
                Action::SettingsTab(SettingsTab::About),
                tab == SettingsTab::About,
            ),
        ],
        focus,
    )?;
    let body = body(ui, content)?;
    match tab {
        SettingsTab::Interface => {
            let language = match c.language {
                super::super::Language::Chinese => "简体中文",
                super::super::Language::English => "English",
                super::super::Language::Japanese => "日本語",
            };
            let first = control(
                ui,
                body,
                t.selector_cell(l.language(), language, Action::Language),
                &Action::Language,
                focus,
            )?;
            control(
                ui,
                body,
                t.selector_cell(l.theme(), l.theme_name(c.light_theme), Action::Theme),
                &Action::Theme,
                focus,
            )?;
            control(
                ui,
                body,
                t.toggle(l.animations(), c.animations, Action::Animations),
                &Action::Animations,
                focus,
            )?;
            if focus.is_none() {
                ui.focus(first)?;
            }
        }
        SettingsTab::About => {
            label(ui, body, t, "KRKR", 32.0, false)?;
            label(
                ui,
                body,
                t,
                concat!("v", env!("CARGO_PKG_VERSION")),
                18.0,
                true,
            )?;
            label(
                ui,
                body,
                t,
                l.pick(
                    "使用 Rust 实现的 Kirikiri / TJS2 引擎",
                    "A Kirikiri / TJS2 engine written in Rust",
                    "Rust 製 Kirikiri / TJS2 エンジン",
                ),
                22.0,
                false,
            )?;
            label(
                ui,
                body,
                t,
                l.pick("作者：今何求", "Author: 今何求", "作者：今何求"),
                20.0,
                true,
            )?;
            label(
                ui,
                body,
                t,
                "Nivora · Qiushui Shotai · PromptFont",
                17.0,
                true,
            )?;
        }
    }
    Ok(f.into_ui())
}

pub(super) fn files(
    c: &Catalog,
    b: &Browser,
) -> Result<(Ui<Action>, Option<VirtualList>), UiError> {
    let t = theme(c);
    let l = Text(c.language);
    let mut f = frame(c, l.startup(), false)?;
    let content = f.content();
    let ui = f.ui_mut();
    label(
        ui,
        content,
        t,
        if b.directory.is_empty() {
            "/"
        } else {
            &b.directory
        },
        18.0,
        true,
    )?;
    let list = if b.entries.is_empty() {
        label(
            ui,
            content,
            t,
            l.pick("此目录为空", "This folder is empty", "空のフォルダーです"),
            24.0,
            false,
        )?;
        None
    } else {
        Some(VirtualList::new(ui, content, b.entries.len(), 58.0)?.with_overscan(1))
    };
    control(
        ui,
        content,
        t.button(l.reset(), Action::DefaultStartup),
        &Action::DefaultStartup,
        None,
    )?;
    Ok((f.into_ui(), list))
}
pub(super) fn file_row(c: &Catalog, b: &Browser, index: usize) -> WidgetSpec<Action> {
    let l = Text(c.language);
    let entry = &b.entries[index];
    let detail = match entry.kind {
        Kind::Folder => l.folder(),
        Kind::Archive => "XP3",
        Kind::File => l.file(),
    };
    theme(c)
        .selector_cell(&entry.name, detail, Action::File(index))
        .update_appearance(|a| {
            a.font_size = 22.0;
            a.text_flow = TextFlow::SingleLine;
        })
}
pub(super) fn busy(c: &Catalog, launching: bool) -> Result<Ui<Action>, UiError> {
    let t = theme(c);
    let l = Text(c.language);
    let mut ui = t.page()?;
    let center = ui.insert(
        ui.root(),
        WidgetSpec::column().update_layout(|s| {
            s.size.width = Dim::percent(1.0);
            s.size.height = Dim::percent(1.0);
            s.align_items = Some(AlignItems::CENTER);
            s.justify_content = Some(nivora_ui::JustifyContent::CENTER);
            s.gap.height = Length::length(24.0);
        }),
    )?;
    if !launching {
        ui.insert(center, t.spinner(SpinnerSize::Large))?;
    }
    label(
        &mut ui,
        center,
        t,
        if launching { l.loading() } else { l.working() },
        25.0,
        false,
    )?;
    Ok(ui)
}
pub(super) fn screen(
    c: &Catalog,
    screen: Screen,
    game_index: usize,
    game_tab: super::GameTab,
    settings_tab: super::SettingsTab,
    focus: Option<&Action>,
) -> Result<Ui<Action>, UiError> {
    match screen {
        Screen::Game => game(c, &c.games[game_index], game_tab, focus),
        Screen::Settings => settings(c, settings_tab, focus),
        _ => unreachable!("list pages have their own builder"),
    }
}

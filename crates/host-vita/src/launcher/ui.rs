//! Launcher application state, navigation and background storage operations.
mod pages;
mod text;
use super::{Catalog, CursorSpeed, Language, RenderQuality, Selection, browser::Browser};
use nivora_platform::{Frame, InputEvent, Key, Size, TextMeasurer};
use nivora_ui::{
    BackBehavior, Dialog, Dropdown, NavigationEvent, Navigator, PageOptions, Ui, UiError,
    VirtualList,
};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, TryRecvError},
    time::Duration,
};
use text::Text;

pub(super) const VIEWPORT: Size = Size {
    width: 960.0,
    height: 544.0,
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum GameTab {
    #[default]
    Launch,
    Diagnostics,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SettingsTab {
    #[default]
    Interface,
    About,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Action {
    OpenGame(usize),
    OpenSelected,
    Play,
    Settings,
    GameTab(GameTab),
    SettingsTab(SettingsTab),
    Back,
    Exit,
    Close,
    Refresh,
    PageUp,
    PageDown,
    Language,
    SetLanguage(Language),
    Theme,
    SetTheme(bool),
    Animations,
    Browse,
    File(usize),
    DefaultStartup,
    Cursor,
    SetCursor(CursorSpeed),
    Quality,
    SetQuality(RenderQuality),
    Stats,
    Scripts,
    Diagnostics,
}
pub(super) enum Effect {
    Launch(Selection),
    Exit,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Library,
    Game,
    Settings,
    Files,
}
enum Completed {
    Catalog(Catalog),
    Browser(Browser, Option<String>),
    Parent(Browser, bool),
}
type Job = Receiver<Result<Completed, String>>;

pub(super) struct LauncherUi {
    catalog: Catalog,
    navigator: Navigator<Action>,
    screens: Vec<Screen>,
    selected: usize,
    game_tab: GameTab,
    settings_tab: SettingsTab,
    library: Option<VirtualList>,
    files: Option<VirtualList>,
    browser: Option<Browser>,
    restore_selection: bool,
    job: Option<Job>,
}
impl LauncherUi {
    pub fn new(catalog: Catalog) -> Result<Self, UiError> {
        let selected = catalog.selected_index();
        let (ui, library) = pages::library(&catalog)?;
        let mut navigator = Navigator::new(ui);
        navigator.set_animations_enabled(catalog.animations);
        Ok(Self {
            catalog,
            navigator,
            screens: vec![Screen::Library],
            selected,
            game_tab: GameTab::default(),
            settings_tab: SettingsTab::default(),
            library,
            files: None,
            browser: None,
            restore_selection: true,
            job: None,
        })
    }
    fn screen(&self) -> Screen {
        *self.screens.last().expect("root exists")
    }
    pub fn frame(&self) -> Frame {
        self.navigator.frame()
    }
    pub fn cancel_pointer(&mut self) {
        self.navigator.active_mut().cancel_pointer();
    }
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
    pub fn advance(&mut self, elapsed: Duration) -> bool {
        self.navigator.advance(elapsed)
    }
    pub fn loading_page(&self) -> Result<Ui<Action>, UiError> {
        pages::busy(&self.catalog, true)
    }
    pub fn load(&mut self, root: PathBuf) -> Result<(), String> {
        self.start_job(move || Catalog::open(&root).map(Completed::Catalog))
    }
    fn start_job(
        &mut self,
        run: impl FnOnce() -> Result<Completed, String> + Send + 'static,
    ) -> Result<(), String> {
        let page = pages::busy(&self.catalog, false).map_err(ui_error)?;
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("krkr-launcher-io".into())
            .spawn(move || {
                let _ = send.send(run());
            })
            .map_err(|e| e.to_string())?;
        self.cancel_pointer();
        self.navigator.push_with_options(
            page,
            PageOptions {
                back: BackBehavior::Ignore,
                ..Default::default()
            },
        );
        self.job = Some(receive);
        Ok(())
    }
    pub fn poll(&mut self) -> Result<bool, String> {
        let Some(job) = &self.job else {
            return Ok(false);
        };
        let result = match job.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return Ok(false),
            Err(TryRecvError::Disconnected) => Err("Launcher storage worker stopped".into()),
        };
        self.job = None;
        self.navigator.pop();
        match result? {
            Completed::Catalog(catalog) => {
                self.catalog = catalog;
                self.selected = self.catalog.selected_index();
                self.screens = vec![Screen::Library];
                self.rebuild(None).map_err(ui_error)?;
            }
            Completed::Browser(browser, startup) => {
                self.browser = Some(browser);
                if let Some(startup) = startup {
                    self.set_startup(startup)?;
                } else if self.screen() == Screen::Files {
                    self.replace_files().map_err(ui_error)?;
                } else {
                    let (page, list) = pages::files(
                        &self.catalog,
                        self.browser.as_ref().expect("browser loaded"),
                    )
                    .map_err(ui_error)?;
                    self.files = list;
                    self.navigator.push_screen_animated(page);
                    self.screens.push(Screen::Files);
                    self.restore_selection = true;
                }
            }
            Completed::Parent(browser, inside) => {
                self.browser = Some(browser);
                if inside {
                    self.replace_files().map_err(ui_error)?;
                } else {
                    self.pop_screen();
                }
            }
        }
        Ok(true)
    }
    pub fn layout(&mut self, measurer: &impl TextMeasurer) -> Result<(), UiError> {
        self.navigator.layout(VIEWPORT, measurer)?;
        if !self.busy() && !self.navigator.is_modal_active() {
            let files = self.screen() == Screen::Files;
            let list = match self.screen() {
                Screen::Library => &mut self.library,
                Screen::Files => &mut self.files,
                _ => return Ok(()),
            };
            if let Some(list) = list {
                let c = &self.catalog;
                let b = self.browser.as_ref();
                let row = |i| {
                    if files {
                        pages::file_row(c, b.expect("file page has browser"), i)
                    } else {
                        pages::game_row(c, i)
                    }
                };
                let ui = self.navigator.active_mut();
                list.update(ui, VIEWPORT, measurer, row)?;
                if self.restore_selection {
                    let index = if files {
                        b.expect("browser").selected
                    } else {
                        self.selected
                    };
                    list.scroll_to_index(ui, index, VIEWPORT, measurer, row)?;
                    if let Some(id) = list.widget_for(index) {
                        let visible = ui.focus_visible();
                        ui.focus(id)?;
                        ui.set_focus_visible(visible);
                    }
                }
            }
            self.restore_selection = false;
        }
        self.navigator.layout(VIEWPORT, measurer)
    }
    fn track_selection(&mut self) {
        if self.navigator.is_modal_active() || self.busy() {
            return;
        }
        match self.navigator.active().focused_action() {
            Some(Action::OpenGame(index)) if self.screen() == Screen::Library => {
                self.selected = index
            }
            Some(Action::File(index)) if self.screen() == Screen::Files => {
                if let Some(b) = &mut self.browser {
                    b.selected = index;
                }
            }
            _ => {}
        }
    }
    pub fn handle(
        &mut self,
        event: InputEvent,
        measurer: &impl TextMeasurer,
    ) -> Result<Option<Effect>, String> {
        if self.busy() || !self.navigator.accepts_input() {
            return Ok(None);
        }
        if !self.navigator.is_modal_active()
            && event == InputEvent::KeyDown(Key::Back)
            && self.screen() == Screen::Files
        {
            return self.apply(Action::Back);
        }
        if !self.navigator.is_modal_active()
            && let InputEvent::KeyDown(key @ (Key::Up | Key::Down)) = event
        {
            let files = self.screen() == Screen::Files;
            let list = match self.screen() {
                Screen::Library => self.library.as_mut(),
                Screen::Files => self.files.as_mut(),
                _ => None,
            };
            if let Some(list) = list {
                let c = &self.catalog;
                let b = self.browser.as_ref();
                if list
                    .move_focus(self.navigator.active_mut(), key, VIEWPORT, measurer, |i| {
                        if files {
                            pages::file_row(c, b.expect("browser"), i)
                        } else {
                            pages::game_row(c, i)
                        }
                    })
                    .map_err(ui_error)?
                {
                    self.track_selection();
                    return Ok(None);
                }
            }
        }
        let result = self.navigator.handle(event);
        self.sync_screens();
        self.track_selection();
        if !self.navigator.is_modal_active() {
            match self.navigator.active().focused_action() {
                Some(Action::GameTab(tab)) if tab != self.game_tab => {
                    self.switch_tab(Action::GameTab(tab)).map_err(ui_error)?
                }
                Some(Action::SettingsTab(tab)) if tab != self.settings_tab => self
                    .switch_tab(Action::SettingsTab(tab))
                    .map_err(ui_error)?,
                _ => {}
            }
        }
        match result {
            Some(NavigationEvent::Action(action)) => self.apply(action),
            Some(NavigationEvent::BackAtRoot) => self.apply(Action::Back),
            _ => Ok(None),
        }
    }
    pub fn shortcut(&mut self, action: Action) -> Result<Option<Effect>, String> {
        if self.busy() || self.navigator.is_modal_active() || !self.navigator.accepts_input() {
            return Ok(None);
        }
        let allowed = match action {
            Action::Play => matches!(self.screen(), Screen::Library | Screen::Game),
            Action::Settings
            | Action::OpenSelected
            | Action::Refresh
            | Action::PageUp
            | Action::PageDown => self.screen() == Screen::Library,
            _ => false,
        };
        if !allowed {
            return Ok(None);
        }
        self.track_selection();
        self.apply(action)
    }
    fn sync_screens(&mut self) {
        self.screens.truncate(self.navigator.depth());
        if !self.screens.contains(&Screen::Files) {
            self.files = None;
            self.browser = None;
        }
    }
    fn pop_screen(&mut self) {
        self.navigator.dismiss();
        self.sync_screens();
    }
    fn open(&mut self, screen: Screen) -> Result<(), UiError> {
        if screen == Screen::Game {
            self.game_tab = GameTab::Launch;
        }
        let page = pages::screen(
            &self.catalog,
            screen,
            self.selected,
            self.game_tab,
            self.settings_tab,
            None,
        )?;
        self.navigator.push_screen_animated(page);
        self.screens.push(screen);
        Ok(())
    }
    fn switch_tab(&mut self, action: Action) -> Result<(), UiError> {
        match action {
            Action::GameTab(tab) => self.game_tab = tab,
            Action::SettingsTab(tab) => self.settings_tab = tab,
            _ => unreachable!(),
        }
        let page = pages::screen(
            &self.catalog,
            self.screen(),
            self.selected,
            self.game_tab,
            self.settings_tab,
            Some(&action),
        )?;
        self.navigator.replace(page);
        Ok(())
    }
    fn replace_files(&mut self) -> Result<(), UiError> {
        let (page, list) = pages::files(&self.catalog, self.browser.as_ref().expect("browser"))?;
        self.navigator.replace(page);
        self.files = list;
        self.restore_selection = true;
        Ok(())
    }
    fn rebuild(&mut self, focus: Option<&Action>) -> Result<(), UiError> {
        let visible = self.navigator.active().focus_visible();
        let (page, list) = pages::library(&self.catalog)?;
        let mut navigator = Navigator::new(page);
        navigator.set_animations_enabled(self.catalog.animations);
        for screen in self.screens.iter().copied().skip(1) {
            navigator.push_screen(pages::screen(
                &self.catalog,
                screen,
                self.selected,
                self.game_tab,
                self.settings_tab,
                focus,
            )?);
        }
        navigator.active_mut().set_focus_visible(visible);
        self.navigator = navigator;
        self.library = list;
        self.restore_selection = true;
        Ok(())
    }
    fn preferences(&mut self, next: Catalog, focus: Action) -> Result<(), String> {
        // Keep the previous configuration and restore toggle state on write failure.
        let result = next.save();
        if result.is_ok() {
            self.catalog = next;
        }
        self.rebuild(Some(&focus)).map_err(ui_error)?;
        result
    }
    fn updated_catalog(&self) -> Catalog {
        let mut catalog = self.catalog.clone();
        catalog.select(self.selected);
        catalog
    }
    fn set_startup(&mut self, startup: String) -> Result<(), String> {
        if !super::valid_startup(&startup) {
            return Err("Invalid startup file name".into());
        }
        let mut next = self.updated_catalog();
        next.games[self.selected].startup = startup;
        next.save()?;
        self.catalog = next;
        if self.screen() == Screen::Files {
            self.navigator.pop();
            self.sync_screens();
        }
        self.rebuild(Some(&Action::Browse)).map_err(ui_error)
    }
    fn dropdown(&mut self, menu: Dropdown<Action>) -> Result<(), String> {
        let theme = pages::theme(&self.catalog);
        if let Some(id) = self.navigator.active().focused() {
            self.navigator.push_dropdown_at(theme, id, menu)
        } else {
            self.navigator.push_dropdown(theme, menu)
        }
        .map_err(ui_error)
    }
    pub fn show_error(&mut self, message: &str) -> Result<(), UiError> {
        let l = Text(self.catalog.language);
        self.navigator.push_dialog(
            pages::theme(&self.catalog),
            Dialog::new(message).button(l.close(), Action::Close),
        )
    }
    fn apply(&mut self, action: Action) -> Result<Option<Effect>, String> {
        let l = Text(self.catalog.language);
        match action {
            Action::OpenGame(i) => {
                if i < self.catalog.games.len() {
                    self.selected = i;
                    return self.apply(Action::Play);
                }
            }
            Action::OpenSelected => {
                if !self.catalog.games.is_empty() {
                    self.open(Screen::Game).map_err(ui_error)?;
                }
            }
            Action::Play => {
                if let Some(game) = self.catalog.games.get(self.selected) {
                    let selected = Selection {
                        directory: game.directory.clone(),
                        cursor_speed: game.cursor.pixels_per_second(),
                        script_logs: game.script_logs,
                        engine_logs: game.engine_logs,
                        show_stats: game.show_stats,
                        startup: game.startup.clone(),
                        language: self.catalog.language,
                        render_quality: game.render_quality,
                    };
                    self.updated_catalog().save()?;
                    return Ok(Some(Effect::Launch(selected)));
                }
            }
            Action::Settings => self.open(Screen::Settings).map_err(ui_error)?,
            Action::GameTab(_) | Action::SettingsTab(_) => {
                self.switch_tab(action).map_err(ui_error)?
            }
            Action::Refresh => {
                let mut next = self.updated_catalog();
                self.start_job(move || {
                    next.refresh()?;
                    next.save()?;
                    Ok(Completed::Catalog(next))
                })?;
            }
            Action::PageUp | Action::PageDown => {
                if !self.catalog.games.is_empty() {
                    self.selected = if action == Action::PageUp {
                        self.selected.saturating_sub(4)
                    } else {
                        (self.selected + 4).min(self.catalog.games.len() - 1)
                    };
                    self.restore_selection = true;
                }
            }
            Action::Language => self.dropdown(
                Dropdown::new()
                    .option("简体中文", Action::SetLanguage(Language::Chinese))
                    .option("English", Action::SetLanguage(Language::English))
                    .option("日本語", Action::SetLanguage(Language::Japanese))
                    .selected(match self.catalog.language {
                        Language::Chinese => 0,
                        Language::English => 1,
                        Language::Japanese => 2,
                    }),
            )?,
            Action::Theme => self.dropdown(
                Dropdown::new()
                    .option(l.theme_name(false), Action::SetTheme(false))
                    .option(l.theme_name(true), Action::SetTheme(true))
                    .selected(usize::from(self.catalog.light_theme)),
            )?,
            Action::SetLanguage(language) => {
                let mut next = self.updated_catalog();
                next.language = language;
                self.preferences(next, Action::Language)?;
            }
            Action::SetTheme(light) => {
                let mut next = self.updated_catalog();
                next.light_theme = light;
                self.preferences(next, Action::Theme)?;
            }
            Action::Animations => {
                let mut next = self.updated_catalog();
                next.animations = !next.animations;
                self.preferences(next, Action::Animations)?;
            }
            Action::Cursor => self.dropdown(
                Dropdown::new()
                    .option(
                        l.speed(CursorSpeed::Slow),
                        Action::SetCursor(CursorSpeed::Slow),
                    )
                    .option(
                        l.speed(CursorSpeed::Normal),
                        Action::SetCursor(CursorSpeed::Normal),
                    )
                    .option(
                        l.speed(CursorSpeed::Fast),
                        Action::SetCursor(CursorSpeed::Fast),
                    )
                    .selected(match self.catalog.games[self.selected].cursor {
                        CursorSpeed::Slow => 0,
                        CursorSpeed::Normal => 1,
                        CursorSpeed::Fast => 2,
                    }),
            )?,
            Action::SetCursor(speed) => {
                let mut next = self.updated_catalog();
                next.games[self.selected].cursor = speed;
                self.preferences(next, Action::Cursor)?;
            }
            Action::Stats | Action::Scripts | Action::Diagnostics => {
                let mut next = self.updated_catalog();
                let g = &mut next.games[self.selected];
                let value = match action {
                    Action::Stats => &mut g.show_stats,
                    Action::Scripts => &mut g.script_logs,
                    _ => &mut g.engine_logs,
                };
                *value = !*value;
                self.preferences(next, action)?;
            }
            Action::Quality => self.dropdown(
                Dropdown::new()
                    .option(
                        l.quality(RenderQuality::Native),
                        Action::SetQuality(RenderQuality::Native),
                    )
                    .option(
                        l.quality(RenderQuality::Balanced),
                        Action::SetQuality(RenderQuality::Balanced),
                    )
                    .option(
                        l.quality(RenderQuality::Performance),
                        Action::SetQuality(RenderQuality::Performance),
                    )
                    .selected(match self.catalog.games[self.selected].render_quality {
                        RenderQuality::Native => 0,
                        RenderQuality::Balanced => 1,
                        RenderQuality::Performance => 2,
                    }),
            )?,
            Action::SetQuality(quality) => {
                let mut next = self.updated_catalog();
                next.games[self.selected].render_quality = quality;
                self.preferences(next, Action::Quality)?;
            }
            Action::Browse => {
                let root = self.catalog.games[self.selected].directory.clone();
                self.start_job(move || Browser::new(&root).map(|b| Completed::Browser(b, None)))?;
            }
            Action::File(index) => {
                let mut b = self.browser.as_ref().expect("file page").clone();
                if index < b.entries.len() {
                    b.selected = index;
                    self.start_job(move || {
                        let result = b.activate()?;
                        Ok(Completed::Browser(b, result))
                    })?;
                }
            }
            Action::DefaultStartup => self.set_startup("startup.tjs".into())?,
            Action::Back => {
                if self.screen() == Screen::Files {
                    let mut b = self.browser.as_ref().expect("browser").clone();
                    self.start_job(move || {
                        let inside = b.back()?;
                        Ok(Completed::Parent(b, inside))
                    })?;
                } else if self.screens.len() > 1 {
                    self.pop_screen();
                } else {
                    self.navigator
                        .push_dialog(
                            pages::theme(&self.catalog),
                            Dialog::new(l.pick(
                                "退出 KRKR？",
                                "Exit KRKR?",
                                "KRKR を終了しますか？",
                            ))
                            .button(l.cancel(), Action::Close)
                            .button(l.exit(), Action::Exit),
                        )
                        .map_err(ui_error)?;
                }
            }
            Action::Exit => {
                self.updated_catalog().save()?;
                return Ok(Some(Effect::Exit));
            }
            Action::Close => {}
        }
        Ok(None)
    }
}
fn ui_error(error: UiError) -> String {
    error.to_string()
}

#[cfg(all(test, not(target_os = "vita")))]
#[path = "../../tests/launcher/ui.rs"]
mod tests;

#[cfg(all(
    test,
    any(target_os = "linux", all(windows, feature = "windows-gles-tests"))
))]
#[allow(unsafe_code)]
#[path = "../../tests/launcher/preview.rs"]
mod preview;

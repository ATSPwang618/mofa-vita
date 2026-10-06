//! Persistent game catalog and launcher preferences.
use std::{
    collections::HashMap,
    fs,
    path::{Component, Path, PathBuf},
};
const CONFIG: &str = "launcher.tsv";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    English,
    #[default]
    Chinese,
    Japanese,
}
impl Language {
    pub fn next(self) -> Self {
        match self {
            Self::English => Self::Chinese,
            Self::Chinese => Self::Japanese,
            Self::Japanese => Self::English,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Chinese => "zh",
            Self::Japanese => "ja",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorSpeed {
    Slow,
    #[default]
    Normal,
    Fast,
}
impl CursorSpeed {
    pub fn next(self, forward: bool) -> Self {
        match (self, forward) {
            (Self::Slow, true) | (Self::Fast, false) => Self::Normal,
            (Self::Normal, true) | (Self::Slow, false) => Self::Fast,
            _ => Self::Slow,
        }
    }
    pub fn pixels_per_second(self) -> f32 {
        match self {
            Self::Slow => 320.,
            Self::Normal => 520.,
            Self::Fast => 800.,
        }
    }
    fn code(self) -> &'static str {
        match self {
            Self::Slow => "slow",
            Self::Normal => "normal",
            Self::Fast => "fast",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RenderQuality {
    #[default]
    Native,
    Balanced,
    Performance,
}
impl RenderQuality {
    pub fn canvas_size(self) -> krkr_protocol::graphics::Size {
        let (width, height) = match self {
            Self::Native => (960, 544),
            Self::Balanced => (720, 408),
            Self::Performance => (480, 272),
        };
        krkr_protocol::graphics::Size { width, height }
    }
    pub fn effect_interval_ms(self) -> Option<u32> {
        (self != Self::Native).then_some(33)
    }
    fn code(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }
}
#[derive(Clone)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub directory: PathBuf,
    pub cursor: CursorSpeed,
    pub script_logs: bool,
    pub engine_logs: bool,
    pub show_stats: bool,
    pub startup: String,
    pub render_quality: RenderQuality,
}
#[derive(Clone)]
pub struct Catalog {
    root: PathBuf,
    pub games: Vec<Game>,
    pub language: Language,
    pub light_theme: bool,
    pub animations: bool,
    selected: Option<String>,
}
impl Catalog {
    pub fn empty(root: &Path) -> Self {
        Self {
            root: root.into(),
            games: Vec::new(),
            language: Language::default(),
            light_theme: false,
            animations: true,
            selected: None,
        }
    }
    pub fn open(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let mut catalog = Self::empty(root);
        match fs::read_to_string(root.join(CONFIG)) {
            Ok(text) => {
                for line in text.lines() {
                    let fields: Vec<_> = line.split('\t').collect();
                    match fields.as_slice() {
                        ["theme", value] => catalog.light_theme = *value == "light",
                        ["animations", value] => catalog.animations = *value != "0",
                        ["language", language] => {
                            catalog.language = match *language {
                                "en" => Language::English,
                                "ja" => Language::Japanese,
                                _ => Language::Chinese,
                            }
                        }
                        ["selected", id] => {
                            catalog.selected = unescape(id).filter(|id| valid_id(id))
                        }
                        ["game", id, cursor, name, ..] => {
                            let Some(id) = unescape(id).filter(|id| valid_id(id)) else {
                                continue;
                            };
                            if catalog.games.iter().any(|g| g.id == id) {
                                continue;
                            }
                            let name = unescape(name)
                                .filter(|s| !s.is_empty())
                                .unwrap_or_else(|| id.clone());
                            let cursor = match *cursor {
                                "slow" => CursorSpeed::Slow,
                                "fast" => CursorSpeed::Fast,
                                _ => CursorSpeed::Normal,
                            };
                            catalog.games.push(Game {
                                directory: root.join(&id),
                                id,
                                name,
                                cursor,
                                script_logs: fields.get(4) == Some(&"1"),
                                engine_logs: fields.get(7) == Some(&"1"),
                                show_stats: fields.get(6) == Some(&"1"),
                                render_quality: match fields.get(8).copied() {
                                    Some("balanced") => RenderQuality::Balanced,
                                    Some("performance") => RenderQuality::Performance,
                                    _ => RenderQuality::Native,
                                },
                                startup: fields
                                    .get(5)
                                    .and_then(|s| unescape(s))
                                    .filter(|s| valid_startup(s))
                                    .unwrap_or_else(|| "startup.tjs".into()),
                            });
                        }
                        _ => {}
                    }
                }
                catalog.sort();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                catalog.refresh()?;
                catalog.save()?;
            }
            Err(e) => return Err(e.to_string()),
        }
        Ok(catalog)
    }
    pub fn refresh(&mut self) -> Result<(), String> {
        let previous: HashMap<_, _> = self.games.iter().map(|g| (g.id.as_str(), g)).collect();
        let mut games = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) {
                continue;
            }
            let directory = entry.path();
            let candidate = fs::read_dir(&directory)
                .map_err(|e| e.to_string())?
                .any(|item| {
                    item.ok().is_some_and(|item| {
                        item.file_type().is_ok_and(|t| t.is_file())
                            && (item
                                .file_name()
                                .to_string_lossy()
                                .eq_ignore_ascii_case("startup.tjs")
                                || item.path().extension().is_some_and(|e| {
                                    e.eq_ignore_ascii_case("xp3") || e.eq_ignore_ascii_case("tjs")
                                }))
                    })
                });
            if candidate || previous.contains_key(id.as_str()) {
                let old = previous.get(id.as_str());
                games.push(Game {
                    name: old.map_or(id.as_str(), |g| g.name.as_str()).into(),
                    cursor: old.map_or(CursorSpeed::Normal, |g| g.cursor),
                    script_logs: old.is_some_and(|g| g.script_logs),
                    engine_logs: old.is_some_and(|g| g.engine_logs),
                    show_stats: old.is_some_and(|g| g.show_stats),
                    render_quality: old.map_or(RenderQuality::Native, |g| g.render_quality),
                    startup: old.map_or_else(|| "startup.tjs".into(), |g| g.startup.clone()),
                    id,
                    directory,
                });
            }
        }
        self.games = games;
        self.sort();
        Ok(())
    }
    fn sort(&mut self) {
        self.games.sort_by_cached_key(|g| g.name.to_lowercase());
    }
    pub fn select(&mut self, index: usize) {
        self.selected = self.games.get(index).map(|g| g.id.clone());
    }
    pub fn selected_index(&self) -> usize {
        self.selected
            .as_ref()
            .and_then(|id| self.games.iter().position(|g| &g.id == id))
            .unwrap_or(0)
    }
    pub fn save(&self) -> Result<(), String> {
        let mut text = format!("language\t{}\n", self.language.code());
        text.push_str(if self.light_theme {
            "theme\tlight\n"
        } else {
            "theme\tdark\n"
        });
        text.push_str(if self.animations {
            "animations\t1\n"
        } else {
            "animations\t0\n"
        });
        if let Some(id) = &self.selected {
            text.push_str(&format!("selected\t{}\n", escape(id)));
        }
        for game in &self.games {
            text.push_str(&format!(
                "game\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                escape(&game.id),
                game.cursor.code(),
                escape(&game.name),
                u8::from(game.script_logs),
                escape(&game.startup),
                u8::from(game.show_stats),
                u8::from(game.engine_logs),
                game.render_quality.code()
            ));
        }
        fs::write(self.root.join(CONFIG), text).map_err(|e| e.to_string())
    }
}
fn valid_id(value: &str) -> bool {
    let mut parts = Path::new(value).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}
pub(crate) fn valid_startup(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 4096
        && !value.starts_with(['/', '\\'])
        && !value.chars().any(|c| c.is_control() || c == ':')
        && !value
            .split(['/', '\\', '>'])
            .any(|p| p.is_empty() || p.chars().all(|c| c == '.'))
}
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}
fn unescape(value: &str) -> Option<String> {
    let mut result = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        result.push(if c == '\\' {
            match chars.next()? {
                '\\' => '\\',
                't' => '\t',
                'n' => '\n',
                'r' => '\r',
                _ => return None,
            }
        } else {
            c
        });
    }
    Some(result)
}

#[cfg(all(test, not(target_os = "vita")))]
#[path = "../../tests/launcher/catalog.rs"]
mod tests;

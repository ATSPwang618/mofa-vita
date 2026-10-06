//! System discovery starts alongside desktop startup, outside the UI/VM loop.
use fontdb::{Database, Family, Query, Source, Style, Weight};
use krkr_protocol::{budget::Budget, text::Font};
use krkr_render::{
    Error, Result,
    font::{Coverage, Face, Provider},
};
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::sync::{Arc, OnceLock};

#[derive(Default)]
pub struct Fonts {
    database: Arc<OnceLock<Database>>,
    coverage: HashMap<fontdb::ID, Option<Coverage>>,
}
impl Fonts {
    /// Discover system metadata alongside game startup, before the first UI
    /// measurement needs it. Only one database is built, with lazy fallback for
    /// callers using Default. The worker never owns VM or window state.
    pub fn discover() -> Result<Self> {
        let fonts = Self::default();
        let database = fonts.database.clone();
        std::thread::Builder::new()
            .name("krkr-font-discovery".into())
            .spawn(move || {
                database.get_or_init(Self::load_database);
            })
            .map_err(|error| Error::Backend(error.to_string()))?;
        Ok(fonts)
    }
    fn load_database() -> Database {
        let mut db = Database::new();
        db.load_system_fonts();

        db
    }
    fn database(&mut self) -> &Database {
        self.database.get_or_init(Self::load_database)
    }
    fn query(db: &Database, font: &Font) -> Option<fontdb::ID> {
        db.query(&Query {
            families: &[Family::Name(&font.face)],
            weight: if font.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            },
            style: if font.italic {
                Style::Italic
            } else {
                Style::Normal
            },
            ..Default::default()
        })
    }
    fn coverage(&mut self, id: fontdb::ID) -> Option<Coverage> {
        if let Some(&coverage) = self.coverage.get(&id) {
            return coverage;
        }
        // Font selection is an infrequent worker operation. Inspect each face
        // at most once and retain only metadata, never another copy of its data.
        let coverage = self
            .database()
            .with_face_data(id, |data, index| {
                let face = ttf_parser::Face::parse(data, index).ok()?;
                Some(Coverage::from_face(&face))
            })
            .flatten();
        self.coverage.insert(id, coverage);
        coverage
    }
}
impl Provider for Fonts {
    fn load(&mut self, font: &Font, budget: &Budget) -> Result<Option<Face>> {
        let db = self.database();
        let Some(id) = Self::query(db, font) else {
            return Ok(None);
        };
        let info = db.face(id).expect("queried font");
        let (data, permit) = match &info.source {
            Source::File(path) => {
                let mut file =
                    std::fs::File::open(path).map_err(|e| Error::Backend(e.to_string()))?;
                let len = usize::try_from(
                    file.metadata()
                        .map_err(|e| Error::Backend(e.to_string()))?
                        .len(),
                )
                .map_err(|_| Error::Message("font file too large"))?;
                let permit = budget.reserve(len)?;
                let mut data = vec![0; len];
                file.read_exact(&mut data)
                    .map_err(|e| Error::Backend(e.to_string()))?;
                (data, permit)
            }
            Source::Binary(data) => {
                let data = data.as_ref().as_ref();
                let permit = budget.reserve(data.len())?;
                (data.to_vec(), permit)
            }
        };
        let mut face = Face::from_bytes(data, info.index, permit)?;
        face.bold = info.weight >= Weight::BOLD;
        face.italic = info.style != Style::Normal;
        Ok(Some(face))
    }
    fn list(&mut self, flags: u32, selected: Option<Coverage>) -> Result<Vec<String>> {
        let ids: Vec<_> = self
            .database()
            .faces()
            .filter(|face| flags & 1 == 0 || face.monospaced)
            .map(|face| face.id)
            .collect();
        let mut list = BTreeSet::new();
        for id in ids {
            if flags & (2 | 8 | 16) != 0 {
                let Some(coverage) = self.coverage(id) else {
                    continue;
                };
                if !coverage.matches(flags, selected) {
                    continue;
                }
            }
            let face = self.database().face(id).expect("enumerated font");
            for (name, _) in &face.families {
                if flags & 4 == 0 || !name.starts_with('@') {
                    list.insert(name.clone());
                }
            }
        }
        Ok(list.into_iter().collect())
    }
}

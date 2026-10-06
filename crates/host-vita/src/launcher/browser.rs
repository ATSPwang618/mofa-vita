//! Browse one game directory; XP3 indexes are opened only on user selection.
use krkr_engine::assets::xp3::Archive;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    Archive,
    File,
}
#[derive(Clone)]
pub struct Entry {
    pub name: String,
    pub location: String,
    pub kind: Kind,
}
#[derive(Clone)]
pub struct Browser {
    root: PathBuf,
    pub directory: String,
    pub entries: Vec<Entry>,
    pub selected: usize,
    archive: Option<(String, Arc<Archive>)>,
}
impl Browser {
    pub fn new(root: &Path) -> Result<Self, String> {
        let mut browser = Self {
            root: root.into(),
            directory: String::new(),
            entries: Vec::new(),
            selected: 0,
            archive: None,
        };
        browser.open("")?;
        Ok(browser)
    }
    fn open(&mut self, directory: &str) -> Result<(), String> {
        let mut entries = Vec::new();
        if let Some((outer, prefix)) = directory.split_once('>') {
            if self.archive.as_ref().is_none_or(|(name, _)| name != outer) {
                self.archive = Some((
                    outer.into(),
                    Arc::new(
                        Archive::load(&self.root.join(outer), crate::memory::storage_limits())
                            .map_err(|e| e.to_string())?,
                    ),
                ));
            }
            let archive = &self.archive.as_ref().unwrap().1;
            let mut names = BTreeMap::new();
            for key in archive.entries.keys() {
                let full = String::from_utf16_lossy(key);
                let Some(relative) = full.strip_prefix(prefix) else {
                    continue;
                };
                if relative.is_empty() {
                    continue;
                }
                let (name, kind) = match relative.split_once('/') {
                    Some((name, _)) => (name, Kind::Folder),
                    None => (relative, Kind::File),
                };
                names.insert(name.to_string(), kind);
            }
            for (name, kind) in names {
                let location = format!(
                    "{directory}{name}{}",
                    if kind == Kind::Folder { "/" } else { "" }
                );
                entries.push(Entry {
                    name,
                    location,
                    kind,
                });
            }
        } else {
            for entry in fs::read_dir(self.root.join(directory)).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                let kind = entry.file_type().map_err(|e| e.to_string())?;
                if kind.is_symlink() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.contains(['>', ':']) {
                    continue;
                }
                let kind = if kind.is_dir() {
                    Kind::Folder
                } else if entry
                    .path()
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("xp3"))
                {
                    Kind::Archive
                } else {
                    Kind::File
                };
                let location = format!(
                    "{directory}{name}{}",
                    match kind {
                        Kind::Folder => "/",
                        Kind::Archive => ">",
                        Kind::File => "",
                    }
                );
                entries.push(Entry {
                    name,
                    location,
                    kind,
                });
            }
            self.archive = None;
        }
        entries.sort_by_cached_key(|e| (e.kind == Kind::File, e.name.to_lowercase()));
        self.entries = entries;
        self.directory = directory.into();
        self.selected = 0;
        Ok(())
    }
    pub fn activate(&mut self) -> Result<Option<String>, String> {
        let Some(entry) = self.entries.get(self.selected) else {
            return Ok(None);
        };
        let location = entry.location.clone();
        if entry.kind == Kind::File {
            return Ok(Some(location));
        }
        self.open(&location)?;
        Ok(None)
    }
    /// False means the user has returned from the game root to settings.
    pub fn back(&mut self) -> Result<bool, String> {
        if self.directory.is_empty() {
            return Ok(false);
        }
        let parent = if let Some((outer, inner)) = self.directory.split_once('>') {
            if inner.is_empty() {
                parent_directory(outer).into()
            } else {
                format!("{outer}>{}", parent_directory(inner.trim_end_matches('/')))
            }
        } else {
            parent_directory(self.directory.trim_end_matches('/')).into()
        };
        self.open(&parent)?;
        Ok(true)
    }
}
fn parent_directory(path: &str) -> &str {
    path.rfind('/').map_or("", |at| &path[..=at])
}

#[cfg(all(test, not(target_os = "vita")))]
mod tests {
    use super::*;
    use krkr_engine::assets::xp3::{Compression, offline::pack_directory};

    #[test]
    fn browse_loose_files_and_nested_archive_members_and_return_to_root() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("scripts")).unwrap();
        fs::write(root.path().join("scripts/boot.tjs"), "System.exit(0);").unwrap();
        fs::create_dir(source.path().join("scripts")).unwrap();
        fs::write(source.path().join("scripts/boot.tjs"), "System.exit(0);").unwrap();
        pack_directory(
            source.path(),
            &root.path().join("data.xp3"),
            Compression::Zlib,
            Default::default(),
        )
        .unwrap();
        // An invalid archive does not prevent browsing other entries.
        fs::write(root.path().join("broken.xp3"), []).unwrap();
        let mut browser = Browser::new(root.path()).unwrap();
        let choose = |b: &mut Browser, name: &str| {
            b.selected = b.entries.iter().position(|e| e.name == name).unwrap();
            b.activate()
        };
        assert!(choose(&mut browser, "broken.xp3").is_err());
        assert!(browser.directory.is_empty());
        assert!(choose(&mut browser, "scripts").unwrap().is_none());
        assert_eq!(
            choose(&mut browser, "boot.tjs").unwrap().as_deref(),
            Some("scripts/boot.tjs")
        );
        assert!(browser.back().unwrap());
        assert!(choose(&mut browser, "data.xp3").unwrap().is_none());
        assert!(choose(&mut browser, "scripts").unwrap().is_none());
        assert_eq!(
            choose(&mut browser, "boot.tjs").unwrap().as_deref(),
            Some("data.xp3>scripts/boot.tjs")
        );
        assert!(browser.back().unwrap());
        assert_eq!(browser.directory, "data.xp3>");
        assert!(browser.back().unwrap());
        assert!(browser.directory.is_empty());
        assert!(!browser.back().unwrap());
    }
}

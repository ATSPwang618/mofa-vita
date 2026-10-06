mod files;
use directories::{BaseDirs, UserDirs};
use krkr_engine::{
    assets::local,
    system::{SystemConfig, SystemHost},
};
use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

/// Process-owned locks. Files are stable rendezvous points and are not deleted
/// on unlock: unlinking a held file would let another process lock a new inode.
pub struct DesktopSystem {
    lock_directory: PathBuf,
    locks: Vec<File>,
}
impl DesktopSystem {
    pub fn new(lock_directory: PathBuf) -> Self {
        Self {
            lock_directory,
            locks: Vec::new(),
        }
    }
}
impl SystemHost for DesktopSystem {
    fn file_attributes(&mut self, path: &[u16]) -> Result<u32, String> {
        files::attributes(path)
    }
    fn change_file_attributes(
        &mut self,
        path: &[u16],
        mask: u32,
        set: bool,
    ) -> Result<bool, String> {
        files::change_attributes(path, mask, set)
    }
    fn file_display_name(&mut self, path: &[u16]) -> Result<Vec<u16>, String> {
        files::display_name(path)
    }
    fn create_app_lock(&mut self, name: &[u16]) -> Result<bool, String> {
        // Hash the exact UTF-16 name, including isolated surrogates. UUID v5
        // keeps the rendezvous name stable across processes and Rust versions.
        let bytes: Vec<_> = name.iter().flat_map(|u| u.to_le_bytes()).collect();
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, &bytes);
        std::fs::create_dir_all(&self.lock_directory).map_err(|e| e.to_string())?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.lock_directory.join(format!("{id}.lock")))
            .map_err(|e| e.to_string())?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => {
                self.locks.push(file);
                Ok(true)
            }
            Err(fs4::TryLockError::WouldBlock) => Ok(false),
            Err(error) => Err(error.to_string()),
        }
    }
}
fn directory(path: &Path) -> Result<Vec<u16>, String> {
    local::directory(path).map_err(|e| e.to_string())
}

/// Capture native directory locations once and install process services. This
/// does not create user directories or acquire any lock until requested.
pub fn configure(config: &mut SystemConfig) -> Result<(), String> {
    let base = BaseDirs::new();
    let user = UserDirs::new();
    if let Some(base) = &base {
        config.app_data_path = directory(base.data_dir())?;
    }
    config.personal_path = match user.as_ref().and_then(UserDirs::document_dir) {
        Some(path) => directory(path)?,
        None => config.app_data_path.clone(),
    };
    #[cfg(windows)]
    if let Some(path) = known_folders::get_known_folder_path(known_folders::KnownFolder::SavedGames)
    {
        // Original savedGamesPath returns the OS path rather than a storage URI.
        config.saved_games_path = local::units(&path).map_err(|e| e.to_string())?;
    }
    config.host = Some(Box::new(DesktopSystem::new(
        std::env::temp_dir().join("krkr-rs-app-locks"),
    )));
    Ok(())
}

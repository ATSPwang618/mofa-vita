//! Cross-platform filesystem attributes. Only representable changes succeed;
//! unsupported Windows flags never turn into fabricated metadata or chmod side effects.
use krkr_engine::assets::local;
#[cfg(windows)]
use krkr_engine::assets::name;
use std::{fs, path::Path};

fn metadata_attributes(metadata: &fs::Metadata) -> u32 {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
    }
    #[cfg(not(windows))]
    {
        let flags =
            u32::from(metadata.permissions().readonly()) | if metadata.is_dir() { 0x10 } else { 0 };
        if flags == 0 { 0x80 } else { flags }
    }
}
pub(super) fn attributes(path: &[u16]) -> Result<u32, String> {
    let path = local::path(path).map_err(|e| e.to_string())?;
    // GetFileAttributes reports INVALID_FILE_ATTRIBUTES for every OS failure.
    Ok(fs::metadata(path)
        .map(|metadata| metadata_attributes(&metadata))
        .unwrap_or(u32::MAX))
}
pub(super) fn change_attributes(path: &[u16], mask: u32, set: bool) -> Result<bool, String> {
    let path = local::path(path).map_err(|e| e.to_string())?;
    let Ok(metadata) = fs::metadata(&path) else {
        return Ok(false);
    };
    let original = metadata_attributes(&metadata);
    let mask = mask & 0x1a7;
    let requested = if set {
        original | mask
    } else {
        original & !mask
    };
    // NORMAL is a synthetic absence of other flags, not an independently
    // mutable attribute. std::fs can change READONLY on all desktop hosts.
    // Hidden/system/archive/temporary have no portable setter: reject an
    // actual change before touching permissions. Already satisfied flags are
    // accepted, as are original masked-out bits such as DIRECTORY.
    if (original ^ requested) & !(1 | 0x80) != 0 {
        return Ok(false);
    }
    let readonly = requested & 1 != 0;
    if metadata.permissions().readonly() == readonly {
        return Ok(true);
    }
    let mut permissions = metadata.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = permissions.mode();
        // Setting readonly removes write bits. Reset grants owner write only;
        // it must not silently grant write permission to group/other users.
        permissions.set_mode(if readonly {
            mode & !0o222
        } else {
            mode | 0o200
        });
    }
    #[cfg(not(unix))]
    permissions.set_readonly(readonly);
    Ok(fs::set_permissions(path, permissions).is_ok())
}
pub(super) fn display_name(path: &[u16]) -> Result<Vec<u16>, String> {
    let path = local::path(path).map_err(|e| e.to_string())?;
    fs::metadata(&path).map_err(|e| e.to_string())?;
    let display = path.file_name().map_or(path.as_path(), Path::new);
    let display = local::units(display).map_err(|e| e.to_string())?;
    // Storage normalization folds ASCII. Preserve real filesystem spelling,
    // without introducing Shell display settings or native platform APIs.
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        for entry in fs::read_dir(parent).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let actual = local::units(Path::new(&entry.file_name())).map_err(|e| e.to_string())?;
            if actual == display {
                return Ok(actual);
            }
            #[cfg(windows)]
            if name::fold(&actual) == name::fold(&display) {
                return Ok(actual);
            }
        }
    }
    Ok(display)
}
